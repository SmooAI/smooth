//! SmoothFlow Diff end to end (th-26f5b9): a fake agent runs two turns that
//! edit a file; the engine's turn snapshots make the `turn` diff exactly the
//! second turn's change, `uncommitted` everything; a hunk reverts over the
//! flow WS (and is then stale), and a review lands in the agent's pane as one
//! steer — refused while the agent waits on an approval.

use std::time::Duration;

use serde_json::{json, Value};

use crate::support::{prereqs, state, Daemon, Host, WAIT};

fn lines(file: &Value) -> Vec<String> {
    file["hunks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|h| h["lines"].as_array().unwrap().clone())
        .map(|l| {
            let p = match l["kind"].as_str().unwrap() {
                "add" => '+',
                "del" => '-',
                _ => ' ',
            };
            format!("{p}{}", l["text"].as_str().unwrap())
        })
        .collect()
}

fn paths(diff: &Value) -> Vec<String> {
    diff["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect()
}

async fn diff_by_turn_revert_stale_and_review(host: Host) {
    if !prereqs(host) {
        return;
    }
    let d = Daemon::boot(host).await;
    // fake-agent writes its own log into the worktree; keep it out of the diff.
    std::fs::write(d.ws.join(".git").join("info").join("exclude"), ".fake-agent*\n").unwrap();
    let mut ws = d.ws().await;

    let s = d.new_session("fake-agent", None).await;
    let id = s["id"].as_str().unwrap().to_string();
    d.wait_state(&id, "idle", WAIT).await;

    // Two turns, each between UserPromptSubmit and Stop.
    let store = d.store();
    for (n, cmd) in [(2, "/edit src/a.txt one"), (4, "/edit src/a.txt two")] {
        d.send(&id, cmd).await;
        let deadline = std::time::Instant::now() + WAIT;
        while store.snapshots(&id).unwrap().len() < n {
            assert!(std::time::Instant::now() < deadline, "turn snapshots never reached {n}:\n{}", d.log());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let snaps = store.snapshots(&id).unwrap();
    let kinds: Vec<&str> = snaps.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(kinds, ["start", "end", "start", "end"], "{snaps:?}");
    assert_eq!(std::fs::read_to_string(d.ws.join("src/a.txt")).unwrap(), "one\ntwo\n");
    d.wait_state(&id, "idle", WAIT).await;

    // `turn`: only the second turn's line, against the first turn's end.
    ws.send(json!({"type":"flow.diff","id":id,"base":"turn","seq":1})).await;
    let f = ws.wait_for("flow.diff turn", WAIT, |v| v["type"] == "flow.diff" && v["base"] == "turn").await;
    let turn = &f["diff"];
    assert_eq!(paths(turn), ["src/a.txt"], "{turn}");
    assert_eq!(lines(&turn["files"][0]), [" one", "+two"], "{turn}");
    assert_eq!(turn["turn"]["live"], Value::Null, "an idle agent's turn is finished (live omitted)");
    assert_eq!(turn["from"]["label"], "turn start");
    let hunk_id = turn["files"][0]["hunks"][0]["id"].as_str().unwrap().to_string();

    // `uncommitted`: the whole new file, untracked as it is.
    ws.send(json!({"type":"flow.diff","id":id,"base":"uncommitted"})).await;
    let f = ws
        .wait_for("flow.diff uncommitted", WAIT, |v| v["type"] == "flow.diff" && v["base"] == "uncommitted")
        .await;
    let unc = &f["diff"];
    assert_eq!(paths(unc), ["src/a.txt"], "{unc}");
    assert_eq!(unc["files"][0]["status"], "added");
    assert_eq!(lines(&unc["files"][0]), ["+one", "+two"]);
    assert_eq!((unc["added"].as_u64(), unc["deleted"].as_u64()), (Some(2), Some(0)));

    // The HTTP twin says the same.
    let (status, http) = d.get(&format!("/api/flow/sessions/{id}/diff?base=turn")).await;
    assert_eq!(status, 200, "{http}");
    assert_eq!(lines(&http["files"][0]), [" one", "+two"]);

    // Revert the turn's hunk: the worktree loses `two`, a broadcast says so.
    ws.send(json!({"type":"flow.diff.revert","id":id,"base":"turn","hunk_id":hunk_id,"seq":2}))
        .await;
    let r = ws.wait_for("flow.diff.result", WAIT, |v| v["type"] == "flow.diff.result").await;
    assert_eq!(r["action"], "revert");
    assert_eq!(r["file"], "src/a.txt");
    ws.wait_for("flow.diff.changed", WAIT, |v| v["type"] == "flow.diff.changed" && v["id"] == id)
        .await;
    assert_eq!(std::fs::read_to_string(d.ws.join("src/a.txt")).unwrap(), "one\n");

    // Again: the hunk no longer applies — refused, nothing written.
    ws.send(json!({"type":"flow.diff.revert","id":id,"base":"turn","hunk_id":hunk_id,"seq":3}))
        .await;
    let e = ws.wait_for("stale error", WAIT, |v| v["type"] == "flow.error" && v["ref"] == 3).await;
    assert_eq!(e["code"], "stale", "{e}");
    assert_eq!(std::fs::read_to_string(d.ws.join("src/a.txt")).unwrap(), "one\n");

    // A review: one steer the agent sees.
    let review = json!({"type":"flow.diff.review","id":id,"base":"uncommitted","seq":4,"comments":[
        {"file":"src/a.txt","line_range":[1,1],"text":"rename this"}
    ]});
    ws.send(review.clone()).await;
    let r = ws
        .wait_for("review result", WAIT, |v| v["type"] == "flow.diff.result" && v["action"] == "review")
        .await;
    let message = r["message"].as_str().unwrap();
    assert!(message.contains("1. src/a.txt:1") && message.contains("rename this"), "{message}");
    d.wait_screen(&id, "rename this", WAIT).await;

    // Blocked on an approval: the review is refused, with the reason.
    d.send(&id, "/perm").await;
    d.wait_until(&id, "needs_you", WAIT, |s| state(s) == "needs_you").await;
    let mut blocked = review;
    blocked["seq"] = json!(5);
    ws.send(blocked).await;
    let e = ws.wait_for("blocked error", WAIT, |v| v["type"] == "flow.error" && v["ref"] == 5).await;
    assert_eq!(e["code"], "blocked", "{e}");
}

crate::on_both_hosts!(diff_by_turn_revert_stale_and_review);

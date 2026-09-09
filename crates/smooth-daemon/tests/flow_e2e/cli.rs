//! `th flow` and `th harness` against the live daemon — the `--json`
//! contracts scripts depend on, and the human lines a person reads.

use serde_json::json;

use crate::support::{prereqs_with_th, state, Daemon, WAIT};

#[tokio::test]
async fn th_flow_json_against_the_live_daemon() {
    if !prereqs_with_th() {
        return;
    }
    let d = Daemon::boot().await;
    let ws = d.ws.to_string_lossy().into_owned();

    // ls on an empty engine: a confirmed empty read, in both renderings.
    assert_eq!(d.th_json(&["flow", "ls", "--json"]), json!({"sessions": []}));
    let (code, out, _) = d.th(&["flow", "ls"]);
    assert_eq!(code, 0);
    assert!(out.contains("No flow sessions"), "{out}");
    assert_eq!(d.th_json(&["flow", "inbox", "--json"]), json!({"sessions": []}));

    // new --json → the row; --kind is any manifest name.
    let v = d.th_json(&[
        "flow",
        "new",
        "--kind",
        "fake-agent",
        "--worktree",
        &ws,
        "--prompt",
        "/work c1",
        "--title",
        "cli one",
        "--json",
    ]);
    let id = v["session"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["session"]["kind"], "fake-agent");
    assert_eq!(v["session"]["title"], "cli one");
    d.wait_until(&id, "unread idle via hooks", WAIT, |s| {
        state(s) == "idle" && s["state_source"] == "hooks" && s["unread"] == true
    })
    .await;

    // ls --json carries the engine's row verbatim; the table shows the id,
    // the state glyph text and the VIA column.
    let ls = d.th_json(&["flow", "ls", "--json"]);
    assert_eq!(ls["sessions"][0]["id"], id);
    assert_eq!(ls["sessions"][0]["state"], "idle");
    assert_eq!(ls["sessions"][0]["state_source"], "hooks");
    let (_, table, _) = d.th(&["flow", "ls"]);
    assert!(
        table.contains(&id) && table.contains("idle") && table.contains("hooks") && table.contains("cli one"),
        "{table}"
    );

    // snapshot --json is the flow.screen frame; the human form is the text.
    let snap = d.th_json(&["flow", "snapshot", &id, "--json"]);
    assert_eq!(snap["type"], "flow.screen");
    assert!(snap["text"].as_str().unwrap().contains("worked: c1"), "{snap}");
    let (_, text, _) = d.th(&["flow", "snapshot", &id]);
    assert!(text.contains("worked: c1"), "{text}");

    // send → inbox shows the permission; approve --json answers it.
    let (code, out, err) = d.th(&["flow", "send", &id, "/perm"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(&format!("sent to {id}")), "{out}");
    let ask = d.wait_state(&id, "needs_you", WAIT).await;
    let request_id = ask["attention"]["request_id"].as_str().unwrap().to_string();
    let inbox = d.th_json(&["flow", "inbox", "--json"]);
    assert_eq!(inbox["sessions"][0]["id"], id);
    assert_eq!(inbox["sessions"][0]["attention"]["request_id"], request_id);
    let (_, inbox_text, _) = d.th(&["flow", "inbox"]);
    assert!(inbox_text.contains("needs you") && inbox_text.contains("[permission]"), "{inbox_text}");
    // approve without --request picks the session's current request.
    let approved = d.th_json(&["flow", "approve", &id, "--decision", "allow", "--json"]);
    assert_eq!(approved["session"]["state"], "working", "{approved}");
    d.wait_screen(&id, r#""behavior":"allow""#, WAIT).await;
    let (code, _, err) = d.th(&["flow", "approve", &id, "--decision", "maybe"]);
    assert_ne!(code, 0, "an unknown decision is refused before any request");
    assert!(err.contains("allow | deny | allow_session"), "{err}");
    let (code, _, err) = d.th(&["flow", "approve", "fs-nope", "--decision", "allow"]);
    assert_ne!(code, 0);
    assert!(err.contains("no pending permission request") || err.contains("th flow ls"), "{err}");

    // handoff: git facts from the engine (no pearl → null pearl).
    let (code, out, err) = d.th(&["flow", "handoff", &id]);
    assert_eq!(code, 0, "{err}");
    let handoff: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(handoff["handoff"]["worktree"], ws);
    assert_eq!(handoff["handoff"]["branch"], "main");
    assert!(handoff["handoff"]["head"].as_str().unwrap().len() >= 7, "{handoff}");
    assert!(handoff["pearl"].is_null() && handoff["checkpoints"].is_array(), "{handoff}");

    // kill --json → done; the two-line error contract for an unknown id.
    let killed = d.th_json(&["flow", "kill", &id, "--json"]);
    assert_eq!(killed["session"]["state"], "done", "{killed}");
    let (code, _, err) = d.th(&["flow", "kill", "fs-nope"]);
    assert_ne!(code, 0);
    assert!(err.contains("no such session") && err.contains("→ check the id with `th flow ls`"), "{err}");

    // new with explicit argv after `--` runs that, in the worktree.
    let v = d.th_json(&[
        "flow",
        "new",
        "--kind",
        "shell",
        "--worktree",
        &ws,
        "--json",
        "--",
        "sh",
        "-c",
        "echo ARGV-OK; exec sh -l",
    ]);
    let sh = v["session"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["session"]["argv"], json!(["sh", "-c", "echo ARGV-OK; exec sh -l"]));
    d.wait_screen(&sh, "ARGV-OK", WAIT).await;
    d.th_json(&["flow", "kill", &sh, "--json"]);
}

#[tokio::test]
async fn th_without_a_daemon_says_so_in_two_lines() {
    if !prereqs_with_th() {
        return;
    }
    // A daemon whose HOME has no daemon.addr yet: point `th` at a HOME of
    // its own by booting a rig and deleting the advertisement.
    let d = Daemon::boot().await;
    std::fs::remove_file(d.home.join(".smooth").join("daemon.addr")).unwrap();
    let (code, _, err) = d.th(&["flow", "ls"]);
    assert_ne!(code, 0);
    assert!(err.contains("no daemon advertised") && err.contains("th up"), "{err}");
    // `th harness list` degrades to the on-disk registry with a note.
    let (code, out, _) = d.th(&["harness", "list"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("daemon not reachable") && out.contains("fake-agent"), "{out}");
    let v = d.th_json(&["harness", "list", "--json"]);
    assert_eq!(v["source"], "local");
}

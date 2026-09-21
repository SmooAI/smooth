//! Shell sessions over the flow WS: new → attach → input echo → resize →
//! snapshot → kill, plus the two ways a shell ends on its own (exit 0 is
//! `done`, a non-zero exit is `dead` — rule 5: the PTY's own report).

use std::time::Duration;

use serde_json::json;

use crate::support::{prereqs, state, unb64, Daemon, TICK, WAIT};

#[tokio::test]
async fn shell_lifecycle_new_attach_input_resize_snapshot_kill() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let mut ws = d.ws().await;
    assert_eq!(ws.hello["sessions"], json!([]), "a fresh daemon has no sessions");
    assert!(
        ws.hello["harnesses"].as_array().unwrap().iter().any(|h| h["name"] == "fake-agent"),
        "{}",
        ws.hello
    );

    // new — the direct reply is the row; a shell is `idle` at once.
    ws.send(json!({"type":"flow.new","kind":"shell","worktree":d.ws,"title":"e2e shell"})).await;
    let created = ws
        .wait_for("flow.session for the new shell", WAIT, |v| {
            v["type"] == "flow.session" && v["session"]["kind"] == "shell"
        })
        .await;
    let id = created["session"]["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("fs-"), "{created}");
    let s = d.wait_state(&id, "idle", WAIT).await;
    assert_eq!(s["title"], "e2e shell");
    assert_eq!(s["worktree"], d.ws.to_string_lossy().as_ref());
    assert_eq!(s["branch"], "main");
    assert_eq!(s["state_source"], "inferred");
    assert!(s["pid"].as_u64().is_some(), "{s}");
    assert_eq!(s["tmux_socket"], d.socket, "the row records the private tmux server");

    // attach → the PTY bridge streams output only to attached clients. The
    // first frame (the redrawn pane) proves the bridge is up before typing.
    ws.attach(&id, 100, 30).await;
    ws.wait_for("first flow.output", WAIT, |v| v["type"] == "flow.output" && v["id"] == id).await;
    let marker = format!("FLOW-E2E-{}", std::process::id());
    ws.input(&id, &format!("echo {marker}\r")).await;
    let mut out = ws.wait_output(&id, &marker, WAIT).await;
    assert!(out.contains(&marker), "{out}");
    // …twice: the typed command echo and the command's output.
    let deadline = std::time::Instant::now() + WAIT;
    while out.matches(&marker).count() < 2 && std::time::Instant::now() < deadline {
        if let Some(f) = ws.next(Duration::from_secs(2)).await {
            if f["type"] == "flow.output" && f["id"] == id {
                out.push_str(&unb64(f["data_b64"].as_str().unwrap_or("")));
            }
        }
    }
    assert!(out.matches(&marker).count() >= 2, "echo + output both stream: {out}");

    // snapshot — the plain-text pane, sized as attached.
    let snap = d.snapshot(&id).await;
    assert_eq!(snap["type"], "flow.screen");
    assert!(snap["text"].as_str().unwrap().contains(&marker), "{snap}");
    assert_eq!(
        (snap["cols"].as_u64(), snap["rows"].as_u64()),
        (Some(100), Some(30)),
        "attach sized the pane: {snap}"
    );

    // resize → the pane follows the client.
    ws.send(json!({"type":"flow.resize","id":id,"cols":90,"rows":28})).await;
    let start = std::time::Instant::now();
    loop {
        let snap = d.snapshot(&id).await;
        if snap["cols"] == 90 && snap["rows"] == 28 {
            break;
        }
        assert!(start.elapsed() < WAIT, "pane never resized to 90x28: {snap}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // A second client attaching sees the same bytes; detaching one keeps the
    // other streaming.
    let mut ws2 = d.ws().await;
    ws2.attach(&id, 90, 28).await;
    ws.send(json!({"type":"flow.detach","id":id})).await;
    ws2.input(&id, "echo SECOND-CLIENT\r").await;
    ws2.wait_output(&id, "SECOND-CLIENT", WAIT).await;

    // kill → done, exit code recorded, the tmux session gone.
    ws2.send(json!({"type":"flow.kill","id":id})).await;
    let killed = ws2
        .wait_for("flow.session done", WAIT, |v| {
            v["type"] == "flow.session" && v["session"]["id"] == id && state(&v["session"]) == "done"
        })
        .await;
    assert!(killed["session"]["ended_at"].is_string(), "{killed}");
    let alive = std::process::Command::new("tmux")
        .args(["-L", &d.socket, "has-session", "-t", &id])
        .output()
        .unwrap();
    assert!(!alive.status.success(), "tmux session should be gone after kill");
    // Input to a dead session is an error object, not a hang.
    ws2.send(json!({"type":"flow.input","id":id,"data_b64":"aGk=","seq":42})).await;
    let err = ws2.wait_for("flow.error", WAIT, |v| v["type"] == "flow.error").await;
    assert_eq!(err["ref"], 42);
    assert_eq!(err["code"], "failed", "{err}");
    // A terminal row can be removed; the broadcast tells every client.
    let (status, _) = d.post(&format!("/api/flow/sessions/{id}/kill"), json!({})).await;
    assert_eq!(status, 200, "killing a done session is idempotent");
}

#[tokio::test]
async fn shell_that_exits_is_done_and_a_failing_command_is_dead() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let mut ws = d.ws().await;

    // exit 0 by itself → done with exit_code 0 (rule 5: the PTY reported it).
    let s = d.new_session("shell", None).await;
    let id = s["id"].as_str().unwrap().to_string();
    d.wait_state(&id, "idle", WAIT).await;
    ws.attach(&id, 80, 24).await;
    ws.wait_for("first flow.output", WAIT, |v| v["type"] == "flow.output" && v["id"] == id).await;
    // `exit 0`, explicitly: a bare `exit` in a fresh login sh returns the
    // status of the last profile test, which is 1 on macOS.
    ws.input(&id, "exit 0\r").await;
    let done = d.wait_state(&id, "done", WAIT + TICK).await;
    assert_eq!(done["exit_code"], 0, "{done}");
    assert!(done["attention"].is_null(), "a clean exit needs nobody: {done}");
    let ev = ws
        .wait_for("flow.session done broadcast", WAIT, |v| {
            v["type"] == "flow.session" && v["session"]["id"] == id && state(&v["session"]) == "done"
        })
        .await;
    assert_eq!(ev["session"]["exit_code"], 0);

    // A non-zero exit → dead, attention `crashed` with the code; shells are
    // never resumed (rule 2 is for agents).
    let (status, v) = d
        .post(
            "/api/flow/sessions",
            json!({"kind":"shell","worktree":d.ws,"argv":["sh","-c","echo boom; exit 7"]}),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    let id2 = v["session"]["id"].as_str().unwrap().to_string();
    let dead = d.wait_state(&id2, "dead", WAIT + TICK).await;
    assert_eq!(dead["exit_code"], 7, "{dead}");
    assert_eq!(dead["attention"]["reason"], "crashed");
    assert!(dead["attention"]["detail"].as_str().unwrap().contains("exit 7"), "{dead}");
    // The list is newest-first and carries both.
    let ids: Vec<String> = d.sessions().await.iter().map(|s| s["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids, vec![id2.clone(), id.clone()]);
    // Nothing streamed for a session nobody attached to.
    let frames = ws.collect(Duration::from_secs(1)).await;
    assert!(!frames.iter().any(|f| f["type"] == "flow.output" && f["id"] == id2), "{frames:?}");
}

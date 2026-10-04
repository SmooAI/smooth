//! The pty host's reason to exist (ADR-011, th-dc9822): sessions survive the
//! daemon. Each test SIGKILLs the daemon mid-session and boots a new one on
//! the same HOME, which adopts the running session hosts — the screen, the
//! input path and the exit status all carry over. Also the replay contract
//! end to end: `flow.replay` on attach, and a relaunch's `seq` continuing.

use std::time::Duration;

use serde_json::json;

use crate::support::{state, unb64, Daemon, Host, WAIT};

#[tokio::test]
async fn a_running_session_survives_a_daemon_crash_and_is_adopted() {
    if !crate::support::prereqs(Host::Pty) {
        return;
    }
    let mut d = Daemon::boot(Host::Pty).await;
    let s = d.new_session("shell", None).await;
    let id = s["id"].as_str().unwrap().to_string();
    d.wait_state(&id, "idle", WAIT).await;
    let pid = s["pid"].as_u64().unwrap() as u32;
    let host_pid = d.host_pid(&id).expect("host record");
    {
        let mut ws = d.ws().await;
        ws.attach(&id, 100, 30).await;
        ws.wait_for("first flow.output", WAIT, |v| v["type"] == "flow.output" && v["id"] == id).await;
        ws.input(&id, "echo BEFORE-$((6*7))\r").await;
        ws.wait_output(&id, "BEFORE-42", WAIT).await;
    }

    d.restart().await;
    assert!(Daemon::pid_alive(pid) && Daemon::pid_alive(host_pid), "the session outlived its daemon");
    assert!(d.log().contains("adopted a running session host"), "{}", d.log());
    let row = d.session(&id).await;
    assert_eq!(state(&row), "idle", "{row}");
    assert_eq!(row["host"], "pty");

    // A replay-capable client gets the screen as `flow.replay`…
    let mut ws = d.ws().await;
    ws.send(json!({"type":"flow.attach","id":id,"cols":100,"rows":30,"replay":true})).await;
    let replay = ws.wait_for("flow.replay", WAIT, |v| v["type"] == "flow.replay" && v["id"] == id).await;
    assert_eq!(replay["reason"], "attach");
    assert_eq!((replay["cols"].as_u64(), replay["rows"].as_u64()), (Some(100), Some(30)), "{replay}");
    assert!(
        unb64(replay["data_b64"].as_str().unwrap()).contains("BEFORE-42"),
        "the snapshot holds the old screen"
    );
    let seq = replay["seq"].as_u64().unwrap();
    assert!(seq > 0, "seq carried over from the host: {replay}");
    // …and a legacy client as one reset-prefixed output.
    let mut old = d.ws().await;
    old.attach(&id, 100, 30).await;
    let first = old.wait_for("first flow.output", WAIT, |v| v["type"] == "flow.output" && v["id"] == id).await;
    let bytes = unb64(first["data_b64"].as_str().unwrap());
    assert!(bytes.starts_with("\x1bc\x1b[3J") && bytes.contains("BEFORE-42"), "{bytes:?}");

    // Input still reaches the same shell, and output is newer than the replay.
    ws.input(&id, "echo AFTER-$((6*7))\r").await;
    let out = ws
        .wait_for("output after the replay", WAIT, |v| {
            v["type"] == "flow.output" && v["id"] == id && unb64(v["data_b64"].as_str().unwrap_or("")).contains("AFTER")
        })
        .await;
    assert!(out["seq"].as_u64().unwrap() > seq, "{out}");
    old.wait_output(&id, "AFTER-42", WAIT).await;
    assert_eq!(d.session(&id).await["pid"].as_u64(), Some(u64::from(pid)), "the same process, adopted");

    d.kill(&id, false).await;
    d.wait_session_gone(&id, WAIT).await;
    assert!(!Daemon::pid_alive(host_pid), "kill releases the host");
}

#[tokio::test]
async fn an_exit_while_the_daemon_is_down_is_settled_exactly() {
    if !crate::support::prereqs(Host::Pty) {
        return;
    }
    let mut d = Daemon::boot(Host::Pty).await;
    let (status, v) = d
        .post(
            "/api/flow/sessions",
            json!({"kind":"shell","worktree":d.ws,"argv":["sh","-c","while [ ! -f go ]; do sleep 0.1; done; exit 5"]}),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    let id = v["session"]["id"].as_str().unwrap().to_string();
    d.wait_state(&id, "idle", WAIT).await;
    let host_pid = d.host_pid(&id).expect("host record");

    // Crash the daemon, then let the child exit while nobody is connected:
    // the host records the status before anyone could be told.
    d.crash();
    std::fs::write(d.ws.join("go"), b"").unwrap();
    let start = std::time::Instant::now();
    loop {
        let rec = d.host_record(&id).expect("the host lingers with its record");
        if !rec["exit"].is_null() {
            assert_eq!(rec["exit"]["code"], 5, "{rec}");
            break;
        }
        assert!(start.elapsed() < WAIT, "the child never exited: {rec}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    d.boot_again().await;
    let dead = d.wait_state(&id, "dead", WAIT).await;
    assert_eq!(dead["exit_code"], 5, "the host's recorded status, exact: {dead}");
    assert!(dead["attention"]["detail"].as_str().unwrap().contains("exit 5"), "{dead}");
    d.wait_session_gone(&id, WAIT).await;
    let start = std::time::Instant::now();
    while Daemon::pid_alive(host_pid) && start.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!Daemon::pid_alive(host_pid), "the settled host was released");
}

/// Kill & Resume moves the session to a new host: attached replay clients
/// get `flow.replay{reason:"host"}`, and the new host's `seq` continues the
/// old one's.
#[tokio::test]
async fn a_relaunch_continues_the_sessions_seq_and_replays_to_attached_clients() {
    if !crate::support::prereqs(Host::Pty) {
        return;
    }
    let d = Daemon::boot(Host::Pty).await;
    let s = d.new_session("fake-agent", Some("/work one")).await;
    let id = s["id"].as_str().unwrap().to_string();
    d.wait_until(&id, "idle", WAIT, |s| state(s) == "idle").await;
    let mut ws = d.ws().await;
    ws.send(json!({"type":"flow.attach","id":id,"cols":100,"rows":30,"replay":true})).await;
    let first = ws.wait_for("flow.replay", WAIT, |v| v["type"] == "flow.replay" && v["id"] == id).await;
    let before = first["seq"].as_u64().unwrap();
    let old_pid = d.host_pid(&id).expect("host record");

    let resumed = d.kill(&id, true).await;
    assert_eq!(resumed["host"], "pty", "a pty row relaunches on pty: {resumed}");
    let replay = ws
        .wait_for("flow.replay host", WAIT, |v| {
            v["type"] == "flow.replay" && v["id"] == id && v["reason"] == "host"
        })
        .await;
    assert!(replay["seq"].as_u64().unwrap() > before, "seq continues across hosts: {first} → {replay}");
    let new_pid = d.host_pid(&id).expect("a new host record");
    assert_ne!(new_pid, old_pid, "a new host process");
    assert!(!Daemon::pid_alive(old_pid), "the old host was released");
    // The relaunch streams to the client that was already attached.
    ws.wait_for("output from the resumed agent", WAIT, |v| {
        v["type"] == "flow.output" && v["id"] == id && unb64(v["data_b64"].as_str().unwrap_or("")).contains("resume=1")
    })
    .await;
    d.kill(&id, false).await;
}

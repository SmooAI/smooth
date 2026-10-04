//! th-9f6814: a daemon launched from Finder (Big Smooth.app, the SmoothFlow
//! app) runs with `PATH=/usr/bin:/bin:/usr/sbin:/sbin`. Homebrew's tmux is in
//! `/opt/homebrew/bin`, so every session it created sat in `starting`
//! forever — no pid, no detail, nothing logged. These boot the daemon in
//! exactly that environment. On the pty host (th-dc9822) the no-tmux case
//! is the opposite proof: sessions run without tmux at all.

use std::path::Path;

use serde_json::json;

use crate::support::{prereqs, state, Daemon, Host, WAIT};

/// What launchd hands an app started from Finder.
const FINDER_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

async fn a_finder_launched_daemon_finds_tmux_off_path_and_panes_get_a_usable_path(host: Host) {
    if !prereqs(host) {
        return;
    }
    let d = Daemon::boot_with(host, Some(FINDER_PATH), &[]).await;
    let log = d.log();
    assert!(log.contains("flow: tmux resolved"), "the daemon names its tmux at boot:\n{log}");

    // The session comes up: a pid, a live state, a snapshot.
    let s = d.new_session("shell", None).await;
    let id = s["id"].as_str().unwrap().to_string();
    let live = d.wait_state(&id, "idle", WAIT).await;
    assert!(live["pid"].as_u64().is_some(), "{live}");
    assert!(live["attention"].is_null(), "{live}");

    // The pane's PATH is more than Finder's: tmux's own directory is on it
    // (whatever the machine — /opt/homebrew/bin on a Mac with Homebrew).
    let tmux_dir = ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin", "/usr/bin"]
        .into_iter()
        .find(|dir| Path::new(dir).join("tmux").is_file())
        // The pty host needs no tmux; the system dirs are on every pane PATH.
        .unwrap_or("/usr/bin");
    let mut ws = d.ws().await;
    ws.attach(&id, 200, 40).await;
    ws.wait_for("first flow.output", WAIT, |v| v["type"] == "flow.output" && v["id"] == id).await;
    // Ask the shell itself, so a long PATH wrapping on screen can't matter.
    // The marker is split by `""` in the typed line, so only output matches.
    ws.input(
        &id,
        &format!("case \":$PATH:\" in *\":{tmux_dir}:\"*) echo HAS\"\"DIROK;; *) echo NO\"\"DIROK;; esac\r"),
    )
    .await;
    let start = std::time::Instant::now();
    let (answer, screen) = loop {
        let screen = d.screen(&id).await;
        if let Some(l) = screen.lines().map(str::trim).find(|l| *l == "HASDIROK" || *l == "NODIROK") {
            break (l.to_string(), screen);
        }
        assert!(start.elapsed() < WAIT, "the shell never answered:\n{screen}");
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };
    assert_eq!(answer, "HASDIROK", "pane PATH lacks {tmux_dir}:\n{screen}");

    d.kill(&id, false).await;
}

async fn with_no_tmux_a_launch_fails_dead_with_the_reason_not_stuck_starting(host: Host) {
    if !prereqs(host) {
        return;
    }
    let d = Daemon::boot_with(host, Some(FINDER_PATH), &[("SMOOTH_TMUX_BIN", "/nonexistent/th-9f6814/tmux")]).await;
    assert!(d.log().contains("flow: no tmux"), "the daemon warns at boot:\n{}", d.log());
    let mut ws = d.ws().await;
    if host == Host::Pty {
        // ADR-011: the engine-owned PTY host needs no tmux at all.
        let s = d.new_session("shell", None).await;
        let id = s["id"].as_str().unwrap().to_string();
        let live = d.wait_state(&id, "idle", WAIT).await;
        assert!(live["pid"].as_u64().is_some() && live["attention"].is_null(), "{live}");
        assert_eq!(live["host"], "pty");
        d.kill(&id, false).await;
        return;
    }

    let (status, v) = d.post("/api/flow/sessions", json!({ "kind": "shell", "worktree": d.ws })).await;
    assert_ne!(status, 200, "the creator hears the failure: {v}");
    assert!(v.to_string().contains("tmux not found"), "{v}");

    // Every client hears it too: the row goes dead with the reason.
    let frame = ws
        .wait_for("flow.session dead", WAIT, |f| f["type"] == "flow.session" && state(&f["session"]) == "dead")
        .await;
    let row = &frame["session"];
    assert_eq!(row["attention"]["reason"], "launch_failed", "{row}");
    let detail = row["attention"]["detail"].as_str().unwrap();
    assert!(
        detail.contains("tmux not found") && detail.contains("SMOOTH_TMUX_BIN") && detail.contains("/nonexistent/th-9f6814/tmux"),
        "{detail}"
    );

    // …and so does anyone who looks later. Nothing is left `starting`.
    let sessions = d.sessions().await;
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(state(&sessions[0]), "dead", "{}", sessions[0]);
    assert!(sessions[0]["pid"].is_null());
    let log = d.log();
    assert!(log.contains("session launch failed") && log.contains("WARN"), "logged at WARN:\n{log}");
}

crate::on_both_hosts!(
    a_finder_launched_daemon_finds_tmux_off_path_and_panes_get_a_usable_path,
    with_no_tmux_a_launch_fails_dead_with_the_reason_not_stuck_starting
);

//! th-9f6814: a daemon launched from Finder (Big Smooth.app, the SmoothFlow
//! app) runs with `PATH=/usr/bin:/bin:/usr/sbin:/sbin`. Homebrew's tmux is in
//! `/opt/homebrew/bin`, so every session it created sat in `starting`
//! forever — no pid, no detail, nothing logged. These boot the daemon in
//! exactly that environment.

use std::path::Path;

use serde_json::json;

use crate::support::{prereqs, state, Daemon, WAIT};

/// What launchd hands an app started from Finder.
const FINDER_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

#[tokio::test]
async fn a_finder_launched_daemon_finds_tmux_off_path_and_panes_get_a_usable_path() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot_with(Some(FINDER_PATH), &[]).await;
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
        .expect("prereqs found a tmux");
    let mut ws = d.ws().await;
    ws.attach(&id, 200, 40).await;
    ws.wait_for("first flow.output", WAIT, |v| v["type"] == "flow.output" && v["id"] == id).await;
    ws.input(&id, "printf 'PANE-%s-PATH\\n' \"$PATH\"\r").await;
    let screen = d.wait_screen(&id, "PANE-/", WAIT).await;
    let line = screen
        .lines()
        .find(|l| l.starts_with("PANE-/"))
        .unwrap_or_else(|| panic!("no PATH line:\n{screen}"));
    let path = line.trim_start_matches("PANE-").trim_end().trim_end_matches("-PATH");
    assert!(path.split(':').any(|d| d == tmux_dir), "pane PATH `{path}` lacks {tmux_dir}");
    assert!(path != FINDER_PATH, "the pane got Finder's bare PATH");

    d.kill(&id, false).await;
}

#[tokio::test]
async fn with_no_tmux_a_launch_fails_dead_with_the_reason_not_stuck_starting() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot_with(Some(FINDER_PATH), &[("SMOOTH_TMUX_BIN", "/nonexistent/th-9f6814/tmux")]).await;
    assert!(d.log().contains("flow: no tmux"), "the daemon warns at boot:\n{}", d.log());
    let mut ws = d.ws().await;

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

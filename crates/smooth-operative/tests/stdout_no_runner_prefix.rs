//! Pearl th-7b95ef regression test: the operative's stdout MUST
//! contain only JSON `AgentEvent` lines. Any `[runner]` substring on
//! stdout is a contract violation — bigsmooth treats stdout as the
//! chat-token stream, and any non-JSON or `[runner]`-prefixed text
//! used to leak into the session as fake `role: assistant` content
//! (every session opened with multiple-KB blobs of policy/role/history
//! diagnostics duplicated five+ times).
//!
//! Strategy: launch the runner binary with NO `RUNNER_CONFIG` env var
//! set. The binary exits with code 2 after emitting a single
//! `AgentEvent::Error` JSON line on stdout. Every diagnostic the
//! runner produces along the way (tracing init, policy resolution if
//! any) goes to stderr per the design. We then assert:
//!
//! 1. Stdout contains only valid JSON-lines.
//! 2. Stdout contains zero `[runner]` substrings.
//!
//! This catches both classes of regressions:
//!
//!   * A bare `println!("[runner] foo")` anywhere in the runner.
//!   * An `emit_event(AgentEvent::TokenDelta { content: "[runner]
//!     loaded policy ..." })` that wraps diagnostic text as a fake
//!     chat token (the original bug).

use std::process::{Command, Stdio};

/// Locate the freshly-built `smooth-operative` binary. Cargo
/// sets `CARGO_BIN_EXE_<name>` for integration tests to point at the
/// in-workspace binary, so we don't have to guess at target paths.
fn runner_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_smooth-operative"))
}

#[test]
fn stdout_contains_only_json_events_and_no_runner_prefix() {
    // Invoke with no env config — the binary should fail config parsing
    // and emit a single `AgentEvent::Error` JSON line before exiting.
    // `env_clear` strips inherited cargo/test env (notably any stray
    // SMOOTH_* vars on the dev machine) so the test is deterministic.
    let output = Command::new(runner_binary())
        .env_clear()
        // Force tracing to be loud so we'd notice if tracing went to
        // stdout by accident (it shouldn't — runner pins the writer to
        // stderr — but the test deliberately probes that invariant).
        .env("RUST_LOG", "trace")
        // Keep PATH so the dynamic linker can find libs on macOS.
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to spawn smooth-operative");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Sanity: the runner should have exited (config missing is fatal).
    assert!(
        !output.status.success(),
        "expected non-zero exit when RUNNER_CONFIG is missing — stdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // The load-bearing assertion: no `[runner]` substring on stdout.
    // If this fails, some new code is writing diagnostics to stdout
    // (either via println!/print! or by wrapping text in
    // AgentEvent::TokenDelta) — see pearl th-7b95ef for the original
    // bug pattern.
    assert!(
        !stdout.contains("[runner]"),
        "runner stdout contains `[runner]` substring — diagnostic chatter is leaking onto the JSON event stream and will be persisted as fake assistant chat content. stdout:\n{stdout}"
    );

    // Every non-empty stdout line must parse as JSON (the AgentEvent
    // serialization). A non-JSON line would be silently dropped by
    // bigsmooth's parser (also a pearl th-7b95ef defense), but is
    // still a contract violation here.
    for (i, line) in stdout.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(trimmed);
        assert!(parsed.is_ok(), "runner stdout line {i} is not valid JSON: {trimmed:?}\nfull stdout:\n{stdout}");
    }
}

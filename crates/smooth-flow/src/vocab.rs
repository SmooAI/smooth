//! The `flow_*` tool vocabulary (th-8b3918).
//!
//! Shared by every agent-facing surface over the engine: the MCP tools in `smooth-cli` (`th mcp serve`,
//! over the daemon's HTTP API) and Big Smooth's in-process operator tools in
//! `smooth-daemon` (engine-direct). One set of rules means a model sees the
//! same answers and the same refusals whichever door it came through.
//!
//! Everything here is pure: session rows arrive as their wire JSON (the shape
//! `GET /api/flow/sessions` returns, i.e. `serde_json::to_value(Session)`).

use std::fmt::Write as _;
use std::time::Duration;

use serde_json::Value;

/// Where a submitted turn stands, from the session's state (th-1efb59).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Keep waiting.
    Running,
    /// The turn ended in this state (idle, needs_you, limited, done, dead).
    Settled,
    /// The agent never started working after the prompt.
    Stalled,
}

/// Pure: has the turn ended?
///
/// `started` is whether the session has been seen
/// `working` since the send. A turn that has not started after `stall_after`
/// stalled (the prompt never landed, or the harness reports no state).
#[must_use]
pub fn turn_progress(started: bool, state: &str, since_send: Duration, stall_after: Duration) -> Turn {
    match state {
        "needs_you" | "limited" | "done" | "dead" => Turn::Settled,
        "idle" if started => Turn::Settled,
        _ if !started && since_send >= stall_after => Turn::Stalled,
        _ => Turn::Running,
    }
}

/// Why a session in `state` cannot take a prompt right now, or `None` when it
/// can.
///
/// An agent waiting on an approval, out of usage, or finished must not
/// have text typed into it: the text would land in a permission menu or a
/// dead pane.
#[must_use]
pub fn prompt_refusal(id: &str, state: &str) -> Option<String> {
    matches!(state, "needs_you" | "limited" | "done" | "dead").then(|| {
        format!(
            "{id} is {state} — it cannot take a prompt now{}",
            if state == "needs_you" {
                " (answer its approval with flow_approve first)"
            } else {
                ""
            }
        )
    })
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// One line per session, the way a person scans a fleet.
#[must_use]
pub fn session_line(v: &Value) -> String {
    let mut line = format!("- {} [{}] {}", s(v, "id"), s(v, "kind"), s(v, "state"));
    let title = s(v, "title");
    if !title.is_empty() {
        let _ = write!(line, " — {title}");
    }
    if let Some(p) = v.get("pearl_id").and_then(Value::as_str).filter(|p| !p.is_empty()) {
        let _ = write!(line, " ({p})");
    }
    let wt = s(v, "worktree");
    if !wt.is_empty() {
        let _ = write!(line, " @ {wt}");
    }
    if let Some(att) = v.get("attention").filter(|a| !a.is_null()) {
        let _ = write!(line, "  ⚠ {}", s(att, "reason"));
        if let Some(r) = att.get("request_id").and_then(Value::as_str).filter(|r| !r.is_empty()) {
            let _ = write!(line, " (request_id {r})");
        }
    }
    line
}

/// `flow_list`'s answer over the session rows: filtered by `state` when
/// given, finished (done/dead) rows only when asked for or named.
#[must_use]
pub fn render_list(all: &[Value], state: Option<&str>, include_finished: bool) -> String {
    let rows: Vec<&Value> = all
        .iter()
        .filter(|r| state.is_none_or(|want| s(r, "state") == want))
        .filter(|r| include_finished || state.is_some() || !matches!(s(r, "state"), "done" | "dead"))
        .collect();
    if rows.is_empty() {
        return "No SmoothFlow sessions match. Start one with flow_new.".to_string();
    }
    let mut out = format!("{} session(s):\n", rows.len());
    for r in rows {
        let _ = writeln!(out, "{}", session_line(r));
    }
    out
}

/// `flow_harnesses`' answer over the harness rows (`HarnessInfo` JSON).
#[must_use]
pub fn render_harnesses(harnesses: &[Value]) -> String {
    let mut out = String::new();
    for h in harnesses {
        let installed = h.get("installed").and_then(Value::as_bool).unwrap_or(false);
        let _ = write!(out, "- {} ({})", s(h, "name"), if installed { "installed" } else { "not installed" });
        if let Some(health) = h.get("health").filter(|x| s(x, "verdict") == "degraded") {
            let _ = write!(out, " — needs setup: {}", s(health, "reason"));
            if !s(health, "fix").is_empty() {
                let _ = write!(out, " (fix: {})", s(health, "fix"));
            }
        }
        out.push('\n');
    }
    out
}

/// `flow_repos`' answer over a repo list (`RepoList` JSON).
#[must_use]
pub fn render_repos(list: &Value) -> String {
    let repos = list.get("repos").and_then(Value::as_array).cloned().unwrap_or_default();
    if repos.is_empty() {
        let scanning = list.get("scanning").and_then(Value::as_bool).unwrap_or(false);
        return if scanning {
            "No match yet — the repo index is still scanning; try again shortly."
        } else {
            "No repo matches."
        }
        .to_string();
    }
    let mut out = String::new();
    for r in repos {
        let _ = write!(out, "- {} {}", s(&r, "name"), s(&r, "path"));
        if !s(&r, "branch").is_empty() {
            let _ = write!(out, " ({})", s(&r, "branch"));
        }
        out.push('\n');
    }
    out
}

/// The last `lines` non-blank lines of a screen.
#[must_use]
pub fn screen_tail(text: &str, lines: usize) -> String {
    let kept: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    kept[kept.len().saturating_sub(lines)..].join("\n")
}

/// How long `flow_prompt_wait` waits by default, and its bounds.
pub const PROMPT_WAIT_DEFAULT_SECS: u64 = 600;
/// The shortest wait a caller may ask for.
pub const PROMPT_WAIT_MIN_SECS: u64 = 5;
/// The longest wait a caller may ask for.
pub const PROMPT_WAIT_MAX_SECS: u64 = 3600;
/// A turn that has not started working this long after the prompt stalled.
pub const PROMPT_STALL_SECS: u64 = 60;

/// The clamped wait for a requested `timeout_secs`.
#[must_use]
pub fn prompt_wait_timeout(requested: Option<u64>) -> Duration {
    Duration::from_secs(requested.unwrap_or(PROMPT_WAIT_DEFAULT_SECS).clamp(PROMPT_WAIT_MIN_SECS, PROMPT_WAIT_MAX_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn a_turn_settles_only_after_it_started_unless_it_is_blocked() {
        let stall = Duration::from_secs(60);
        assert_eq!(turn_progress(false, "idle", S, stall), Turn::Running, "still idle right after the send");
        assert_eq!(turn_progress(true, "working", S, stall), Turn::Running);
        assert_eq!(turn_progress(true, "idle", S * 30, stall), Turn::Settled);
        assert_eq!(
            turn_progress(false, "needs_you", S, stall),
            Turn::Settled,
            "blocked on an approval ends the wait"
        );
        assert_eq!(turn_progress(false, "dead", S, stall), Turn::Settled);
        assert_eq!(turn_progress(false, "idle", S * 61, stall), Turn::Stalled, "never started");
        assert_eq!(turn_progress(false, "starting", S * 61, stall), Turn::Stalled, "never reached working");
        assert_eq!(turn_progress(true, "working", S * 600, stall), Turn::Running, "a long turn is not a stall");
    }

    #[test]
    fn a_blocked_agent_refuses_a_prompt_and_says_why() {
        for ok in ["starting", "working", "idle"] {
            assert_eq!(prompt_refusal("fs-1", ok), None, "{ok} takes a prompt");
        }
        let needs = prompt_refusal("fs-1", "needs_you").unwrap_or_default();
        assert!(needs.contains("flow_approve"), "{needs}");
        for blocked in ["limited", "done", "dead"] {
            let why = prompt_refusal("fs-1", blocked).unwrap_or_default();
            assert!(why.contains(blocked) && !why.contains("flow_approve"), "{why}");
        }
    }

    #[test]
    fn a_session_line_names_what_a_person_needs() {
        let v = json!({
            "id": "fs-1", "kind": "claude", "state": "needs_you", "title": "fix the parser",
            "pearl_id": "th-1", "worktree": "/w", "attention": { "reason": "permission", "request_id": "r9" }
        });
        let line = session_line(&v);
        for part in [
            "fs-1",
            "[claude]",
            "needs_you",
            "fix the parser",
            "(th-1)",
            "@ /w",
            "permission",
            "request_id r9",
        ] {
            assert!(line.contains(part), "{part} missing from {line}");
        }
        assert_eq!(session_line(&json!({ "id": "fs-2", "kind": "shell", "state": "idle" })), "- fs-2 [shell] idle");
    }

    #[test]
    fn the_list_hides_finished_sessions_unless_asked() {
        let rows = vec![
            json!({"id": "fs-a", "kind": "claude", "state": "working"}),
            json!({"id": "fs-b", "kind": "claude", "state": "done"}),
            json!({"id": "fs-c", "kind": "shell", "state": "idle"}),
        ];
        let live = render_list(&rows, None, false);
        assert!(live.starts_with("2 session(s)") && !live.contains("fs-b"), "{live}");
        assert!(render_list(&rows, None, true).starts_with("3 session(s)"));
        let done = render_list(&rows, Some("done"), false);
        assert!(done.contains("fs-b") && !done.contains("fs-a"), "naming a state shows it: {done}");
        assert!(render_list(&[], None, false).contains("flow_new"));
    }

    #[test]
    fn harness_and_repo_answers_read_like_the_mcp_ones() {
        let h = render_harnesses(&[
            json!({"name": "claude", "installed": true}),
            json!({"name": "codex", "installed": true, "health": {"verdict": "degraded", "reason": "hooks untrusted", "fix": "th harness enable codex"}}),
            json!({"name": "gemini", "installed": false}),
        ]);
        assert!(h.contains("- claude (installed)"), "{h}");
        assert!(h.contains("needs setup: hooks untrusted (fix: th harness enable codex)"), "{h}");
        assert!(h.contains("- gemini (not installed)"), "{h}");

        let r = render_repos(&json!({"repos": [{"name": "smooth", "path": "/d/smooth", "branch": "main"}], "scanning": false}));
        assert_eq!(r, "- smooth /d/smooth (main)\n");
        assert!(render_repos(&json!({"repos": [], "scanning": true})).contains("still scanning"));
        assert_eq!(render_repos(&json!({"repos": [], "scanning": false})), "No repo matches.");
    }

    #[test]
    fn the_screen_tail_skips_blank_lines() {
        assert_eq!(screen_tail("a\n\n b\n  \nc\nd\n", 2), "c\nd");
        assert_eq!(screen_tail("", 5), "");
        assert_eq!(screen_tail("one", 5), "one");
    }

    #[test]
    fn the_wait_is_clamped() {
        assert_eq!(prompt_wait_timeout(None), Duration::from_secs(600));
        assert_eq!(prompt_wait_timeout(Some(1)), Duration::from_secs(5));
        assert_eq!(prompt_wait_timeout(Some(99_999)), Duration::from_secs(3600));
    }
}

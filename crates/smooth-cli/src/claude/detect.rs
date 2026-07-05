//! Pure pane-state detection for Claude Code TUIs.
//!
//! A supervisor decides what to do by scraping the captured pane text.
//! All logic here is pure string analysis so it is exhaustively unit
//! testable on captured fixtures without a live tmux or a live Claude.
//!
//! These are heuristics against a TUI we don't control, so the patterns
//! are intentionally broad and the matching is case-insensitive. The
//! supervisor treats [`PaneState::RateLimited`] as "wait per the
//! governor and resend the last message" and is conservative about
//! everything else.

/// What the pane appears to be doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneState {
    /// The model is actively working (an interrupt hint is visible).
    Working,
    /// The transient server throttle fired ("temporarily limiting
    /// requests" / "Rate limited"). This is the one we auto-retry.
    RateLimited,
    /// The account hit its real usage/quota limit (resets at a time).
    /// NOT auto-retried — backing off won't help until reset.
    UsageLimit,
    /// Claude is asking the human to approve a tool/edit.
    AwaitingApproval,
    /// A non-rate-limit error is on screen.
    Errored,
    /// The input box is idle and ready for a new message.
    Idle,
    /// Nothing matched confidently.
    Unknown,
}

impl PaneState {
    /// Whether the supervisor should wait-and-resend for this state.
    #[must_use]
    pub fn is_retryable_rate_limit(self) -> bool {
        matches!(self, PaneState::RateLimited)
    }
}

/// Substrings (lowercased) that mark the transient server throttle.
const RATE_LIMIT_MARKERS: &[&str] = &[
    "temporarily limiting requests",
    "rate limited",
    "(not your usage limit)",
    "overloaded_error",
    "529",
];

/// Substrings (lowercased) that mark a real usage/quota limit. Checked
/// BEFORE the throttle markers so "usage limit" never reads as the
/// retryable throttle.
const USAGE_LIMIT_MARKERS: &[&str] = &[
    "usage limit reached",
    "approaching usage limit",
    "limit will reset",
    "limit resets at",
    "out of credits",
];

/// Substrings that mark an approval prompt.
const APPROVAL_MARKERS: &[&str] = &[
    "do you want to proceed",
    "do you want to make this edit",
    "❯ 1. yes",
    "1. yes",
    "would you like to proceed",
];

/// Substrings that mark active work (interrupt hint).
const WORKING_MARKERS: &[&str] = &["esc to interrupt", "esc to cancel", "(running", "tokens · esc"];

/// Substrings that mark a generic error.
const ERROR_MARKERS: &[&str] = &["api error", "fatal error", "request failed", "execution error"];

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Classify the pane. **Intended to run on the *visible* pane** (not full
/// scrollback): an error line that has scrolled into history would
/// otherwise make every later capture read as `RateLimited` forever.
///
/// Order matters and is deliberate:
/// 1. `Working` first — the "esc to interrupt" hint only renders while
///    the model is actively streaming, so it is the most reliable *live*
///    signal. If it is present we are working, even if an older
///    rate-limit line is still visible above it; resending then would be
///    wrong.
/// 2. `UsageLimit` before `RateLimited` — the real quota limit must never
///    be mistaken for the retryable throttle.
#[must_use]
pub fn detect_state(pane: &str) -> PaneState {
    let lower = pane.to_lowercase();

    if contains_any(&lower, WORKING_MARKERS) {
        return PaneState::Working;
    }
    // Real quota limit before the throttle — must never be confused.
    if contains_any(&lower, USAGE_LIMIT_MARKERS) {
        return PaneState::UsageLimit;
    }
    if contains_any(&lower, RATE_LIMIT_MARKERS) {
        return PaneState::RateLimited;
    }
    if contains_any(&lower, APPROVAL_MARKERS) {
        return PaneState::AwaitingApproval;
    }
    if contains_any(&lower, ERROR_MARKERS) {
        return PaneState::Errored;
    }
    // Heuristic for "idle and ready": Claude Code shows a prompt box. If
    // there's a recognizable prompt affordance and no working hint, call
    // it idle.
    if lower.contains("> ") || lower.contains("for shortcuts") || lower.contains("? for shortcuts") {
        return PaneState::Idle;
    }
    PaneState::Unknown
}

/// Best-effort extraction of the most recent **user** message from the
/// captured pane. Claude Code renders submitted user turns with a `>`
/// gutter; we collect the last contiguous run of `>`-prefixed lines.
///
/// This is a fallback for the "attach to a session I didn't launch"
/// case — when the supervisor launched the task itself it already knows
/// the prompt and should prefer that. Returns `None` when no user turn
/// is recognizable.
#[must_use]
pub fn extract_last_user_message(pane: &str) -> Option<String> {
    // Walk lines bottom-up; capture the last block of gutter lines.
    let lines: Vec<&str> = pane.lines().collect();
    let mut block: Vec<String> = Vec::new();
    let mut seen_gutter = false;
    for raw in lines.iter().rev() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if let Some(rest) = gutter_content(trimmed) {
            seen_gutter = true;
            block.push(rest.to_string());
            continue;
        }
        if seen_gutter {
            // We were in a user block and hit a non-gutter line — the
            // block is complete.
            break;
        }
    }
    if block.is_empty() {
        return None;
    }
    block.reverse();
    let joined = block.join("\n").trim().to_string();
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// If `line` is a Claude Code user-gutter line (`>` or `│ >`), return the
/// content after the gutter. Distinguishes the user gutter from shell
/// prompts by requiring the `>` to be followed by a space and content.
fn gutter_content(line: &str) -> Option<&str> {
    // Common renderings: "> text", "│ > text", "> text │".
    let stripped = line.strip_prefix("│ ").unwrap_or(line);
    let after = stripped.strip_prefix("> ").or_else(|| stripped.strip_prefix(">"))?;
    // Trim a trailing box border if present.
    let content = after.trim_end_matches([' ', '│']).trim();
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_is_detected() {
        let pane = "Ran 1 shell command\n\n● API Error: Server is temporarily limiting requests (not your usage limit) · Rate limited";
        assert_eq!(detect_state(pane), PaneState::RateLimited);
        assert!(detect_state(pane).is_retryable_rate_limit());
    }

    #[test]
    fn usage_limit_wins_over_rate_limit_wording() {
        // Even if both appear, the real quota limit must not be retried.
        let pane = "You've reached your usage limit. limit will reset at 4pm. (rate limited)";
        assert_eq!(detect_state(pane), PaneState::UsageLimit);
        assert!(!detect_state(pane).is_retryable_rate_limit());
    }

    #[test]
    fn approval_prompt_detected() {
        let pane = "Edit file foo.rs?\n  Do you want to proceed?\n  ❯ 1. Yes\n  2. No";
        assert_eq!(detect_state(pane), PaneState::AwaitingApproval);
    }

    #[test]
    fn working_detected() {
        let pane = "● Thinking…\n  (esc to interrupt · 1.2k tokens)";
        assert_eq!(detect_state(pane), PaneState::Working);
    }

    #[test]
    fn live_working_beats_stale_rate_limit_on_screen() {
        // After a successful resend the model streams again while the old
        // throttle line is still visible. The live interrupt hint must win
        // so the supervisor does NOT resend on top of working output.
        let pane = "● API Error: temporarily limiting requests · Rate limited\n● Thinking…\n  (esc to interrupt · 200 tokens)";
        assert_eq!(detect_state(pane), PaneState::Working);
    }

    #[test]
    fn idle_detected() {
        let pane = "╭─────────╮\n│ >       │\n╰─────────╯\n  ? for shortcuts";
        assert_eq!(detect_state(pane), PaneState::Idle);
    }

    #[test]
    fn unknown_when_nothing_matches() {
        assert_eq!(detect_state("just some neutral build output here"), PaneState::Unknown);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(detect_state("RATE LIMITED"), PaneState::RateLimited);
    }

    #[test]
    fn extract_simple_user_message() {
        let pane = "● done thinking\n\n> fix the flaky test in foo\n\n● Working…";
        // The last gutter block is the user message.
        assert_eq!(extract_last_user_message(pane).as_deref(), Some("fix the flaky test in foo"));
    }

    #[test]
    fn extract_multiline_user_message() {
        let pane = "● earlier\n> line one\n> line two\n● response";
        assert_eq!(extract_last_user_message(pane).as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn extract_handles_box_gutters() {
        let pane = "● prior\n│ > do the thing │\n● ok";
        assert_eq!(extract_last_user_message(pane).as_deref(), Some("do the thing"));
    }

    #[test]
    fn extract_picks_the_last_block() {
        let pane = "> first question\n● answer\n> second question\n● working";
        assert_eq!(extract_last_user_message(pane).as_deref(), Some("second question"));
    }

    #[test]
    fn extract_none_when_no_user_turn() {
        assert_eq!(extract_last_user_message("● only assistant output\nno gutter here"), None);
    }
}

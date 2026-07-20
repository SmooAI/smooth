//! Pure pane-state detection for Claude Code TUIs.
//!
//! A supervisor decides what to do by scraping the captured pane text.
//! All logic here is pure string analysis so it is exhaustively unit
//! testable on captured fixtures without a live tmux or a live Claude.
//!
//! These are heuristics against a TUI we don't control, so the patterns
//! are intentionally broad and the matching is case-insensitive. The
//! supervisor stops on [`PaneState::UsageLimit`] and is conservative
//! about everything else.
//!
//! The transient server throttle ("temporarily limiting requests") is
//! deliberately *not* a state here: Claude Code retries it internally,
//! so it needs no supervisor reaction.

/// What the pane appears to be doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneState {
    /// The model is actively working (an interrupt hint is visible).
    Working,
    /// The account hit its real usage/quota limit (resets at a time).
    /// Backing off won't help until reset, so the supervisor gives up.
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

/// Substrings (lowercased) that mark a real usage/quota limit.
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
/// otherwise make every later capture read as `Errored` forever.
///
/// `Working` is checked first — the "esc to interrupt" hint only renders
/// while the model is actively streaming, so it is the most reliable
/// *live* signal. If it is present we are working, even if an older
/// error line is still visible above it.
#[must_use]
pub fn detect_state(pane: &str) -> PaneState {
    let lower = pane.to_lowercase();

    if contains_any(&lower, WORKING_MARKERS) {
        return PaneState::Working;
    }
    if contains_any(&lower, USAGE_LIMIT_MARKERS) {
        return PaneState::UsageLimit;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_limit_is_detected() {
        let pane = "You've reached your usage limit. limit will reset at 4pm.";
        assert_eq!(detect_state(pane), PaneState::UsageLimit);
    }

    #[test]
    fn usage_limit_survives_rate_limit_wording() {
        // The transient throttle is Claude Code's own problem now, but a
        // pane mentioning both must still read as the real quota limit —
        // that's the one the supervisor stops on.
        let pane = "You've reached your usage limit. limit will reset at 4pm. (rate limited)";
        assert_eq!(detect_state(pane), PaneState::UsageLimit);
    }

    #[test]
    fn transient_throttle_is_not_a_usage_limit() {
        // Claude Code retries this itself — it must never read as the
        // quota limit and stop the supervisor.
        let pane = "● API Error: Server is temporarily limiting requests (not your usage limit) · Rate limited";
        assert_ne!(detect_state(pane), PaneState::UsageLimit);
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
    fn live_working_beats_stale_error_on_screen() {
        // Once the model recovers it streams again while the old error
        // line is still visible; the live interrupt hint must win.
        let pane = "● API Error: something went wrong\n● Thinking…\n  (esc to interrupt · 200 tokens)";
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
        assert_eq!(detect_state("USAGE LIMIT REACHED"), PaneState::UsageLimit);
    }
}

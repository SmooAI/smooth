//! Pure pane-state detection for Claude Code TUIs.
//!
//! Lives in `smooth-tmux` (moved from `smooth-cli::claude::detect`, th-7f0af3)
//! so `th claude`'s supervisor and the SmoothFlow engine share ONE copy of the
//! heuristics.
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

/// Substrings that mark an idle, ready prompt (the composer + its hint line).
const IDLE_MARKERS: &[&str] = &["? for shortcuts", "for shortcuts", "shift+tab to cycle", "> "];

/// The live signals (working / idle) render at the BOTTOM of the pane — the
/// status line under the composer. Only this many trailing lines are
/// consulted for them, so a dialog's "Esc to cancel" that has scrolled up
/// after being answered can't keep a session reading as working (th-7f0af3).
const LIVE_TAIL_LINES: usize = 12;

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
/// The last [`LIVE_TAIL_LINES`] non-blank lines of `pane`, joined.
fn live_tail(lower: &str) -> String {
    let lines: Vec<&str> = lower.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(LIVE_TAIL_LINES);
    lines[start..].join("\n")
}

#[must_use]
pub fn detect_state(pane: &str) -> PaneState {
    let lower = pane.to_lowercase();
    let tail = live_tail(&lower);

    if contains_any(&tail, WORKING_MARKERS) {
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
    if contains_any(&tail, IDLE_MARKERS) {
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
    fn answered_dialog_scrolled_up_no_longer_reads_as_working() {
        // The trust dialog's "Esc to cancel" is still visible at the top of
        // a 40-row pane after it was answered; the live composer at the
        // bottom says idle. The bottom wins.
        let mut pane = String::from("Quick safety check\n  Enter to confirm · Esc to cancel\n");
        for i in 0..20 {
            pane.push_str("output line ");
            pane.push_str(&i.to_string());
            pane.push('\n');
        }
        pane.push_str("❯ \n  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents\n");
        assert_eq!(detect_state(&pane), PaneState::Idle);
        // …but a live interrupt hint at the bottom still wins over everything.
        pane.push_str("● Thinking… (esc to interrupt)\n");
        assert_eq!(detect_state(&pane), PaneState::Working);
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

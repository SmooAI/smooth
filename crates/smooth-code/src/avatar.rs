//! The working avatar: a tiny animated Big Smooth that sits just above the
//! status bar while a turn runs, with what he is doing and for how long.
//!
//! Three rows of the splash avatar's legend ([`crate::render::welcome_banner_lines`]
//! is the full-size original): fedora, shades, smirk. The animation is the lens
//! glint sweeping across the shades, four frames at 5 fps — the frame index
//! comes from the turn's elapsed time, so the redraw cadence of the event loop
//! never changes how fast he moves.
//!
//! Shown only while a turn is in flight (`AppState::turn_started` is `Some`),
//! and only in rows the preview would otherwise use — never the input box's.
//! Off entirely under `NO_COLOR` or reduced motion (`TH_REDUCED_MOTION=1`, or
//! `SMOOTH_REDUCED_MOTION=1`); the status bar and preview spinner still show
//! that a turn is running.

use std::time::Duration;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::state::{AppState, ChatRole, ToolStatus};
use crate::theme;

/// Rows the avatar occupies when shown.
pub const ROWS: u16 = 3;

/// One animation frame every 200 ms (5 fps).
const FRAME_MS: u128 = 200;

/// The splash legend at 3 rows: `h` hat, `x` head, `g` shades, `w` glint,
/// `m` smirk. Each frame moves the glint one lens-cell to the right.
const FRAMES: [[&str; 3]; 4] = [
    [" hhhhhh ", "xwgggggx", " xxxmmx "],
    [" hhhhhh ", "xgwggggx", " xxxmmx "],
    [" hhhhhh ", "xggggwgx", " xxxmmx "],
    [" hhhhhh ", "xgggggwx", " xxxmmx "],
];

/// Whether the avatar may animate, given `NO_COLOR` and the reduced-motion vars.
///
/// `NO_COLOR` counts when set to anything non-empty (no-color.org); reduced
/// motion when set to anything but empty/`0`/`false`/`no`/`off`.
#[must_use]
pub fn motion_allowed(no_color: Option<&str>, reduced_motion: &[Option<&str>]) -> bool {
    let truthy = |v: Option<&str>| v.is_some_and(|v| !matches!(v.trim(), "" | "0" | "false" | "no" | "off"));
    no_color.is_none_or(str::is_empty) && !reduced_motion.iter().copied().any(truthy)
}

/// [`motion_allowed`] read from the process environment.
#[must_use]
pub fn motion_allowed_from_env() -> bool {
    let var = |k: &str| std::env::var(k).ok();
    let (nc, a, b) = (var("NO_COLOR"), var("TH_REDUCED_MOTION"), var("SMOOTH_REDUCED_MOTION"));
    motion_allowed(nc.as_deref(), &[a.as_deref(), b.as_deref()])
}

/// Rows to reserve for the avatar this frame: [`ROWS`] while a turn runs and
/// motion is allowed, else 0 (hidden).
#[must_use]
pub fn rows_wanted(state: &AppState) -> u16 {
    if state.avatar_motion && state.thinking && state.turn_started.is_some() {
        ROWS
    } else {
        0
    }
}

/// What he is doing right now, for the line beside the avatar.
#[must_use]
pub fn activity(state: &AppState) -> String {
    let last = state.messages.iter().rev().find(|m| m.role == ChatRole::Assistant);
    if let Some(tc) = last.and_then(|m| {
        m.tool_calls
            .iter()
            .rev()
            .find(|t| matches!(t.status, ToolStatus::Running | ToolStatus::Pending))
    }) {
        return format!("running {}", tc.tool_name);
    }
    if last.is_some_and(|m| m.streaming && !m.content.is_empty()) {
        return "writing".to_string();
    }
    "thinking".to_string()
}

/// `12s`, `3m 07s` — compact, ticking once a second.
#[must_use]
pub fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// The avatar's lines for a turn `elapsed` in: three art rows, the middle one
/// carrying `activity · elapsed`.
#[must_use]
pub fn lines(activity: &str, elapsed: Duration) -> Vec<Line<'static>> {
    let frame = &FRAMES[usize::try_from(elapsed.as_millis() / FRAME_MS).unwrap_or(0) % FRAMES.len()];
    let n = frame.len();
    frame
        .iter()
        .enumerate()
        .map(|(row, art)| {
            let head = Style::default().fg(theme::th_gradient_color(row, n)).add_modifier(Modifier::BOLD);
            let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
            spans.extend(art.chars().map(|c| match c {
                'h' | 'g' | 'm' => Span::styled("\u{2593}", Style::default().fg(theme::FACE_DARK)),
                'x' => Span::styled("\u{2588}", head),
                'w' => Span::styled("\u{2580}", Style::default().fg(theme::SMOO_WHITE)),
                _ => Span::raw(" "),
            }));
            if row == 1 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(activity.to_string(), theme::assistant_label()));
                spans.push(Span::styled(format!(" · {}", format_elapsed(elapsed)), theme::muted()));
            }
            Line::from(spans)
        })
        .collect()
}

/// The avatar for `state` now, or nothing when it should be hidden.
#[must_use]
pub fn state_lines(state: &AppState) -> Vec<Line<'static>> {
    match (rows_wanted(state), state.turn_started) {
        (0, _) | (_, None) => Vec::new(),
        (_, Some(t)) => lines(&activity(state), t.elapsed()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ChatMessage, ToolCallState};

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    fn state() -> AppState {
        let mut s = AppState::new(std::path::PathBuf::from("/tmp"));
        s.avatar_motion = true;
        s
    }

    #[test]
    fn appears_when_a_turn_starts_and_hides_when_it_ends() {
        let mut s = state();
        s.sync_turn_clock();
        assert_eq!(rows_wanted(&s), 0, "idle: hidden");
        assert!(state_lines(&s).is_empty());

        s.thinking = true;
        s.sync_turn_clock();
        assert!(s.turn_started.is_some(), "turn clock starts with the turn");
        assert_eq!(rows_wanted(&s), ROWS);
        let shown = state_lines(&s);
        assert_eq!(shown.len(), usize::from(ROWS));
        assert!(text(&shown)[1].contains("thinking · 0s"), "{:?}", text(&shown));

        s.thinking = false;
        s.sync_turn_clock();
        assert!(s.turn_started.is_none(), "turn clock stops with the turn");
        assert_eq!(rows_wanted(&s), 0);
        assert!(state_lines(&s).is_empty(), "idle again: hidden");
    }

    #[test]
    fn turn_clock_is_not_reset_mid_turn() {
        let mut s = state();
        s.thinking = true;
        s.sync_turn_clock();
        let t0 = s.turn_started;
        s.sync_turn_clock();
        assert_eq!(s.turn_started, t0);
    }

    #[test]
    fn off_without_motion() {
        let mut s = state();
        s.avatar_motion = false;
        s.thinking = true;
        s.sync_turn_clock();
        assert_eq!(rows_wanted(&s), 0);
        assert!(state_lines(&s).is_empty());
    }

    #[test]
    fn no_color_and_reduced_motion_turn_it_off() {
        assert!(motion_allowed(None, &[None, None]));
        assert!(motion_allowed(Some(""), &[Some("0"), Some("false")]));
        assert!(!motion_allowed(Some("1"), &[None, None]), "NO_COLOR");
        assert!(!motion_allowed(None, &[Some("1"), None]), "TH_REDUCED_MOTION");
        assert!(!motion_allowed(None, &[None, Some("yes")]), "SMOOTH_REDUCED_MOTION");
    }

    #[test]
    fn frames_animate_at_five_fps_and_keep_their_width() {
        let a = text(&lines("thinking", Duration::from_millis(0)));
        let b = text(&lines("thinking", Duration::from_millis(199)));
        let c = text(&lines("thinking", Duration::from_millis(200)));
        assert_eq!(a, b, "same frame within 200 ms");
        assert_ne!(a[1], c[1], "glint moves on the next frame");
        assert_eq!(a[0], c[0]);
        for f in FRAMES {
            assert!(f.iter().all(|r| r.chars().count() == 8), "frames are equal width: {f:?}");
        }
    }

    #[test]
    fn activity_names_the_running_tool() {
        let mut s = state();
        let mut msg = ChatMessage::assistant("");
        msg.streaming = true;
        msg.tool_calls.push(ToolCallState::new("t1", "bash", &serde_json::json!({"command": "ls"})));
        s.messages.push(msg);
        assert_eq!(activity(&s), "running bash");
        s.messages.last_mut().unwrap().tool_calls[0].status = ToolStatus::Done;
        assert_eq!(activity(&s), "thinking");
        s.messages.last_mut().unwrap().content = "Here".into();
        assert_eq!(activity(&s), "writing");
    }

    #[test]
    fn elapsed_is_compact() {
        assert_eq!(format_elapsed(Duration::from_secs(9)), "9s");
        assert_eq!(format_elapsed(Duration::from_secs(187)), "3m 07s");
    }
}

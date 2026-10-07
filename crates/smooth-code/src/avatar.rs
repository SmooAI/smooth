//! The working animation: a rainbow double-helix ribbon that flows along
//! just above the status bar while a turn runs, beside what the agent is doing
//! and for how long. (The module keeps its old "avatar" name; it used to be a
//! tiny Big Smooth face.)
//!
//! Two sine strands half a turn apart are drawn on a half-block grid (3 rows
//! = 6 sub-rows), the hue sweeping along the ribbon and drifting with time;
//! the strand that is "behind" at each column is dimmed so it reads as a twist.
//! The frame comes from the turn's elapsed time quantized to [`FRAME_MS`], so
//! the event loop's redraw cadence never changes how fast it moves.
//!
//! Shown only while a turn is in flight (`AppState::turn_started` is `Some`),
//! and only in rows the preview would otherwise use — never the input box's.
//! Off entirely under `NO_COLOR` or reduced motion (`TH_REDUCED_MOTION=1`, or
//! `SMOOTH_REDUCED_MOTION=1`); the status bar and preview spinner still show
//! that a turn is running.

use std::f64::consts::{PI, TAU};
use std::time::Duration;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::state::{AppState, ChatRole, ToolStatus};
use crate::theme;

/// Rows the animation occupies when shown.
pub const ROWS: u16 = 3;

/// Columns of ribbon.
const WIDTH: usize = 24;

/// One animation frame every 80 ms (12.5 fps; the event loop polls at 50 ms).
const FRAME_MS: u128 = 80;

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

/// HSV (hue in degrees, s/v in 0..=1) to an RGB terminal color.
fn hsv(h: f64, s: f64, v: f64) -> Color {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to = |f: f64| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(to(r), to(g), to(b))
}

/// The lit color of the sub-pixel at column `x`, sub-row `sub` (0 = top of
/// 6) at time `t` seconds, or `None` when it is dark.
fn pixel(x: usize, sub: usize, t: f64) -> Option<Color> {
    let phase = x as f64 * 0.42 - t * 3.2;
    let mid = 2.5;
    let amp = 2.4;
    // Strand A and its twin half a turn later; the one with the larger
    // cosine is in front at this column.
    let strands = [
        (mid + amp * phase.sin(), phase.cos(), 0.0),
        (mid + amp * (phase + PI).sin(), (phase + PI).cos(), 150.0),
    ];
    let hue = x as f64 * (300.0 / WIDTH as f64) + t * 90.0;
    strands
        .iter()
        .filter(|(y, _, _)| (sub as f64 - y).abs() < 0.62)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|&(_, depth, hue_off)| {
            let v = if depth >= 0.0 { 1.0 } else { 0.45 + 0.3 * (depth + 1.0) };
            hsv(hue + hue_off, 0.85, v)
        })
}

/// The animation's lines for a turn `elapsed` in: three ribbon rows, the
/// middle one carrying `activity · elapsed`.
#[must_use]
pub fn lines(activity: &str, elapsed: Duration) -> Vec<Line<'static>> {
    let frame = elapsed.as_millis() / FRAME_MS;
    let t = (frame * FRAME_MS) as f64 / 1000.0 % (TAU * 100.0);
    (0..usize::from(ROWS))
        .map(|row| {
            let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
            spans.extend((0..WIDTH).map(|x| match (pixel(x, row * 2, t), pixel(x, row * 2 + 1, t)) {
                (Some(top), Some(bottom)) => Span::styled("\u{2580}", Style::default().fg(top).bg(bottom)),
                (Some(top), None) => Span::styled("\u{2580}", Style::default().fg(top)),
                (None, Some(bottom)) => Span::styled("\u{2584}", Style::default().fg(bottom)),
                (None, None) => Span::raw(" "),
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
    fn ribbon_animates_per_frame_and_keeps_its_width() {
        let a = lines("thinking", Duration::from_millis(0));
        let b = lines("thinking", Duration::from_millis(FRAME_MS as u64 - 1));
        let c = lines("thinking", Duration::from_millis(FRAME_MS as u64 * 3));
        assert_eq!(a, b, "same frame within one frame period");
        assert_ne!(a, c, "the ribbon moves on later frames");
        for l in [&a, &c] {
            let t = text(l);
            assert!(t.iter().all(|r| r.chars().take(1 + WIDTH).count() == 1 + WIDTH));
            assert_eq!(t[0].chars().count(), 1 + WIDTH, "art rows are a fixed width: {t:?}");
            assert!(t.iter().any(|r| r.contains(['\u{2580}', '\u{2584}'])), "ribbon renders: {t:?}");
        }
    }

    #[test]
    fn ribbon_is_colorful() {
        let colors: std::collections::HashSet<String> = lines("thinking", Duration::from_secs(1))
            .iter()
            .flat_map(|l| l.spans.iter().filter_map(|s| s.style.fg).map(|c| format!("{c:?}")))
            .collect();
        assert!(colors.len() >= 10, "many hues across the ribbon: {colors:?}");
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv(0.0, 1.0, 1.0), Color::Rgb(255, 0, 0));
        assert_eq!(hsv(120.0, 1.0, 1.0), Color::Rgb(0, 255, 0));
        assert_eq!(hsv(240.0, 1.0, 1.0), Color::Rgb(0, 0, 255));
        assert_eq!(hsv(-120.0, 1.0, 1.0), hsv(240.0, 1.0, 1.0));
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

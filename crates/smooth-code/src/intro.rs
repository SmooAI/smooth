//! The startup intro: Big Smooth pops up from below with a little bounce,
//! cocks his head, winks, and settles into the splash avatar — once, on a
//! fresh session, in about 1.4 s.
//!
//! The splash goes into terminal scrollback (see
//! [`crate::render::welcome_banner_lines`]), where nothing can animate, so the
//! intro plays first in its own small inline viewport. Its last frame is the
//! resting avatar, the cursor is parked under it, and the app then pushes only
//! the rest of the splash ([`crate::render::welcome_banner_tail_lines`]).
//!
//! Skipped on resume, under `NO_COLOR` / reduced motion (same switches as the
//! working animation, [`crate::avatar::motion_allowed_from_env`]), when stdout
//! is not a terminal, or when the terminal is too small. Any key press jumps
//! straight to the resting frame (the key is left in the queue for the app).

use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::render::{avatar_lines, AVATAR_ROWS};

/// Furthest any avatar row moves sideways during the tilt.
pub const MAX_SHIFT: i16 = 3;

/// Rows the intro draws: one spacer above the avatar (the bounce uses it)
/// plus the avatar.
pub const AREA_H: u16 = 1 + AVATAR_ROWS.len() as u16;

/// Each frame is shown this long.
const FRAME: Duration = Duration::from_millis(50);

/// One frame of the intro. `lift` is how many rows below its resting place
/// the avatar sits (negative = above, into the spacer); `tilt` leans the head
/// around the chin (positive = top to the left); `wink` closes the right lens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub lift: i16,
    pub tilt: f64,
    pub wink: bool,
}

const fn pose(lift: i16, tilt: f64, wink: bool) -> Pose {
    Pose { lift, tilt, wink }
}

/// The whole intro, one entry per [`FRAME`]: pop up with an overshoot, lean
/// in, wink at the peak of the lean, swing back past center, settle.
pub const TIMELINE: [Pose; 28] = [
    pose(12, 0.0, false),
    pose(9, 0.0, false),
    pose(6, 0.0, false),
    pose(4, 0.0, false),
    pose(2, 0.0, false),
    pose(1, 0.0, false),
    pose(0, 0.0, false),
    pose(-1, 0.0, false),
    pose(-1, 0.0, false),
    pose(0, 0.0, false),
    pose(0, 0.08, false),
    pose(0, 0.16, false),
    pose(0, 0.22, false),
    pose(0, 0.25, false),
    pose(0, 0.25, true),
    pose(0, 0.25, true),
    pose(0, 0.25, true),
    pose(0, 0.25, true),
    pose(0, 0.25, false),
    pose(0, 0.2, false),
    pose(0, 0.12, false),
    pose(0, 0.04, false),
    pose(0, -0.06, false),
    pose(0, -0.1, false),
    pose(0, -0.06, false),
    pose(0, 0.0, false),
    pose(0, 0.0, false),
    pose(0, 0.0, false),
];

/// The resting pose: exactly the static splash avatar.
pub const REST: Pose = pose(0, 0.0, false);

/// The [`AREA_H`] lines for one pose.
#[must_use]
pub fn frame_lines(p: Pose) -> Vec<Line<'static>> {
    let chin = (AVATAR_ROWS.len() - 1) as f64;
    #[allow(clippy::cast_possible_truncation)]
    let shift = |row: usize| ((row as f64 - chin) * p.tilt).round() as i16;
    let avatar = avatar_lines(shift, p.wink);
    (0..i16::try_from(AREA_H).unwrap_or(0))
        .map(|y| {
            let idx = y - 1 - p.lift;
            usize::try_from(idx).ok().and_then(|i| avatar.get(i).cloned()).unwrap_or_default()
        })
        .collect()
}

/// Whether the intro should play in this process, given the terminal size.
#[must_use]
pub fn should_play(cols: u16, rows: u16) -> bool {
    crate::avatar::motion_allowed_from_env() && io::stdout().is_terminal() && cols >= 40 && rows >= AREA_H + 16
}

/// Play the intro in its own inline viewport and leave the cursor on the
/// line below it. Raw mode must already be on. Returns once the resting
/// frame is drawn.
pub fn play() -> anyhow::Result<()> {
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(AREA_H),
        },
    )?;
    let draw = |terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, p: Pose| {
        terminal.draw(|f| f.render_widget(Paragraph::new(frame_lines(p)), f.area())).map(|_| ())
    };
    for p in TIMELINE {
        draw(&mut terminal, p)?;
        if crossterm::event::poll(FRAME)? {
            break;
        }
    }
    draw(&mut terminal, REST)?;
    let bottom = terminal.get_frame().area().bottom().saturating_sub(1);
    terminal.set_cursor_position((0, bottom))?;
    terminal.show_cursor()?;
    drop(terminal);
    let mut out = io::stdout();
    write!(out, "\r\n")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn every_frame_fills_the_area_at_one_width() {
        for p in TIMELINE {
            let t = text(&frame_lines(p));
            assert_eq!(t.len(), usize::from(AREA_H));
            let widths: std::collections::HashSet<usize> = t.iter().map(|r| r.chars().count()).filter(|&w| w > 0).collect();
            assert!(widths.len() <= 1, "rows in one frame share a width so centering can't skew: {p:?} {widths:?}");
        }
    }

    #[test]
    fn rests_on_the_static_splash_avatar() {
        assert_eq!(*TIMELINE.last().unwrap(), REST);
        let rest = frame_lines(REST);
        assert!(rest[0].spans.is_empty(), "spacer row stays empty at rest");
        assert_eq!(rest[1..], avatar_lines(|_| 0, false)[..], "the last frame is the splash avatar");
    }

    #[test]
    fn pops_up_from_below() {
        let first = text(&frame_lines(TIMELINE[0]));
        assert!(
            first[..usize::from(AREA_H) - 1].iter().all(|r| r.trim().is_empty()),
            "starts below the area: {first:#?}"
        );
        assert!(TIMELINE.iter().any(|p| p.lift < 0), "overshoots into the spacer row");
    }

    #[test]
    fn tilts_and_winks() {
        let lean = TIMELINE.iter().copied().find(|p| p.tilt > 0.2 && !p.wink).unwrap();
        let rows = text(&frame_lines(lean));
        let lead = |r: &str| r.len() - r.trim_start().len();
        let (hat, chin) = (&rows[1], &rows[usize::from(AREA_H) - 1]);
        assert!(lead(hat) + 2 <= lead(chin), "the hat leans left of the chin: {rows:#?}");
        assert_ne!(frame_lines(lean), frame_lines(Pose { tilt: 0.0, ..lean }));

        let wink = TIMELINE.iter().copied().find(|p| p.wink).unwrap();
        let open = Pose { wink: false, ..wink };
        let glints = |p: Pose| text(&frame_lines(p)).iter().map(|r| r.matches('\u{2580}').count()).sum::<usize>();
        assert_eq!(glints(open), 2);
        assert_eq!(glints(wink), 1, "one glint goes out with the wink");
    }
}

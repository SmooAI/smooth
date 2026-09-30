//! Layout math: cell metrics from the font, pane frames to grid sizes, and
//! the one size a session gets when several panes show it.

use std::collections::BTreeMap;

use smooth_flow_client::pane::Rect;

/// Padding inside a pane around the grid, in pixels.
pub const PANE_PADDING: f32 = 6.0;

/// One terminal cell, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
}

impl CellMetrics {
    /// From the font's measured advance (of `m`), ascent and descent. The
    /// height is ascent plus descent — the font's own line, no extra leading —
    /// rounded up to whole pixels so rows never overlap. A font that won't
    /// measure (advance or height not positive) falls back to the usual
    /// monospace proportions.
    #[must_use]
    pub fn measured(font_size: f32, advance: Option<f32>, ascent: f32, descent: f32) -> Self {
        let width = advance.filter(|a| a.is_finite() && *a > 0.0).unwrap_or(font_size * 0.6);
        let line = ascent.abs() + descent.abs();
        let height = if line.is_finite() && line > 0.0 {
            line.ceil()
        } else {
            (font_size * 1.3).ceil()
        };
        Self { width, height }
    }

    /// The grid that fits `frame` (a pane), after [`PANE_PADDING`]; never
    /// smaller than 2×1.
    #[must_use]
    pub fn grid(&self, frame: Rect) -> (usize, usize) {
        let w = (frame.w as f32 - 2.0 * PANE_PADDING).max(0.0);
        let h = (frame.h as f32 - 2.0 * PANE_PADDING).max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "floored, non-negative, small")]
        let (cols, rows) = ((w / self.width).floor() as usize, (h / self.height).floor() as usize);
        (cols.max(2), rows.max(1))
    }
}

/// The size each session is attached at, from the panes on screen.
///
/// A session has one PTY, so one size, however many panes show it. It gets
/// the **smallest** of those panes on each axis (tmux's `window-size
/// smallest`): every pane then shows the whole screen, and a larger pane
/// just has spare space below and to the right. The focused pane's size would
/// clip the others, and a program redrawing for a size that changes with
/// every focus move is worse.
#[must_use]
pub fn session_sizes<'a>(panes: impl IntoIterator<Item = (&'a str, (usize, usize))>) -> BTreeMap<String, (usize, usize)> {
    let mut out: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (session, (c, r)) in panes {
        out.entry(session.to_string())
            .and_modify(|(oc, or)| (*oc, *or) = ((*oc).min(c), (*or).min(r)))
            .or_insert((c, r));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_come_from_the_font_with_a_fallback() {
        let m = CellMetrics::measured(13.0, Some(7.8), 12.1, -3.2);
        assert!((m.width - 7.8).abs() < f32::EPSILON);
        assert!((m.height - 16.0).abs() < f32::EPSILON, "ascent + |descent|, rounded up");
        let fallback = CellMetrics::measured(10.0, None, 0.0, 0.0);
        assert!((fallback.width - 6.0).abs() < 1e-5 && (fallback.height - 13.0).abs() < 1e-5);
    }

    #[test]
    fn grid_fits_the_frame_after_padding() {
        let m = CellMetrics { width: 8.0, height: 16.0 };
        let r = |w: f64, h: f64| Rect { x: 0.0, y: 0.0, w, h };
        assert_eq!(m.grid(r(812.0, 492.0)), (100, 30));
        assert_eq!(m.grid(r(819.0, 507.0)), (100, 30), "partial cells don't count");
        assert_eq!(m.grid(r(4.0, 4.0)), (2, 1), "never a zero grid");
    }

    #[test]
    fn a_session_in_two_panes_gets_the_smaller_on_each_axis() {
        let sizes = session_sizes([("a", (120, 40)), ("b", (80, 24)), ("a", (60, 50))]);
        assert_eq!(sizes.get("a"), Some(&(60, 40)));
        assert_eq!(sizes.get("b"), Some(&(80, 24)));
        assert!(session_sizes([]).is_empty());
    }
}

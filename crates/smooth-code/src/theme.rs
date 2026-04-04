//! Smoo AI branding colors and semantic style functions.
//!
//! Colors derived from `packages/ui/globals.css` in the smooai monorepo.
//! "smoo" text: gradient orange → red. "th" text: gradient green → blue.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

// ── Core brand colors (from globals.css) ──────────────────────

/// Smoo AI brand green/teal (#00a6a6) — --color-smooai-green
pub const SMOO_GREEN: Color = Color::Rgb(0, 166, 166);
/// Smoo AI brand orange (#f49f0a) — --color-smooai-orange
pub const SMOO_ORANGE: Color = Color::Rgb(244, 159, 10);
/// Smoo AI brand red (#ff6b6c) — --color-smooai-red
pub const SMOO_RED: Color = Color::Rgb(255, 107, 108);
/// Smoo AI brand blue (#bbdef0) — --color-smooai-blue
pub const SMOO_BLUE: Color = Color::Rgb(187, 222, 240);
/// Smoo AI dark blue (#020618) — --color-smooai-dark-blue
pub const SMOO_DARK_BLUE: Color = Color::Rgb(2, 6, 24);
/// Smoo AI white (#f8fafc) — --color-smooai-white
pub const SMOO_WHITE: Color = Color::Rgb(248, 250, 252);

// ── Extended palette ──────────────────────────────────────────

pub const SMOO_ORANGE_400: Color = Color::Rgb(248, 190, 87); // #f8be57
pub const SMOO_ORANGE_600: Color = Color::Rgb(200, 130, 8); // approx
pub const SMOO_RED_400: Color = Color::Rgb(255, 148, 149); // #ff9495
pub const SMOO_RED_600: Color = Color::Rgb(255, 51, 52); // #ff3334
pub const SMOO_GREEN_400: Color = Color::Rgb(74, 255, 255); // #4affff
pub const SMOO_GREEN_600: Color = Color::Rgb(0, 248, 248); // #00f8f8
pub const SMOO_BLUE_400: Color = Color::Rgb(95, 177, 220); // #5fb1dc
pub const SMOO_BLUE_600: Color = Color::Rgb(37, 122, 166); // #257aa6
pub const SMOO_GRAY_500: Color = Color::Rgb(134, 134, 134); // #868686
pub const SMOO_GRAY_700: Color = Color::Rgb(78, 78, 78); // #4e4e4e
pub const SMOO_GRAY_900: Color = Color::Rgb(29, 29, 29); // #1d1d1d

/// Muted/secondary text — --color-smooai-gray
pub const MUTED: Color = Color::Rgb(163, 163, 163);
/// Error indicator — --color-smooai-red
pub const ERROR_RED: Color = Color::Rgb(255, 107, 108);
/// Success indicator
pub const SUCCESS_GREEN: Color = Color::Rgb(46, 160, 67);

// ── Gradient title spans ──────────────────────────────────────

/// "smoo" gradient: orange → red
pub fn smoo_gradient() -> Vec<Span<'static>> {
    vec![
        Span::styled("s", Style::default().fg(SMOO_ORANGE).add_modifier(Modifier::BOLD)),
        Span::styled("m", Style::default().fg(Color::Rgb(248, 140, 40)).add_modifier(Modifier::BOLD)),
        Span::styled("o", Style::default().fg(Color::Rgb(252, 120, 70)).add_modifier(Modifier::BOLD)),
        Span::styled("o", Style::default().fg(SMOO_RED).add_modifier(Modifier::BOLD)),
    ]
}

/// "th" gradient: green → blue
pub fn th_gradient() -> Vec<Span<'static>> {
    vec![
        Span::styled("t", Style::default().fg(SMOO_GREEN).add_modifier(Modifier::BOLD)),
        Span::styled("h", Style::default().fg(SMOO_BLUE_400).add_modifier(Modifier::BOLD)),
    ]
}

/// Full branded title: "th" (green→blue) + " " + "smoo" (orange→red)
pub fn branded_title() -> Vec<Span<'static>> {
    let mut spans = th_gradient();
    spans.push(Span::raw(" "));
    spans.extend(smoo_gradient());
    spans
}

/// Style for the main title bar.
pub fn title() -> Style {
    Style::default().fg(SMOO_GREEN).add_modifier(Modifier::BOLD)
}

/// Style for user message labels ("You").
pub fn user_label() -> Style {
    Style::default().fg(SMOO_ORANGE).add_modifier(Modifier::BOLD)
}

/// Style for assistant message labels ("Smooth").
pub fn assistant_label() -> Style {
    Style::default().fg(SMOO_GREEN).add_modifier(Modifier::BOLD)
}

/// Style for the input text area.
pub fn input_style() -> Style {
    Style::default().fg(Color::White)
}

/// Style for the status bar.
pub fn status_style() -> Style {
    Style::default().fg(MUTED)
}

/// Style for muted/secondary text.
pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

/// Style for error text.
pub fn error() -> Style {
    Style::default().fg(ERROR_RED)
}

/// Style for success text.
pub fn success() -> Style {
    Style::default().fg(SUCCESS_GREEN)
}

// ── Gradient and dynamic color helpers ───────────────────────

/// Interpolate between `SMOO_ORANGE` and `SMOO_GREEN` based on row position.
///
/// `row` 0 returns pure orange, `row == total - 1` returns pure green.
/// `total` must be >= 1; if 1, returns orange.
pub fn gradient_row(row: usize, total: usize) -> Style {
    let total = total.max(1);
    let t = if total <= 1 { 0.0 } else { row as f64 / (total as f64 - 1.0) };

    // SMOO_ORANGE = (244, 159, 10), SMOO_GREEN = (0, 166, 166)
    let r = lerp_u8(244, 0, t);
    let g = lerp_u8(159, 166, t);
    let b = lerp_u8(10, 166, t);

    Style::default().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD)
}

/// Return a color for a file based on its extension.
pub fn file_color(extension: &str) -> Color {
    match extension {
        "rs" => SMOO_ORANGE,
        "ts" | "tsx" | "js" | "jsx" => SMOO_BLUE_400,
        "md" => SMOO_GREEN,
        "json" => Color::Rgb(255, 255, 100),                  // yellow
        "toml" | "yaml" | "yml" => Color::Rgb(100, 220, 220), // cyan
        _ => Color::White,
    }
}

/// Style for a tool-call status border.
pub fn tool_status_border(status: crate::state::ToolStatus) -> Style {
    use crate::state::ToolStatus;
    match status {
        ToolStatus::Pending => Style::default().fg(MUTED),
        ToolStatus::Running => Style::default().fg(SMOO_ORANGE),
        ToolStatus::Done => Style::default().fg(SUCCESS_GREEN),
        ToolStatus::Error => Style::default().fg(ERROR_RED),
    }
}

/// Panel border style — bright when active (focused), dim when inactive.
pub fn panel_border(active: bool) -> Style {
    if active {
        Style::default().fg(SMOO_GREEN).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SMOO_GRAY_700)
    }
}

/// Linear interpolation between two u8 values.
fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    let result = f64::from(a) + (f64::from(b) - f64::from(a)) * t;
    result.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_constants_exist() {
        assert_eq!(SMOO_GREEN, Color::Rgb(0, 166, 166));
        assert_eq!(SMOO_ORANGE, Color::Rgb(244, 159, 10));
        assert_eq!(SMOO_RED, Color::Rgb(255, 107, 108));
        assert_eq!(SMOO_BLUE, Color::Rgb(187, 222, 240));
        assert_eq!(SMOO_DARK_BLUE, Color::Rgb(2, 6, 24));
        assert_eq!(SMOO_WHITE, Color::Rgb(248, 250, 252));
        assert_eq!(MUTED, Color::Rgb(163, 163, 163));
        assert_eq!(ERROR_RED, Color::Rgb(255, 107, 108));
        assert_eq!(SUCCESS_GREEN, Color::Rgb(46, 160, 67));
    }

    #[test]
    fn test_smoo_gradient_has_4_chars() {
        let spans = smoo_gradient();
        assert_eq!(spans.len(), 4);
    }

    #[test]
    fn test_th_gradient_has_2_chars() {
        let spans = th_gradient();
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn test_branded_title() {
        let spans = branded_title();
        assert_eq!(spans.len(), 7); // t, h, " ", s, m, o, o
    }

    #[test]
    fn test_style_functions_return_styles() {
        // Ensure style functions don't panic and return non-default styles
        let t = title();
        assert_eq!(t.fg, Some(SMOO_GREEN));

        let ul = user_label();
        assert_eq!(ul.fg, Some(SMOO_ORANGE));

        let al = assistant_label();
        assert_eq!(al.fg, Some(SMOO_GREEN));

        let is = input_style();
        assert_eq!(is.fg, Some(Color::White));

        let ss = status_style();
        assert_eq!(ss.fg, Some(MUTED));
    }

    #[test]
    fn test_gradient_row_interpolates_correctly() {
        // First row = pure SMOO_ORANGE
        let first = gradient_row(0, 6);
        assert_eq!(first.fg, Some(Color::Rgb(244, 159, 10)));

        // Last row = pure SMOO_GREEN
        let last = gradient_row(5, 6);
        assert_eq!(last.fg, Some(Color::Rgb(0, 166, 166)));

        // Middle row should be somewhere between
        let mid = gradient_row(3, 6);
        if let Some(Color::Rgb(r, g, b)) = mid.fg {
            assert!(r < 244, "mid red should be less than orange red");
            assert!(r > 0, "mid red should be greater than green red");
            assert!(b > 10, "mid blue should be greater than orange blue");
            assert!(b < 166, "mid blue should be less than green blue");
            // green channel stays close (159 -> 166)
            assert!(g >= 159);
            assert!(g <= 166);
        } else {
            panic!("expected Rgb color");
        }

        // Edge case: total=1 returns orange
        let single = gradient_row(0, 1);
        assert_eq!(single.fg, Some(Color::Rgb(244, 159, 10)));
    }

    #[test]
    fn test_file_color_returns_different_colors() {
        let rs = file_color("rs");
        let ts = file_color("ts");
        let md = file_color("md");
        let json = file_color("json");
        let toml = file_color("toml");
        let other = file_color("xyz");

        assert_eq!(rs, SMOO_ORANGE);
        assert_eq!(ts, SMOO_BLUE_400);
        assert_eq!(md, SMOO_GREEN);
        // Ensure json/toml/other are all distinct
        assert_ne!(json, toml);
        assert_ne!(json, other);
        assert_eq!(other, Color::White);
    }

    #[test]
    fn test_tool_status_border_returns_correct_colors() {
        use crate::state::ToolStatus;

        let pending = tool_status_border(ToolStatus::Pending);
        assert_eq!(pending.fg, Some(MUTED));

        let running = tool_status_border(ToolStatus::Running);
        assert_eq!(running.fg, Some(SMOO_ORANGE));

        let done = tool_status_border(ToolStatus::Done);
        assert_eq!(done.fg, Some(SUCCESS_GREEN));

        let error = tool_status_border(ToolStatus::Error);
        assert_eq!(error.fg, Some(ERROR_RED));
    }

    #[test]
    fn test_panel_border_active_vs_inactive() {
        let active = panel_border(true);
        let inactive = panel_border(false);

        assert_ne!(active.fg, inactive.fg);
        assert_eq!(active.fg, Some(SMOO_GREEN));
        assert_eq!(inactive.fg, Some(SMOO_GRAY_700));
    }
}

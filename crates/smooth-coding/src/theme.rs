//! Smoo AI branding colors and semantic style functions.

use ratatui::style::{Color, Modifier, Style};

/// Smoo AI brand green/teal (#00a6a6).
pub const SMOO_GREEN: Color = Color::Rgb(0, 166, 166);
/// Smoo AI brand orange (#f49f0a).
pub const SMOO_ORANGE: Color = Color::Rgb(244, 159, 10);
/// Muted/secondary text.
pub const MUTED: Color = Color::Rgb(128, 128, 128);
/// Error indicator.
pub const ERROR_RED: Color = Color::Rgb(248, 81, 73);
/// Success indicator.
pub const SUCCESS_GREEN: Color = Color::Rgb(46, 160, 67);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_constants_exist() {
        // Verify the RGB values are correct
        assert_eq!(SMOO_GREEN, Color::Rgb(0, 166, 166));
        assert_eq!(SMOO_ORANGE, Color::Rgb(244, 159, 10));
        assert_eq!(MUTED, Color::Rgb(128, 128, 128));
        assert_eq!(ERROR_RED, Color::Rgb(248, 81, 73));
        assert_eq!(SUCCESS_GREEN, Color::Rgb(46, 160, 67));
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
}

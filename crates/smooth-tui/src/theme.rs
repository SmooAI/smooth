//! Smoo AI brand colors for the TUI.

use ratatui::style::{Color, Modifier, Style};

/// Smoo AI brand green/teal (#00a6a6).
pub const SMOO_GREEN: Color = Color::Rgb(0, 166, 166);
/// Smoo AI brand orange (#f49f0a).
pub const SMOO_ORANGE: Color = Color::Rgb(244, 159, 10);
/// Muted text.
pub const MUTED: Color = Color::Rgb(163, 163, 163);

pub fn title() -> Style {
    Style::default().fg(SMOO_ORANGE).add_modifier(Modifier::BOLD)
}

pub fn subtitle() -> Style {
    Style::default().fg(SMOO_GREEN).add_modifier(Modifier::BOLD)
}

pub fn active_tab() -> Style {
    Style::default().fg(SMOO_ORANGE).add_modifier(Modifier::BOLD)
}

pub fn inactive_tab() -> Style {
    Style::default().fg(MUTED)
}

pub fn status_style(status: &str) -> Style {
    match status {
        "healthy" | "connected" => Style::default().fg(Color::Green),
        "degraded" => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::Red),
    }
}

pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

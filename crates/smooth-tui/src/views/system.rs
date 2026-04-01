//! System view — detailed system info and configuration.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::theme;
use crate::views::dashboard::HealthData;

pub fn render(f: &mut Frame, area: Rect, health: &HealthData, leader_url: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(10), Constraint::Length(8), Constraint::Min(0)])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled("System", theme::title())));
    f.render_widget(title, chunks[0]);

    // Connection info
    let conn_lines = vec![
        Line::from(vec![
            Span::styled("  Leader URL: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(leader_url),
        ]),
        Line::from(vec![
            Span::styled("  Version:    ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(env!("CARGO_PKG_VERSION")),
        ]),
        Line::from(vec![
            Span::styled("  Uptime:     ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format_uptime(health.leader_uptime)),
        ]),
        Line::from(vec![
            Span::styled("  Database:   ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&health.db_path),
        ]),
        Line::from(vec![
            Span::styled("  Tailscale:  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "{} ({})",
                health.tailscale_status,
                health.tailscale_hostname.as_deref().unwrap_or("n/a")
            )),
        ]),
        Line::from(vec![
            Span::styled("  Sandbox:    ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{}/{} slots", health.sandbox_active, health.sandbox_max)),
        ]),
    ];

    let conn_block = Paragraph::new(conn_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::SMOO_GREEN))
            .title("Connection"),
    );
    f.render_widget(conn_block, chunks[1]);

    // Keyboard shortcuts
    let shortcuts = vec![
        Line::from(vec![
            Span::styled("  Tab/Shift+Tab  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Switch tabs"),
        ]),
        Line::from(vec![
            Span::styled("  1-7            ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Jump to tab"),
        ]),
        Line::from(vec![
            Span::styled("  Click          ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Select tab"),
        ]),
        Line::from(vec![
            Span::styled("  q / Ctrl+C     ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Quit"),
        ]),
    ];

    let shortcuts_block = Paragraph::new(shortcuts).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::SMOO_GREEN))
            .title("Keyboard Shortcuts"),
    );
    f.render_widget(shortcuts_block, chunks[2]);
}

fn format_uptime(secs: f64) -> String {
    let s = secs as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

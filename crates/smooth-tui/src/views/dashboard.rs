//! Dashboard view — system overview.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::theme;

/// Health data for rendering.
pub struct HealthData {
    pub leader_status: String,
    pub leader_uptime: f64,
    pub db_status: String,
    pub db_path: String,
    pub sandbox_status: String,
    pub sandbox_active: u32,
    pub sandbox_max: u32,
    pub tailscale_status: String,
    pub tailscale_hostname: Option<String>,
    pub beads_status: String,
    pub beads_open: u32,
}

impl Default for HealthData {
    fn default() -> Self {
        Self {
            leader_status: "unknown".into(),
            leader_uptime: 0.0,
            db_status: "unknown".into(),
            db_path: String::new(),
            sandbox_status: "unknown".into(),
            sandbox_active: 0,
            sandbox_max: 3,
            tailscale_status: "disconnected".into(),
            tailscale_hostname: None,
            beads_status: "unknown".into(),
            beads_open: 0,
        }
    }
}

pub fn render(f: &mut Frame, area: Rect, health: &HealthData) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(7), Constraint::Min(0)])
        .split(area);

    // Title
    let title = Paragraph::new(Line::from(vec![Span::styled("Dashboard", theme::title())]));
    f.render_widget(title, chunks[0]);

    // Health cards
    let status_icon = |s: &str| {
        if s == "healthy" || s == "connected" {
            "●"
        } else if s == "degraded" {
            "◐"
        } else {
            "○"
        }
    };

    let health_lines = vec![
        Line::from(vec![
            Span::styled(format!(" {} ", status_icon(&health.leader_status)), theme::status_style(&health.leader_status)),
            Span::styled("Leader: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{} (uptime: {}s)", health.leader_status, health.leader_uptime as u64)),
        ]),
        Line::from(vec![
            Span::styled(format!(" {} ", status_icon(&health.db_status)), theme::status_style(&health.db_status)),
            Span::styled("Database: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&health.db_status),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" {} ", status_icon(&health.sandbox_status)),
                theme::status_style(&health.sandbox_status),
            ),
            Span::styled("Sandbox: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{}/{}", health.sandbox_active, health.sandbox_max)),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" {} ", status_icon(&health.tailscale_status)),
                theme::status_style(&health.tailscale_status),
            ),
            Span::styled("Tailscale: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(health.tailscale_hostname.as_deref().unwrap_or("disconnected")),
        ]),
        Line::from(vec![
            Span::styled(format!(" {} ", status_icon(&health.beads_status)), theme::status_style(&health.beads_status)),
            Span::styled("Beads: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{} open", health.beads_open)),
        ]),
    ];

    let health_block = Paragraph::new(health_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::SMOO_GREEN))
            .title("System Health"),
    );
    f.render_widget(health_block, chunks[1]);
}

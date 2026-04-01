//! Beads view — issue tracker overview.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Frame;

use crate::theme;

/// Bead data for rendering.
#[derive(Debug, Clone, Default)]
pub struct Bead {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: String,
}

/// Beads panel state.
pub struct BeadsState {
    pub beads: Vec<Bead>,
    pub loading: bool,
}

impl Default for BeadsState {
    fn default() -> Self {
        Self {
            beads: Vec::new(),
            loading: true,
        }
    }
}

pub fn render(f: &mut Frame, area: Rect, state: &BeadsState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled("Beads", theme::title())));
    f.render_widget(title, chunks[0]);

    if state.loading && state.beads.is_empty() {
        let loading = Paragraph::new(Line::from(Span::styled("Loading beads...", theme::muted())));
        f.render_widget(loading, chunks[1]);
        return;
    }

    if state.beads.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled("No open beads. Create one with: bd create --title=\"...\"", theme::muted())));
        f.render_widget(empty, chunks[1]);
        return;
    }

    let header = Row::new(vec!["ID", "Title", "Status", "Priority"])
        .style(Style::default().fg(theme::SMOO_GREEN).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = state
        .beads
        .iter()
        .map(|b| {
            let status_style = match b.status.as_str() {
                "in_progress" => Style::default().fg(theme::SMOO_ORANGE),
                "open" => Style::default().fg(theme::SMOO_GREEN),
                _ => theme::muted(),
            };
            Row::new(vec![
                Span::styled(&b.id, theme::muted()),
                Span::raw(&b.title),
                Span::styled(&b.status, status_style),
                Span::raw(&b.priority),
            ])
        })
        .collect();

    let table = Table::new(rows, [Constraint::Length(8), Constraint::Min(30), Constraint::Length(14), Constraint::Length(10)])
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::SMOO_GREEN))
                .title("Open Issues"),
        );
    f.render_widget(table, chunks[1]);
}

//! Operators view — active Smooth Operators.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Frame;

use crate::theme;

/// Operator data for rendering.
#[derive(Debug, Clone, Default)]
pub struct Operator {
    pub id: String,
    pub bead_id: String,
    pub status: String,
    pub runtime: String,
}

/// Operators panel state.
pub struct OperatorsState {
    pub operators: Vec<Operator>,
}

impl Default for OperatorsState {
    fn default() -> Self {
        Self { operators: Vec::new() }
    }
}

pub fn render(f: &mut Frame, area: Rect, state: &OperatorsState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled("Smooth Operators", theme::title())));
    f.render_widget(title, chunks[0]);

    if state.operators.is_empty() {
        let empty = Paragraph::new(vec![
            Line::from(Span::styled("No active operators.", theme::muted())),
            Line::default(),
            Line::from(Span::styled("Start one with: th run <bead-id>", theme::muted())),
        ]);
        f.render_widget(empty, chunks[1]);
        return;
    }

    let header = Row::new(vec!["ID", "Bead", "Status", "Runtime"])
        .style(Style::default().fg(theme::SMOO_GREEN).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = state
        .operators
        .iter()
        .map(|op| {
            let status_style = match op.status.as_str() {
                "running" => Style::default().fg(theme::SMOO_GREEN),
                "paused" => Style::default().fg(theme::SMOO_ORANGE),
                _ => theme::muted(),
            };
            Row::new(vec![
                Span::styled(&op.id, theme::muted()),
                Span::raw(&op.bead_id),
                Span::styled(&op.status, status_style),
                Span::raw(&op.runtime),
            ])
        })
        .collect();

    let table = Table::new(rows, [Constraint::Length(10), Constraint::Length(12), Constraint::Length(10), Constraint::Length(10)])
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::SMOO_GREEN))
                .title("Active Operators"),
        );
    f.render_widget(table, chunks[1]);
}

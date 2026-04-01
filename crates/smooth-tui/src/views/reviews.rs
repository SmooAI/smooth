//! Reviews view — pending code reviews from operators.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme;

/// Review data for rendering.
#[derive(Debug, Clone, Default)]
pub struct Review {
    pub bead_id: String,
    pub title: String,
    pub status: String,
}

/// Reviews panel state.
pub struct ReviewsState {
    pub reviews: Vec<Review>,
}

impl Default for ReviewsState {
    fn default() -> Self {
        Self { reviews: Vec::new() }
    }
}

pub fn render(f: &mut Frame, area: Rect, state: &ReviewsState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled("Reviews", theme::title())));
    f.render_widget(title, chunks[0]);

    if state.reviews.is_empty() {
        let empty = Paragraph::new(vec![
            Line::from(Span::styled("No pending reviews.", theme::muted())),
            Line::default(),
            Line::from(Span::styled("When operators complete work, their changes appear here for approval.", theme::muted())),
        ]);
        f.render_widget(empty, chunks[1]);
        return;
    }

    let lines: Vec<Line> = state
        .reviews
        .iter()
        .flat_map(|r| {
            vec![
                Line::from(vec![
                    Span::styled(format!("[{}] ", r.bead_id), theme::muted()),
                    Span::styled(&r.title, theme::subtitle()),
                ]),
                Line::from(Span::styled(format!("  Status: {}", r.status), theme::muted())),
                Line::default(),
            ]
        })
        .collect();

    let reviews = Paragraph::new(lines);
    f.render_widget(reviews, chunks[1]);
}

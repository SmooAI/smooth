//! Messages view — inbox for operator notifications.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme;

/// Message data for rendering.
#[derive(Debug, Clone, Default)]
pub struct Message {
    pub from: String,
    pub content: String,
    pub timestamp: String,
}

/// Messages panel state.
pub struct MessagesState {
    pub messages: Vec<Message>,
}

impl Default for MessagesState {
    fn default() -> Self {
        Self { messages: Vec::new() }
    }
}

pub fn render(f: &mut Frame, area: Rect, state: &MessagesState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled("Messages", theme::title())));
    f.render_widget(title, chunks[0]);

    if state.messages.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled("Inbox empty — no messages from operators.", theme::muted())));
        f.render_widget(empty, chunks[1]);
        return;
    }

    let lines: Vec<Line> = state
        .messages
        .iter()
        .flat_map(|m| {
            vec![
                Line::from(vec![
                    Span::styled(&m.from, theme::subtitle()),
                    Span::styled(format!("  {}", m.timestamp), theme::muted()),
                ]),
                Line::from(Span::raw(&m.content)),
                Line::default(),
            ]
        })
        .collect();

    let msgs = Paragraph::new(lines);
    f.render_widget(msgs, chunks[1]);
}

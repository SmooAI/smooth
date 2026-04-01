//! Chat view — message input + history with markdown rendering.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::markdown;
use crate::theme;

/// A chat message.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Chat state.
pub struct ChatState {
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub streaming: bool,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            messages: vec![ChatMessage {
                role: "assistant".into(),
                content: "Welcome to Smooth. How can I help?".into(),
            }],
            input: String::new(),
            streaming: false,
        }
    }
}

pub fn render(f: &mut Frame, area: Rect, state: &ChatState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(5), Constraint::Length(3)])
        .split(area);

    // Title
    let title = Paragraph::new(Line::from(Span::styled("Chat with Leader", theme::title())));
    f.render_widget(title, chunks[0]);

    // Messages
    let mut msg_lines: Vec<Line> = Vec::new();
    for msg in &state.messages {
        let (label, label_style) = match msg.role.as_str() {
            "user" => ("You: ", Style::default().fg(theme::SMOO_ORANGE).add_modifier(Modifier::BOLD)),
            "assistant" => ("Smooth: ", Style::default().fg(theme::SMOO_GREEN).add_modifier(Modifier::BOLD)),
            _ => ("", Style::default()),
        };

        msg_lines.push(Line::from(Span::styled(label, label_style)));

        if msg.role == "assistant" {
            let rendered = markdown::render(&msg.content);
            msg_lines.extend(rendered.lines.into_iter().map(|l| l.into()));
        } else {
            msg_lines.push(Line::from(Span::raw(&msg.content)));
        }
        msg_lines.push(Line::default());
    }

    if state.streaming {
        msg_lines.push(Line::from(Span::styled("Thinking...", theme::muted())));
    }

    let messages = Paragraph::new(msg_lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::LEFT).border_style(Style::default().fg(theme::SMOO_GREEN)));
    f.render_widget(messages, chunks[1]);

    // Input
    let input_text = if state.input.is_empty() {
        "Message the leader... (@ for context search)"
    } else {
        &state.input
    };
    let input_style = if state.input.is_empty() { theme::muted() } else { Style::default() };
    let input = Paragraph::new(Line::from(Span::styled(input_text, input_style))).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::SMOO_GREEN))
            .title("Input"),
    );
    f.render_widget(input, chunks[2]);
}

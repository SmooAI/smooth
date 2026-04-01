//! Chat view — message input + history with markdown rendering.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::markdown;
use crate::theme;

/// A chat message.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// An autocomplete search result.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub result_type: String,
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
}

/// Chat state.
pub struct ChatState {
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub streaming: bool,
    pub scroll_offset: u16,
    /// @ autocomplete state
    pub autocomplete: AutocompleteState,
}

/// Autocomplete popup state.
pub struct AutocompleteState {
    /// Whether the popup is visible.
    pub active: bool,
    /// Current search results.
    pub results: Vec<SearchResult>,
    /// Selected index in results.
    pub selected: usize,
    /// The query text after @.
    pub query: String,
    /// Position of the @ in the input string.
    pub at_pos: Option<usize>,
}

impl Default for AutocompleteState {
    fn default() -> Self {
        Self {
            active: false,
            results: Vec::new(),
            selected: 0,
            query: String::new(),
            at_pos: None,
        }
    }
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
            scroll_offset: 0,
            autocomplete: AutocompleteState::default(),
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

    // Auto-scroll to bottom: calculate total lines after wrapping
    let visible_height = chunks[1].height.saturating_sub(1); // account for border
    let content_width = chunks[1].width.saturating_sub(2) as usize; // account for borders
    let total_lines: u16 = msg_lines
        .iter()
        .map(|line| {
            let line_len: usize = line.spans.iter().map(|s| s.content.len()).sum();
            if line_len == 0 || content_width == 0 {
                1
            } else {
                ((line_len + content_width - 1) / content_width) as u16
            }
        })
        .sum();

    // Auto-scroll to show latest messages
    let scroll = if total_lines > visible_height { total_lines - visible_height } else { 0 };
    // Use manual offset if user scrolled up, otherwise auto-scroll
    let effective_scroll = if state.scroll_offset > 0 { state.scroll_offset } else { scroll };

    let messages = Paragraph::new(msg_lines)
        .wrap(Wrap { trim: false })
        .scroll((effective_scroll, 0))
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

    // Autocomplete popup (rendered on top of messages area)
    if state.autocomplete.active && !state.autocomplete.results.is_empty() {
        let max_items = state.autocomplete.results.len().min(8);
        let popup_height = (max_items as u16) + 2; // +2 for borders
        let popup_width = area.width.min(60);

        // Position popup above the input box
        let popup_y = chunks[2].y.saturating_sub(popup_height);
        let popup_x = chunks[2].x + 1;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear the area behind the popup
        f.render_widget(Clear, popup_area);

        let items: Vec<Line> = state
            .autocomplete
            .results
            .iter()
            .enumerate()
            .take(max_items)
            .map(|(i, r)| {
                let icon = match r.result_type.as_str() {
                    "bead" => "◉ ",
                    "file" => "◇ ",
                    "path" => "▸ ",
                    _ => "  ",
                };
                let detail = r.detail.as_deref().unwrap_or("");
                let is_selected = i == state.autocomplete.selected;
                let style = if is_selected {
                    Style::default().fg(theme::SMOO_ORANGE).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(vec![
                    Span::styled(icon, if is_selected { style } else { theme::muted() }),
                    Span::styled(&r.label, style),
                    Span::styled(format!("  {detail}"), theme::muted()),
                ])
            })
            .collect();

        let popup = Paragraph::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::SMOO_ORANGE))
                .title(Span::styled(" @ Search ", Style::default().fg(theme::SMOO_ORANGE))),
        );
        f.render_widget(popup, popup_area);
    }
}

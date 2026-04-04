//! Main render function — draws the full TUI frame.

use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::layout::compute_layout;
use crate::state::{AppState, ChatRole};
use crate::theme;

/// Render the full TUI frame from the current application state.
pub fn render(frame: &mut Frame, state: &AppState) {
    let regions = compute_layout(frame.area(), state.sidebar_visible);

    render_chat(frame, state, regions.chat);
    render_input(frame, state, regions.input);
    render_status(frame, state, regions.status);

    if let Some(sidebar_rect) = regions.sidebar {
        render_sidebar(frame, sidebar_rect);
    }
}

/// Render the chat message area.
fn render_chat(frame: &mut Frame, state: &AppState, area: Rect) {
    let block = Block::default()
        .title(Span::styled(" Smooth Coding ", theme::title()))
        .borders(Borders::ALL)
        .border_style(theme::muted());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line<'_>> = Vec::new();

    for msg in &state.messages {
        let (label, label_style) = match msg.role {
            ChatRole::User => ("You", theme::user_label()),
            ChatRole::Assistant => ("Smooth", theme::assistant_label()),
            ChatRole::System => ("System", theme::muted()),
        };

        // Role label line
        lines.push(Line::from(Span::styled(format!("{label}:"), label_style)));

        // Content lines
        for content_line in msg.content.lines() {
            lines.push(Line::from(Span::raw(content_line.to_string())));
        }

        // Blank line between messages
        lines.push(Line::from(""));
    }

    // Thinking indicator
    if state.thinking {
        lines.push(Line::from(Span::styled("Thinking...", theme::muted())));
    }

    // Calculate scroll: show the bottom of the conversation
    let visible_height = inner.height as usize;
    let total_lines = lines.len();
    let scroll = if total_lines > visible_height {
        (total_lines - visible_height).saturating_sub(state.scroll_offset)
    } else {
        0
    };

    let paragraph = Paragraph::new(lines)
        .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, inner);
}

/// Render the text input area.
fn render_input(frame: &mut Frame, state: &AppState, area: Rect) {
    let block = Block::default().title(" Message ").borders(Borders::ALL).border_style(theme::input_style());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let input_text = Paragraph::new(state.input.as_str()).style(theme::input_style());
    frame.render_widget(input_text, inner);

    // Position cursor
    let cursor_x = inner.x + u16::try_from(state.input_cursor).unwrap_or(0);
    let cursor_y = inner.y;
    if cursor_x < inner.x + inner.width {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Render the bottom status bar.
fn render_status(frame: &mut Frame, state: &AppState, area: Rect) {
    let status_text = format!(" {} | tokens: {} | Ctrl+C quit | Ctrl+B sidebar ", state.model_name, state.total_tokens);

    let paragraph = Paragraph::new(status_text).style(theme::status_style()).alignment(Alignment::Left);

    frame.render_widget(paragraph, area);
}

/// Render the sidebar panel (placeholder for now).
fn render_sidebar(frame: &mut Frame, area: Rect) {
    let block = Block::default().title(" Context ").borders(Borders::ALL).border_style(theme::muted());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items: Vec<ListItem<'_>> = vec![ListItem::new(Span::styled("(no context files)", theme::muted()))];

    let list = List::new(items);
    frame.render_widget(Clear, inner);
    frame.render_widget(list, inner);
}

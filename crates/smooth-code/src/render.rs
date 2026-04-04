//! Main render function — draws the full TUI frame.

use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::layout::compute_layout;
use crate::state::{AppState, ChatRole, ToolStatus};
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
        let content_lines_vec: Vec<&str> = msg.content.lines().collect();
        let last_content_idx = content_lines_vec.len().saturating_sub(1);
        for (i, content_line) in content_lines_vec.iter().enumerate() {
            if msg.streaming && !msg.content.is_empty() && i == last_content_idx {
                // Append blinking block cursor to the last line of a streaming message
                lines.push(Line::from(vec![
                    Span::raw(content_line.to_string()),
                    Span::styled("█", theme::assistant_label()),
                ]));
            } else {
                lines.push(Line::from(Span::raw(content_line.to_string())));
            }
        }

        // Tool call blocks (only rendered for assistant messages with tool calls)
        for tc in &msg.tool_calls {
            let (icon, icon_style) = match tc.status {
                ToolStatus::Pending => ("⏳", theme::muted()),
                ToolStatus::Running => ("⚙", theme::user_label()),
                ToolStatus::Done => ("✓", theme::success()),
                ToolStatus::Error => ("✗", theme::error()),
            };

            #[allow(clippy::cast_precision_loss)]
            let duration_str = tc.duration_ms.map_or_else(String::new, |ms| {
                let secs = ms as f64 / 1000.0;
                format!(" ({secs:.1}s)")
            });

            let status_label = match tc.status {
                ToolStatus::Pending => "pending...".to_string(),
                ToolStatus::Running => "running...".to_string(),
                ToolStatus::Done => format!("done{duration_str}"),
                ToolStatus::Error => format!("error{duration_str}"),
            };

            let collapse_indicator = if tc.output.is_some() {
                if tc.collapsed {
                    " ▶"
                } else {
                    " ▼"
                }
            } else {
                ""
            };

            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), icon_style),
                Span::styled(format!("{}(\"{}\")", tc.tool_name, tc.arguments_preview), theme::muted()),
                Span::raw(format!(" ── {status_label}{collapse_indicator}")),
            ]));

            // Show output if not collapsed
            if !tc.collapsed {
                if let Some(ref output) = tc.output {
                    for output_line in output.lines() {
                        lines.push(Line::from(Span::styled(format!("  │ {output_line}"), theme::muted())));
                    }
                }
            }
        }

        // Blank line between messages
        lines.push(Line::from(""));
    }

    // Streaming indicator — spinner when waiting for first token
    // When streaming with content, the blinking block cursor is appended
    // to the last content line above — handled in the content rendering loop.
    if let Some(last_msg) = state.messages.last() {
        if last_msg.streaming && last_msg.content.is_empty() {
            let spinner = state.spinner_char();
            lines.push(Line::from(Span::styled(format!("{spinner} Generating..."), theme::muted())));
        }
    }

    // Thinking indicator (non-streaming fallback)
    if state.thinking && state.messages.last().is_none_or(|m| !m.streaming) {
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
    let branch_indicator = state
        .git_state
        .as_ref()
        .filter(|g| g.is_repo)
        .map_or(String::new(), |g| format!("{} \u{2387} | ", g.branch));

    let status_text = format!(
        " {branch_indicator}{} | tokens: {} | Ctrl+C quit | Ctrl+B sidebar ",
        state.model_name, state.total_tokens
    );

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

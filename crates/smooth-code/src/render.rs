//! Main render function — draws the full TUI frame.

use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::layout::compute_layout;
use crate::state::{AppState, ChatRole, FocusPanel, ToolStatus};
use crate::theme;

/// Render the full TUI frame from the current application state.
pub fn render(frame: &mut Frame, state: &AppState) {
    let regions = compute_layout(frame.area(), state.sidebar_visible);

    render_chat(frame, state, regions.chat);
    render_input(frame, state, regions.input);
    render_status(frame, state, regions.status);

    if let Some(sidebar_rect) = regions.sidebar {
        render_sidebar(frame, state, sidebar_rect);
    }

    if state.model_picker.active {
        render_model_picker(frame, state, frame.area());
    }
}

/// The ASCII art banner rows for the welcome screen.
const BANNER_ROWS: [&str; 6] = [
    " \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2588}\u{2557}   \u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2557}",
    " \u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{255a}\u{2550}\u{2550}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{255d}\u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2551}",
    " \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}",
    " \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}\u{255a}\u{2588}\u{2588}\u{2554}\u{255d}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2551}",
    " \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551} \u{255a}\u{2550}\u{255d} \u{2588}\u{2588}\u{2551}\u{255a}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d}\u{255a}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d}   \u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2551}",
    " \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}\u{255a}\u{2550}\u{255d}     \u{255a}\u{2550}\u{255d} \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}  \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}    \u{255a}\u{2550}\u{255d}   \u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d}",
];

/// Render the welcome banner with gradient colors when there are no messages.
fn render_welcome_banner(lines: &mut Vec<Line<'_>>) {
    let total_rows = BANNER_ROWS.len();
    // Add a blank line at top for spacing
    lines.push(Line::from(""));
    for (i, row) in BANNER_ROWS.iter().enumerate() {
        let style = theme::gradient_row(i, total_rows);
        lines.push(Line::from(Span::styled(*row, style)).alignment(Alignment::Center));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("AI Agent Orchestration Platform", theme::muted())).alignment(Alignment::Center));
    lines.push(Line::from(Span::styled("smoo.ai", Style::default().fg(theme::SMOO_GRAY_500))).alignment(Alignment::Center));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Type a message to get started. /help for commands.", theme::muted())).alignment(Alignment::Center));
    lines.push(Line::from(""));
}

/// Render the chat message area.
fn render_chat(frame: &mut Frame, state: &AppState, area: Rect) {
    let block = Block::default()
        .title(Span::styled(" Smooth Coding ", theme::title()))
        .borders(Borders::ALL)
        .border_style(theme::panel_border(state.focus == FocusPanel::Chat));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Show welcome banner when there are no messages
    if state.messages.is_empty() && !state.thinking {
        render_welcome_banner(&mut lines);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, inner);
        return;
    }

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
    let block = Block::default()
        .title(" Message ")
        .borders(Borders::ALL)
        .border_style(theme::panel_border(state.focus == FocusPanel::Input));

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

/// Render the sidebar panel with the file tree.
fn render_sidebar(frame: &mut Frame, state: &AppState, area: Rect) {
    let block = Block::default()
        .title(" Files ")
        .borders(Borders::ALL)
        .border_style(theme::panel_border(state.focus == FocusPanel::Sidebar));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Clear, inner);

    let Some(file_tree) = &state.file_tree else {
        let items: Vec<ListItem<'_>> = vec![ListItem::new(Span::styled("(no files)", theme::muted()))];
        let list = List::new(items);
        frame.render_widget(list, inner);
        return;
    };

    let height = inner.height as usize;
    if height == 0 || file_tree.entries.is_empty() {
        return;
    }

    // Calculate the visible window manually (read-only, no mutation).
    let scroll = file_tree.scroll_offset;
    let selected = file_tree.selected;

    // Determine effective scroll offset (same logic as visible_entries but without &mut).
    let eff_scroll = if selected >= scroll + height {
        selected + 1 - height
    } else if selected < scroll {
        selected
    } else {
        scroll
    };

    let end = (eff_scroll + height).min(file_tree.entries.len());
    let visible = &file_tree.entries[eff_scroll..end];

    let items: Vec<ListItem<'_>> = visible
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let global_idx = eff_scroll + i;
            let indent = "  ".repeat(entry.depth);

            let prefix = if entry.is_dir { "\u{25b8} " } else { "  " };

            let text = format!("{indent}{prefix}{}", entry.name);

            if global_idx == file_tree.selected {
                ListItem::new(Span::styled(text, Style::default().bg(theme::SMOO_GREEN).fg(ratatui::style::Color::Black)))
            } else if entry.is_dir {
                ListItem::new(Span::styled(text, theme::title()))
            } else {
                // Color file name by extension
                let ext = entry.name.rsplit('.').next().unwrap_or("");
                let color = theme::file_color(ext);
                ListItem::new(Span::styled(text, Style::default().fg(color)))
            }
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, inner);
}

/// Render the model picker as a centered popup overlay.
fn render_model_picker(frame: &mut Frame, state: &AppState, area: Rect) {
    let picker = &state.model_picker;

    // Size the popup: width ~50 cols, height = options + 2 (border)
    let popup_width = 50.min(area.width.saturating_sub(4));
    #[allow(clippy::cast_possible_truncation)]
    let provider_count = picker.providers.len().min(usize::from(u16::MAX) - 2) as u16;
    let popup_height = (provider_count + 2).min(area.height.saturating_sub(2));

    let [popup_y] = Layout::vertical([Constraint::Length(popup_height)]).flex(Flex::Center).areas(area);
    let [popup_area] = Layout::horizontal([Constraint::Length(popup_width)]).flex(Flex::Center).areas(popup_y);

    // Clear the area behind the popup
    frame.render_widget(Clear, popup_area);

    let block = Block::default().title(" Select Model ").borders(Borders::ALL).border_style(theme::title());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let items: Vec<ListItem<'_>> = picker
        .providers
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let prefix = if i == picker.selected { "▸ " } else { "  " };
            let provider_tag = format!(" ({})", opt.provider);
            let text = format!("{prefix}{}{provider_tag}", opt.display_name);

            if i == picker.selected {
                ListItem::new(Span::styled(
                    text,
                    ratatui::style::Style::default().bg(theme::SMOO_GREEN).fg(ratatui::style::Color::Black),
                ))
            } else {
                ListItem::new(Span::raw(text))
            }
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, inner);
}

//! Main render function — draws the full TUI frame.

use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::state::{AppState, ChatRole, FocusPanel, HealthStatus, ToolStatus};
use crate::theme;

/// Render the full TUI frame from the current application state.
///
/// Inline-viewport mode: the frame area is just the small bottom
/// region the TUI owns (preview + status + input). Finalized chat
/// messages live in the terminal's own scrollback above the viewport
/// and are NOT rendered here — `app.rs` calls
/// [`crate::inline::flush_to_scrollback`] every tick to push them
/// out of `state.messages` once they stop streaming.
pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    // The preview area shows the streaming/in-flight assistant
    // message. Cap at half the viewport height so a runaway response
    // doesn't crowd out the input/status; once it overflows, the
    // wrap-aware Paragraph keeps the most recent rows visible.
    let max_preview = area.height.saturating_sub(4).max(1);
    let preview_h = crate::inline::preview_height(state, area.width, max_preview);
    let regions = crate::inline::compute_regions(area, preview_h);

    if let Some(preview_rect) = regions.preview {
        let lines = crate::inline::viewport_preview_lines(state);
        let total_lines = lines.len();
        let visible = preview_rect.height as usize;
        // When the preview is taller than its allotted region, scroll
        // to the bottom so the most recent tokens are always visible
        // — same behavior the user expects from a terminal that's
        // mid-output.
        let scroll = total_lines.saturating_sub(visible);
        let paragraph = Paragraph::new(lines)
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, preview_rect);
    }

    render_input(frame, state, regions.input);
    render_status(frame, state, regions.status);

    if state.autocomplete.active && !state.autocomplete.results.is_empty() {
        render_autocomplete_popup(frame, state, regions.input);
    }

    if state.model_picker.active {
        render_model_picker(frame, state, area);
    }
}

/// Render the autocomplete popup directly above the input box.
/// Shows up to 8 rows; stays narrow (40 cols) so it doesn't cover
/// the chat content behind it.
fn render_autocomplete_popup(frame: &mut Frame, state: &AppState, input_area: Rect) {
    use crate::autocomplete::CompletionKind;

    let popup_height = (state.autocomplete.results.len() as u16).min(8) + 2; // +2 for borders
    let popup_width = 48u16.min(input_area.width);
    if popup_height == 0 || popup_width == 0 || input_area.y < popup_height {
        return;
    }

    let popup_rect = Rect {
        x: input_area.x,
        y: input_area.y - popup_height,
        width: popup_width,
        height: popup_height,
    };

    let title = match state.autocomplete.kind {
        CompletionKind::File => " @ File ",
        CompletionKind::Command => " / Command ",
    };

    let items: Vec<ListItem<'_>> = state
        .autocomplete
        .results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let is_selected = i == state.autocomplete.selected;
            let marker = if is_selected { "▶ " } else { "  " };
            let label_style = if is_selected {
                theme::user_label() // orange bold
            } else {
                Style::default().fg(theme::SMOO_WHITE)
            };
            let detail_style = theme::muted();
            let line = Line::from(vec![
                Span::raw(marker),
                Span::styled(r.label.clone(), label_style),
                Span::raw("  "),
                Span::styled(r.detail.clone(), detail_style),
            ]);
            ListItem::new(line)
        })
        .collect();

    let block = Block::default()
        .title(Span::styled(title, theme::title()))
        .borders(Borders::ALL)
        .border_style(theme::panel_border(true));

    let list = List::new(items).block(block);
    frame.render_widget(Clear, popup_rect);
    frame.render_widget(list, popup_rect);
}

/// Build the gradient SMOOTH wordmark welcome banner as styled
/// lines.
///
/// Used by the inline TUI at session start: `app::run` calls this
/// once for fresh sessions and pushes the lines into terminal
/// scrollback via [`crate::inline::insert_before_lines`], so the
/// banner sits at the top of the session like a real terminal
/// program's startup banner.
#[must_use]
pub fn welcome_banner_lines() -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    welcome_banner_into(&mut lines);
    lines
}

/// Internal helper that appends the banner lines into a caller-
/// provided buffer. Kept separate from [`welcome_banner_lines`] so
/// future callers that want to weave the banner in with other
/// content (e.g. a fixed-screen mode rebuild) don't have to
/// allocate twice.
fn welcome_banner_into(lines: &mut Vec<Line<'static>>) {
    /// Box-drawing SMOOTH wordmark — figlet "ANSI Shadow" font. Solid
    /// blocks with double-line outlines; reads as a chunky brand
    /// statement when colored with the smooth-web logo gradient.
    ///
    /// Stored as `(smoo_chunk, th_chunk)` tuples — the explicit split
    /// means each half gets its own gradient applied uniformly across
    /// only its own letters, with no boundary math. If the banner art
    /// is ever swapped, the split point is right here in source for
    /// review (no fraction-of-width to recompute).
    const BANNER_ROWS: [(&str, &str); 6] = [
        (
            " \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2588}\u{2557}   \u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}",
            " \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2557}",
        ),
        (
            " \u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}",
            "\u{255a}\u{2550}\u{2550}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{255d}\u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2551}",
        ),
        (
            " \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}",
            "   \u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}",
        ),
        (
            " \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}\u{255a}\u{2588}\u{2588}\u{2554}\u{255d}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}",
            "   \u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2551}",
        ),
        (
            " \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551} \u{255a}\u{2550}\u{255d} \u{2588}\u{2588}\u{2551}\u{255a}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d}\u{255a}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d}",
            "   \u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2551}",
        ),
        (
            " \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}\u{255a}\u{2550}\u{255d}     \u{255a}\u{2550}\u{255d} \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}  \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}",
            "    \u{255a}\u{2550}\u{255d}   \u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d}",
        ),
    ];
    // Blank line at the top for spacing
    lines.push(Line::from(""));
    // Each row is split into a (smoo, th) pair. The Smoo chunk gets
    // the orange→coral→pink gradient over only its own length; the
    // th chunk gets teal→blue over its own. This is structural —
    // no boundary math, no fraction of total width — so the
    // gradients always fit their letters exactly regardless of how
    // wide the banner is or how the split lands.
    for (smoo_chunk, th_chunk) in &BANNER_ROWS {
        let smoo_chars: Vec<char> = smoo_chunk.chars().collect();
        let th_chars: Vec<char> = th_chunk.chars().collect();
        let smoo_total = smoo_chars.len().max(1);
        let th_total = th_chars.len().max(1);

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(smoo_chars.len() + th_chars.len());
        for (i, ch) in smoo_chars.into_iter().enumerate() {
            let color = theme::smoo_gradient_color(i, smoo_total);
            spans.push(Span::styled(ch.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)));
        }
        for (i, ch) in th_chars.into_iter().enumerate() {
            let color = theme::th_gradient_color(i, th_total);
            spans.push(Span::styled(ch.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)));
        }
        lines.push(Line::from(spans).alignment(Alignment::Center));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("AI Agent Orchestration Platform", theme::muted())).alignment(Alignment::Center));
    lines.push(Line::from(Span::styled("smoo.ai", Style::default().fg(theme::SMOO_GRAY_500))).alignment(Alignment::Center));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Type a message to get started. /help for commands.", theme::muted())).alignment(Alignment::Center));
    lines.push(Line::from(""));
}

/// Render the chat message area.
#[allow(dead_code)] // superseded by inline-viewport rendering; kept for reference
fn render_chat(frame: &mut Frame, state: &AppState, area: Rect) {
    // Build a " Smooth " title where "Smooth" uses the brand gradient
    // (matches the CLI wordmark — `smoo` orange→pink, `th` teal→blue).
    // No box border — terminal text selection drags rectangular cells,
    // so any border glyphs (│ ─ etc.) leak into the copy buffer when
    // the user drags across the chat history. Role labels + blank-line
    // spacing carry enough visual structure on their own.
    let mut title_spans: Vec<Span<'_>> = vec![Span::raw(" ")];
    title_spans.extend(theme::smooth_wordmark());
    title_spans.push(Span::raw(" "));

    // Render the title as the first line of the area; the rest is body.
    // The Paragraph below scrolls within `inner`, leaving the title
    // pinned to the top.
    let title_line = Line::from(title_spans);
    let title_rect = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1.min(area.height),
    };
    frame.render_widget(Paragraph::new(title_line), title_rect);
    let inner = Rect {
        x: area.x,
        y: area.y.saturating_add(1),
        width: area.width,
        height: area.height.saturating_sub(1),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Show welcome banner when there are no messages
    if state.messages.is_empty() && !state.thinking {
        welcome_banner_into(&mut lines);
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

        // Content lines. Assistant content is markdown — bold, italic,
        // inline code, fenced code, headings, lists. User and system
        // messages are plain text and stay raw to avoid surprising
        // formatting on what the user typed.
        let mut content_lines: Vec<Line<'_>> = if matches!(msg.role, ChatRole::Assistant) && !msg.content.is_empty() {
            crate::markdown::render(&msg.content)
        } else {
            msg.content.lines().map(|l| Line::from(Span::raw(l.to_string()))).collect()
        };
        if content_lines.is_empty() {
            content_lines.push(Line::from(""));
        }

        if msg.streaming && !msg.content.is_empty() {
            // Append a blinking block cursor at the end of the last
            // content line so the user can see the response is still
            // arriving. Owned spans only — append to the last line in
            // place.
            if let Some(last) = content_lines.last_mut() {
                last.spans.push(Span::styled("█", theme::assistant_label()));
            }
        }
        lines.append(&mut content_lines);

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
    // Orange-bold "▶ Message" title + orange border so it's the
    // obvious place to type. Stays orange even when the chat panel
    // is focused — there's only one thing to do in this surface
    // (type) and we want it findable at a glance.
    let title_line = Line::from(vec![Span::styled(" ▶ ", theme::title()), Span::styled("Message ", theme::title())]);
    let block = Block::default()
        .title(title_line)
        .borders(Borders::ALL)
        .border_style(theme::input_border(state.mode));

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

    let (health_dot, health_style) = match state.health_status {
        HealthStatus::Healthy => ("\u{25cf}", Style::default().fg(theme::SUCCESS_GREEN)),
        HealthStatus::Warnings(_) => ("\u{25cf}", Style::default().fg(theme::SMOO_ORANGE)),
        HealthStatus::Unknown => ("\u{25cf}", Style::default().fg(theme::SMOO_GRAY_700)),
    };

    // When a workflow phase is active, prefix the status bar with
    // `<PHASE> · <alias> → <upstream>  |  <cycling phrase>`. The
    // thesaurus rotates every ~30 spinner ticks (~3 sec) so long
    // phases feel alive without spamming events.
    let phase_prefix = state
        .current_phase
        .as_deref()
        .map(|phase| {
            let alias = state.current_phase_alias.as_deref().unwrap_or("?");
            let upstream_suffix = state.current_phase_upstream.as_deref().map_or(String::new(), |u| format!(" → {u}"));
            let phrases = crate::thesaurus::phrases_for(phase);
            let phrase = phrases[(state.phrase_idx / 30) % phrases.len()];
            format!(" {phase} · {alias}{upstream_suffix} | {phrase} |")
        })
        .unwrap_or_default();

    let status_left = format!(
        "{phase_prefix} {branch_indicator}agent: {} | {} | tokens: {} | spend: {} | ",
        state.agent_name,
        state.model_name,
        state.total_tokens,
        format_spend(state.total_cost_usd),
    );
    let status_right = " | Ctrl+C quit ";

    let line = Line::from(vec![
        Span::styled(status_left, theme::status_style()),
        Span::styled(health_dot, health_style),
        Span::styled(status_right, theme::status_style()),
    ]);

    let paragraph = Paragraph::new(line).alignment(Alignment::Left);

    frame.render_widget(paragraph, area);
}

/// Render the sidebar panel with the file tree.
#[allow(dead_code)] // sidebar removed when switching to inline viewport (Ctrl+B unbound)
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
///
/// Two views, rendered off the same popup frame:
///   * `PickerView::Slots` — list of 8 routing slots with current model
///   * `PickerView::Models { slot }` — candidate models for that slot
fn render_model_picker(frame: &mut Frame, state: &AppState, area: Rect) {
    use crate::model_picker::PickerView;

    let picker = &state.model_picker;

    // Wider popup than the old list-only view so slot label + model +
    // description fit on one line.
    let popup_width = 72.min(area.width.saturating_sub(4));
    let row_count = match picker.view {
        PickerView::Slots => picker.slots.len(),
        PickerView::Models { .. } => picker.models.len(),
    };
    #[allow(clippy::cast_possible_truncation)]
    let body_rows = row_count.min(usize::from(u16::MAX) - 6) as u16;
    // +2 for outer border, +1 header, +1 footer
    let popup_height = (body_rows + 4).min(area.height.saturating_sub(2));

    let [popup_y] = Layout::vertical([Constraint::Length(popup_height)]).flex(Flex::Center).areas(area);
    let [popup_area] = Layout::horizontal([Constraint::Length(popup_width)]).flex(Flex::Center).areas(popup_y);

    frame.render_widget(Clear, popup_area);

    let title = match picker.view {
        PickerView::Slots => " Models — routing slots ".to_string(),
        PickerView::Models { slot } => {
            let label = picker.slots.iter().find(|e| e.slot == slot).map_or("?", |e| e.label);
            format!(" Models — {label} ")
        }
    };

    let block = Block::default().title(title).borders(Borders::ALL).border_style(theme::title());
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Layout inside the border: [body][footer]
    let [body_area, footer_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    match picker.view {
        PickerView::Slots => render_slots_view(frame, picker, body_area),
        PickerView::Models { .. } => render_models_view(frame, picker, body_area),
    }

    let footer = match picker.view {
        PickerView::Slots => "↑/↓ navigate  Enter pick slot  Esc close".to_string(),
        PickerView::Models { .. } => "↑/↓ navigate  Enter apply  Esc back".to_string(),
    };
    let footer_line = if let Some(err) = picker.error.as_ref() {
        format!("⚠ {err}")
    } else {
        footer
    };
    frame.render_widget(ratatui::widgets::Paragraph::new(footer_line).style(theme::muted()), footer_area);
}

fn render_slots_view(frame: &mut Frame, picker: &crate::model_picker::ModelPickerState, area: Rect) {
    let items: Vec<ListItem<'_>> = picker
        .slots
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let selected = i == picker.selected;
            let prefix = if selected { "▸ " } else { "  " };
            // One-line row: "Label        model-name        description"
            let text = format!("{prefix}{:<11} {}  —  {}", entry.label, entry.current_model, entry.description);
            if selected {
                ListItem::new(Span::styled(
                    text,
                    ratatui::style::Style::default().bg(theme::SMOO_ORANGE).fg(ratatui::style::Color::Black),
                ))
            } else {
                ListItem::new(Span::raw(text))
            }
        })
        .collect();
    frame.render_widget(List::new(items), area);
}

fn render_models_view(frame: &mut Frame, picker: &crate::model_picker::ModelPickerState, area: Rect) {
    let items: Vec<ListItem<'_>> = picker
        .models
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let selected = i == picker.selected;
            let prefix = if selected { "▸ " } else { "  " };
            let text = format!("{prefix}{}", m.display());
            if selected {
                ListItem::new(Span::styled(
                    text,
                    ratatui::style::Style::default().bg(theme::SMOO_ORANGE).fg(ratatui::style::Color::Black),
                ))
            } else {
                ListItem::new(Span::raw(text))
            }
        })
        .collect();
    frame.render_widget(List::new(items), area);
}

/// Format a spend total for the status bar.
pub fn format_spend(usd: f64) -> String {
    if usd <= 0.0 {
        "$0".to_string()
    } else if usd < 1.0 {
        format!("${usd:.3}")
    } else {
        format!("${usd:.2}")
    }
}

#[cfg(test)]
mod spend_fmt_tests {
    use super::format_spend;

    #[test]
    fn zero_is_short() {
        assert_eq!(format_spend(0.0), "$0");
        assert_eq!(format_spend(-0.001), "$0");
    }

    #[test]
    fn sub_dollar_has_three_decimals() {
        assert_eq!(format_spend(0.042), "$0.042");
        assert_eq!(format_spend(0.1), "$0.100");
    }

    #[test]
    fn dollar_plus_has_two_decimals() {
        assert_eq!(format_spend(1.0), "$1.00");
        assert_eq!(format_spend(12.345), "$12.35");
    }
}

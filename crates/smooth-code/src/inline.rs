//! Inline-viewport rendering helpers (Claude Code-style chat).
//!
//! Smooth's TUI used to run in alt-screen mode with a fixed
//! `Paragraph` that scrolled an in-app message buffer. That meant the
//! terminal's native wheel-scroll, drag-select, and search were all
//! disabled (the alt-screen replaces the scrollback). The new mode
//! uses ratatui's [`Viewport::Inline`]: the TUI owns only a small
//! region at the bottom of the terminal (input + status + an
//! optional streaming-preview area), and finalized messages flow
//! into the terminal's *own* scrollback via
//! [`Frame::insert_before`]. The user's terminal handles scroll,
//! selection, and copy natively.
//!
//! Two functions live here:
//! - [`message_lines`] — turn a single [`ChatMessage`] into styled
//!   ratatui [`Line`]s. Shared between the in-viewport preview render
//!   and the scrollback-flush path so the look is identical above
//!   and below the viewport boundary.
//! - [`flush_to_scrollback`] — push every finalized message that is
//!   still inside `state.messages` (index >= `committed_count`) into
//!   the terminal's scrollback. Skips the in-flight streaming
//!   message; that one renders inside the viewport until it finishes.

use anyhow::Result;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::Terminal;

use crate::state::{AppState, ChatMessage, ChatRole, ToolStatus};
use crate::theme;

/// Render a single chat message into a vector of styled lines.
///
/// Mirrors the per-message structure the old `render_chat` produced
/// (role label, content, tool-call blocks, trailing blank line) but
/// is callable in isolation so the same rendering powers both the
/// in-viewport streaming preview and the `insert_before` scrollback
/// flush.
#[must_use]
pub fn message_lines(msg: &ChatMessage) -> Vec<Line<'static>> {
    message_lines_with_verbose(msg, false)
}

/// Same as [`message_lines`] but with explicit control over whether
/// the trailing `[runner stderr]` / `[cast-summary]` diagnostic
/// block is rendered. Default callers should use [`message_lines`]
/// which hides them; the active dispatch path passes the user's
/// `/verbose` toggle via [`AppState::verbose`].
#[must_use]
pub fn message_lines_with_verbose(msg: &ChatMessage, verbose: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Role label. Assistant uses the brand wordmark gradient
    // (Smoo orange→pink + th teal→blue) — anywhere "Smooth" shows
    // up on screen, it should read like the logo. User and System
    // labels stay flat-styled since they're not brand surfaces.
    match msg.role {
        ChatRole::User => {
            lines.push(Line::from(Span::styled("You:", theme::user_label())));
        }
        ChatRole::Assistant => {
            let mut spans: Vec<Span<'static>> = theme::smooth_wordmark();
            spans.push(Span::styled(":", theme::assistant_label()));
            lines.push(Line::from(spans));
        }
        ChatRole::System => {
            lines.push(Line::from(Span::styled("System:", theme::muted())));
        }
    }

    // Assistant content path: markdown for prose, ANSI-color parsing
    // for the runner-stderr block (which arrives at the tail of the
    // message as ANSI-coded tracing logs). Split the content at the
    // first occurrence of `[runner stderr]` — everything before is
    // markdown, everything after gets per-line ANSI parsing so the
    // dim timestamps + green INFO + italic field names render in
    // their actual colors instead of as raw `[2m...[0m` litter.
    let mut content_lines: Vec<Line<'static>> = if matches!(msg.role, ChatRole::Assistant) && !msg.content.is_empty() {
        if let Some(pos) = msg.content.find("[runner stderr]") {
            let (prose, stderr_block) = msg.content.split_at(pos);
            let mut out = if prose.is_empty() {
                Vec::new()
            } else {
                crate::markdown::render(prose)
            };
            // Diagnostics tail. Hidden unless the user has toggled
            // `/verbose` — for normal turns it's noise that buries
            // the actual answer. The content stays in `msg.content`
            // either way so saved sessions round-trip.
            if verbose {
                for raw_line in stderr_block.lines() {
                    let spans = if crate::ansi::line_has_ansi(raw_line) {
                        crate::ansi::parse_line_to_spans(raw_line)
                    } else {
                        vec![Span::styled(raw_line.to_string(), theme::muted())]
                    };
                    out.push(Line::from(spans));
                }
            }
            out
        } else {
            crate::markdown::render(&msg.content)
        }
    } else {
        msg.content.lines().map(|l| Line::from(Span::raw(l.to_string()))).collect()
    };
    if content_lines.is_empty() {
        content_lines.push(Line::from(""));
    }
    if msg.streaming && !msg.content.is_empty() {
        if let Some(last) = content_lines.last_mut() {
            last.spans.push(Span::styled("█", theme::assistant_label()));
        }
    }
    lines.append(&mut content_lines);

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
        // Mutating-on-disk tools (edit_file / write_file / apply_patch)
        // get a unified-diff render below the header line. Other tools
        // keep the existing header-plus-collapsed-output shape.
        let diff_lines = tc
            .arguments_full
            .as_ref()
            .and_then(|args| crate::tool_diff::render(&tc.tool_name, args));

        // For diff-renderable tools, hide the noisy "(args_preview...)"
        // inline payload — the diff below carries the same info, more
        // usefully. Also drop the collapse glyph since the diff is
        // always shown.
        let header_args = if diff_lines.is_some() {
            String::new()
        } else {
            format!("(\"{}\")", tc.arguments_preview)
        };
        let collapse_indicator = if diff_lines.is_some() {
            ""
        } else if tc.output.is_some() {
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
            Span::styled(format!("{}{header_args}", tc.tool_name), theme::muted()),
            Span::raw(format!(" ── {status_label}{collapse_indicator}")),
        ]));

        if let Some(diff) = diff_lines {
            lines.extend(diff);
        } else if !tc.collapsed {
            if let Some(ref output) = tc.output {
                for output_line in output.lines() {
                    lines.push(Line::from(Span::styled(format!("  │ {output_line}"), theme::muted())));
                }
            }
        }
    }

    // Trailing blank line keeps consecutive messages visually separated
    // both in the viewport preview and in scrollback.
    lines.push(Line::from(""));
    lines
}

/// Push every finalized message that's still in `state.messages` past
/// `committed_count` into the terminal's scrollback via
/// [`Frame::insert_before`]. Stops at the first streaming message —
/// in-flight content stays inside the viewport until it finishes.
///
/// Safe to call on every event-loop tick; it's a no-op when
/// `committed_count == messages.len()`.
pub fn flush_to_scrollback<B>(state: &mut AppState, terminal: &mut Terminal<B>) -> Result<()>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
{
    let viewport_width = terminal.size().map_err(anyhow::Error::from)?.width.max(1);
    let verbose = state.verbose;
    while state.committed_count < state.messages.len() {
        let msg = &state.messages[state.committed_count];
        if msg.streaming {
            break;
        }
        let lines = message_lines_with_verbose(msg, verbose);
        let height = paragraph_height(&lines, viewport_width);
        if height == 0 {
            // Nothing to render — still mark committed so we don't
            // loop forever on an oddly-shaped message.
            state.committed_count += 1;
            continue;
        }
        let lines_for_closure = lines;
        terminal
            .insert_before(height, |buf| {
                let paragraph = Paragraph::new(lines_for_closure).wrap(Wrap { trim: false });
                paragraph.render(buf.area, buf);
            })
            .map_err(anyhow::Error::from)?;
        state.committed_count += 1;
    }
    Ok(())
}

/// Push an arbitrary block of styled lines into the terminal's
/// scrollback. Used for the welcome banner / one-off system notes
/// that aren't proper [`ChatMessage`]s.
pub fn insert_before_lines<B>(terminal: &mut Terminal<B>, lines: Vec<Line<'static>>) -> Result<()>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
{
    let viewport_width = terminal.size().map_err(anyhow::Error::from)?.width.max(1);
    let height = paragraph_height(&lines, viewport_width);
    if height == 0 {
        return Ok(());
    }
    terminal
        .insert_before(height, |buf| {
            let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
            paragraph.render(buf.area, buf);
        })
        .map_err(anyhow::Error::from)?;
    Ok(())
}

/// Compute the rendered height for a wrapped paragraph at a given
/// width. Counts soft-wraps so messages whose lines exceed the
/// viewport width don't get clipped when pushed via
/// `insert_before`.
///
/// Implementation note: ratatui's `Paragraph::line_count` is
/// gated behind an unstable feature in 0.30 (issue #293), so this
/// function does the count itself. The arithmetic mirrors what
/// `Wrap { trim: false }` produces — split each logical line into
/// `ceil(display_width / width)` rows, with empty lines counting as
/// one row each.
fn paragraph_height(lines: &[Line<'static>], width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    let w = usize::from(width);
    let mut total: usize = 0;
    for line in lines {
        // Approximate display width as char count. Wide CJK glyphs +
        // emoji can drift by one or two cells per line, but
        // insert_before with a slightly-too-tall block just leaves
        // a blank row in scrollback rather than clipping content,
        // so over-estimating is the safe direction.
        let display_width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        if display_width == 0 {
            total += 1;
        } else {
            total += display_width.div_ceil(w);
        }
    }
    u16::try_from(total).unwrap_or(u16::MAX)
}

/// Render the still-uncommitted (streaming / in-flight) messages into
/// a `Vec<Line>` for the viewport's small preview area. Returns an
/// empty vec when there's nothing in flight.
#[must_use]
pub fn viewport_preview_lines(state: &AppState) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for msg in state.messages.iter().skip(state.committed_count) {
        lines.extend(message_lines_with_verbose(msg, state.verbose));
    }

    // "Generating..." spinner — only when the assistant has started
    // streaming but hasn't emitted any tokens yet. The blinking
    // cursor handles the in-flight visual once content arrives.
    if let Some(last) = state.messages.last() {
        if last.streaming && last.content.is_empty() {
            let spinner = state.spinner_char();
            lines.push(Line::from(Span::styled(format!("{spinner} Generating..."), theme::muted())));
        }
    }
    if state.thinking && state.messages.last().is_none_or(|m| !m.streaming) {
        lines.push(Line::from(Span::styled("Thinking...", theme::muted())));
    }
    lines
}

/// Compute the rendered height of the viewport preview at a given
/// width. Used by the layout to pick how tall to draw the preview
/// region above the input box.
#[must_use]
pub fn preview_height(state: &AppState, width: u16, max: u16) -> u16 {
    let lines = viewport_preview_lines(state);
    if lines.is_empty() {
        return 0;
    }
    paragraph_height(&lines, width).min(max)
}

/// Layout regions for the inline viewport.
///
/// The viewport is laid out top-to-bottom: an optional preview region
/// for the in-flight assistant message, a single-row status bar, and
/// the input box at the bottom.
pub struct InlineRegions {
    pub preview: Option<Rect>,
    pub status: Rect,
    pub input: Rect,
}

/// Compute regions inside the viewport. `preview_h` is the desired
/// preview height (0 = no preview). The input box gets a fixed 3
/// rows; status gets 1; preview takes whatever is left up to
/// `preview_h`.
#[must_use]
pub fn compute_regions(area: Rect, preview_h: u16) -> InlineRegions {
    const INPUT_H: u16 = 3;
    const STATUS_H: u16 = 1;
    let bottom_h = INPUT_H + STATUS_H;
    let available_top = area.height.saturating_sub(bottom_h);
    let actual_preview = preview_h.min(available_top);

    let preview = if actual_preview > 0 {
        Some(Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: actual_preview,
        })
    } else {
        None
    };
    let status = Rect {
        x: area.x,
        y: area.y + actual_preview,
        width: area.width,
        height: STATUS_H,
    };
    let input = Rect {
        x: area.x,
        y: area.y + actual_preview + STATUS_H,
        width: area.width,
        height: INPUT_H,
    };
    InlineRegions { preview, status, input }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ChatMessage;

    #[test]
    fn message_lines_user_emits_label_then_content() {
        let msg = ChatMessage::user("hello");
        let lines = message_lines(&msg);
        assert!(lines[0].spans.iter().any(|s| s.content.contains("You")));
        assert!(lines.iter().any(|l| l.spans.iter().any(|s| s.content.contains("hello"))));
    }

    #[test]
    fn compute_regions_no_preview_when_zero() {
        let area = Rect::new(0, 0, 80, 8);
        let r = compute_regions(area, 0);
        assert!(r.preview.is_none());
        assert_eq!(r.status.height, 1);
        assert_eq!(r.input.height, 3);
        // status sits directly above input
        assert_eq!(r.status.y + r.status.height, r.input.y);
    }

    #[test]
    fn compute_regions_with_preview() {
        let area = Rect::new(0, 0, 80, 12);
        let r = compute_regions(area, 4);
        let preview = r.preview.expect("preview should be present");
        assert_eq!(preview.height, 4);
        assert_eq!(preview.y, 0);
        assert_eq!(r.status.y, 4);
        assert_eq!(r.input.y, 5);
    }

    #[test]
    fn compute_regions_preview_capped_at_available() {
        // Tiny viewport — preview gets squeezed to 0 if input+status
        // already fill it.
        let area = Rect::new(0, 0, 80, 4);
        let r = compute_regions(area, 8);
        assert!(r.preview.is_none());
    }

    #[test]
    fn paragraph_height_handles_wrapping() {
        let lines = vec![Line::from("a".repeat(100))];
        // At width 20, this should wrap to 5 rows.
        let h = paragraph_height(&lines, 20);
        assert_eq!(h, 5);
    }
}

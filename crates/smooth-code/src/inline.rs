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
        // Two diagnostic "frames" the runner / Big Smooth inject
        // into the assistant content:
        //
        // 1. **[runner stderr] block** — single tail block from the
        //    direct (non-streaming) dispatch path. Marker present.
        // 2. **`[runner] …\n` lines** — per-line stderr forwarded
        //    by the sandboxed dispatch path (server.rs:2598). No
        //    marker; the lines just start with `[runner] ` and
        //    are interleaved with prose.
        //
        // Default render hides BOTH unless `/verbose` is on. Prose
        // lines in between still render via markdown.
        let (prose_part, marker_block_part) = if let Some(pos) = msg.content.find("[runner stderr]") {
            let (a, b) = msg.content.split_at(pos);
            (a.to_string(), Some(b.to_string()))
        } else {
            (msg.content.clone(), None)
        };

        // Strip per-line `[runner] ` lines from prose_part when
        // `verbose` is off. Keep them when on so the diagnostics
        // are complete.
        let prose_for_render: String = if verbose {
            prose_part
        } else {
            prose_part
                .lines()
                .filter(|l| !l.starts_with("[runner] ") && !l.starts_with("[runner stderr]") && !l.starts_with("[cast-summary]"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut out = if prose_for_render.trim().is_empty() {
            Vec::new()
        } else {
            crate::markdown::render(&prose_for_render)
        };

        if let Some(stderr_block) = marker_block_part {
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
        }
        out
    } else {
        msg.content.lines().map(|l| Line::from(Span::raw(l.to_string()))).collect()
    };
    if msg.streaming && !msg.content.is_empty() {
        if let Some(last) = content_lines.last_mut() {
            last.spans.push(Span::styled("█", theme::assistant_label()));
        }
    }

    // Render order: role label → tool calls → final response prose.
    // Tool calls happen first chronologically (the model decides to
    // call a tool, the tool runs, then the model writes its answer
    // using the result), so the visible order in chat now matches
    // the temporal order. The TUI's 50ms tick means tool calls show
    // ⚙ pending → ✓ done in real time as they execute, with the
    // final prose appearing only when the model finishes streaming
    // its post-tool answer.
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
        // Live elapsed counter for in-flight tools — the TUI redraws
        // every 50ms so this ticks visibly. Tools that finish in <50ms
        // typically don't render Running at all (the Complete arrives
        // before the next tick), so this only matters for the longer
        // calls where the user actually wants progress feedback.
        #[allow(clippy::cast_precision_loss)]
        let live_elapsed_str = if matches!(tc.status, ToolStatus::Running | ToolStatus::Pending) {
            let elapsed_ms = (chrono::Utc::now() - tc.started_at).num_milliseconds().max(0);
            let secs = elapsed_ms as f64 / 1000.0;
            format!(" ({secs:.1}s)")
        } else {
            String::new()
        };
        let status_label = match tc.status {
            ToolStatus::Pending => format!("pending{live_elapsed_str}"),
            ToolStatus::Running => format!("running{live_elapsed_str}"),
            ToolStatus::Done => format!("done{duration_str}"),
            ToolStatus::Error => format!("error{duration_str}"),
        };
        // Mutating-on-disk tools (edit_file / write_file / apply_patch)
        // get a unified-diff render below the header line. Other tools
        // keep the existing header-plus-collapsed-output shape.
        let diff_lines = tc.arguments_full.as_ref().and_then(|args| crate::tool_diff::render(&tc.tool_name, args));

        // For diff-renderable tools, hide the noisy "(args_preview...)"
        // inline payload — the diff below carries the same info, more
        // usefully. Also drop the collapse glyph since the diff is
        // always shown.
        let header_args = if diff_lines.is_some() {
            String::new()
        } else {
            format!("(\"{}\")", tc.arguments_preview)
        };
        // Force errors expanded — the failure reason is the whole point.
        // Collapsing it behind ▶ hides the actionable info ("path required",
        // "Wonk denied: ...", "tool not in allowlist") at exactly the moment
        // the user needs it to debug. We also force-expand when an errored
        // tool has *no* output captured at all — there's no reason to give
        // the user a chevron to expand into empty content; replace it with
        // a diagnostic "(no error message captured ...)" body.
        let is_error = matches!(tc.status, ToolStatus::Error);
        let has_nonempty_output = tc.output.as_deref().is_some_and(|s| !s.is_empty());
        let force_expand_error = is_error;
        let collapse_indicator = if diff_lines.is_some() || force_expand_error {
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
        } else if !tc.collapsed || force_expand_error {
            let style = if is_error { theme::error() } else { theme::muted() };
            if has_nonempty_output {
                if let Some(ref output) = tc.output {
                    for output_line in output.lines() {
                        lines.push(Line::from(Span::styled(format!("  │ {output_line}"), style)));
                    }
                }
            } else if is_error {
                // Errored tool with empty body — most often a stale Big
                // Smooth daemon (pre-`result`-field parser) or a runner
                // serialization gap. Surface a hint inline rather than
                // leaving the user with a silent ✗.
                lines.push(Line::from(Span::styled(
                    "  │ (no error message captured — daemon may be stale; try `th down && th up`)".to_string(),
                    style,
                )));
            }
        }
    }

    // Blank separator between the tool-call block and the prose
    // response — without it the answer butts up against the last
    // tool call (`✓ list_files(...) ── done` immediately above the
    // first prose line) and reads as one wall of text.
    if !msg.tool_calls.is_empty() && !content_lines.is_empty() {
        lines.push(Line::from(""));
    }

    lines.append(&mut content_lines);

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
    fn assistant_tool_calls_render_before_prose() {
        // Pearl th-render-order: tool calls happen first chronologically
        // (model decides → tool runs → model writes answer using result),
        // so the chat order should match — tools above the response
        // prose, with a blank separator. Regression guard against
        // accidentally moving tool_calls back below content.
        use crate::state::{ToolCallState, ToolStatus};
        let mut msg = ChatMessage::assistant("the answer");
        msg.tool_calls.push(ToolCallState {
            id: "1".into(),
            tool_name: "list_files".into(),
            arguments_preview: "{}".into(),
            arguments_full: None,
            output: None,
            status: ToolStatus::Done,
            collapsed: true,
            started_at: chrono::Utc::now(),
            duration_ms: Some(120),
        });
        let lines = message_lines(&msg);

        let tool_idx = lines
            .iter()
            .position(|l| l.spans.iter().any(|s| s.content.contains("list_files")))
            .expect("tool call header should be present");
        let answer_idx = lines
            .iter()
            .position(|l| l.spans.iter().any(|s| s.content.contains("the answer")))
            .expect("answer prose should be present");

        assert!(
            tool_idx < answer_idx,
            "tool calls must render BEFORE prose (got tool@{tool_idx}, answer@{answer_idx})"
        );

        // And there should be a blank line between them so they don't
        // visually butt together.
        let blank_between = lines[tool_idx + 1..answer_idx]
            .iter()
            .any(|l| l.spans.iter().all(|s| s.content.trim().is_empty()));
        assert!(blank_between, "expected a blank separator line between tool block and prose");
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
    fn errored_tool_call_renders_output_inline_even_when_collapsed() {
        // Even if `collapsed` is true (the default for non-streaming
        // tool calls), an Error status should force the output to
        // render inline so the user sees the failure reason without
        // having to expand. Regression for pearl th-f34f45.
        use crate::state::{ToolCallState, ToolStatus};
        let mut msg = ChatMessage::assistant("");
        let tc = ToolCallState {
            id: "1".into(),
            tool_name: "list_files".into(),
            arguments_preview: String::new(),
            arguments_full: None,
            output: Some("path required".into()),
            status: ToolStatus::Error,
            collapsed: true,
            started_at: chrono::Utc::now(),
            duration_ms: Some(0),
        };
        msg.tool_calls.push(tc);
        let lines = message_lines(&msg);
        let body_visible = lines.iter().any(|l| l.spans.iter().any(|s| s.content.contains("path required")));
        assert!(body_visible, "error output must render inline regardless of collapsed flag");
        // And the header shouldn't carry a ▶ indicator since we forced expand.
        let header_no_caret = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("list_files")))
            .map(|l| !l.spans.iter().any(|s| s.content.contains('▶')))
            .unwrap_or(false);
        assert!(header_no_caret, "errored tool header should drop the ▶ collapse indicator");
    }

    #[test]
    fn errored_tool_with_empty_output_renders_diagnostic_hint() {
        // Wonk-denied calls and stale-daemon scenarios produce an
        // errored tool with `output == Some("")`. Without a fallback
        // the user sees only "✗ name() ── error" and nothing actionable.
        // Pearl th-93ae2e — surface a diagnostic line so the failure
        // is never silent.
        use crate::state::{ToolCallState, ToolStatus};
        let mut msg = ChatMessage::assistant("");
        let tc = ToolCallState {
            id: "1".into(),
            tool_name: "project_inspect".into(),
            arguments_preview: String::new(),
            arguments_full: None,
            output: Some(String::new()),
            status: ToolStatus::Error,
            collapsed: true,
            started_at: chrono::Utc::now(),
            duration_ms: Some(0),
        };
        msg.tool_calls.push(tc);
        let lines = message_lines(&msg);
        let has_diagnostic = lines.iter().any(|l| l.spans.iter().any(|s| s.content.contains("no error message captured")));
        assert!(has_diagnostic, "empty-output errored tool must surface a diagnostic hint");
    }

    #[test]
    fn paragraph_height_handles_wrapping() {
        let lines = vec![Line::from("a".repeat(100))];
        // At width 20, this should wrap to 5 rows.
        let h = paragraph_height(&lines, 20);
        assert_eq!(h, 5);
    }
}

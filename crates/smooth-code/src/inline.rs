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

use std::io;

use anyhow::Result;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::{Terminal, TerminalOptions, Viewport};

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
        // A user turn is a block, not a label: see `user_block` below.
        ChatRole::User => {}
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
            ToolStatus::Running => (theme::tool_status_glyph(ToolStatus::Running), theme::tool_status_border(ToolStatus::Running)),
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
        // Render args as `(<preview>)` — the preview is already
        // human-formatted by `pretty_args_preview` (single-key
        // objects unwrapped, multi-key objects rendered as
        // `key="val", key="val"`). Empty preview → `()` rather than
        // `("")` so no-arg tools don't carry a phantom empty string.
        let header_args = if diff_lines.is_some() {
            String::new()
        } else if tc.arguments_preview.is_empty() {
            "()".to_string()
        } else {
            format!("({})", tc.arguments_preview)
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

        // Indented and dim: the tool calls are the work, the prose below
        // them is the answer, and the answer is what should stand out.
        lines.push(Line::from(vec![
            Span::raw(TOOL_INDENT),
            Span::styled(format!("{icon} "), icon_style),
            Span::styled(format!("{}{header_args}", tc.tool_name), theme::tool_line()),
            Span::styled(format!(" ── {status_label}{collapse_indicator}"), theme::tool_line()),
        ]));

        if let Some(diff) = diff_lines {
            lines.extend(diff);
        } else if !tc.collapsed || force_expand_error {
            let style = if is_error { theme::error() } else { theme::muted() };
            if has_nonempty_output {
                if let Some(ref output) = tc.output {
                    for output_line in output.lines() {
                        lines.push(Line::from(Span::styled(format!("{TOOL_INDENT}  │ {output_line}"), style)));
                    }
                }
            } else if is_error {
                // Errored tool with empty body — most often a stale Big
                // Smooth daemon (pre-`result`-field parser) or a runner
                // serialization gap. Surface a hint inline rather than
                // leaving the user with a silent ✗.
                lines.push(Line::from(Span::styled(
                    format!("{TOOL_INDENT}  │ (no error message captured — daemon may be stale; try `th down && th up`)"),
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

    if msg.role == ChatRole::User {
        content_lines = user_block(content_lines);
    }
    lines.append(&mut content_lines);

    // Trailing blank line keeps consecutive messages visually separated
    // both in the viewport preview and in scrollback.
    lines.push(Line::from(""));
    lines
}

/// Left indent for tool-call lines, so they sit under the assistant label.
const TOOL_INDENT: &str = "  ";

/// Turn the user's content lines into the user-turn block: a coral `▌`
/// accent bar down the left edge, a `❯` prompt glyph on the first row (the
/// rest align under it), and the text in bold default foreground. Same idea
/// as Claude Code / Codex — you can find your own turns by shape alone, so it
/// still works under `NO_COLOR`.
fn user_block(content: Vec<Line<'static>>) -> Vec<Line<'static>> {
    content
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            Line::from(vec![
                Span::styled("\u{258c}", theme::user_accent()),
                Span::styled(if i == 0 { " \u{276f} " } else { "   " }, theme::user_accent()),
                Span::styled(text, theme::user_text()),
            ])
        })
        .collect()
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
        // Pearl `th-2e6693`: prefix with the animated spinner glyph
        // so the byte-stream actually changes every ~100 ms. Without
        // this, the bench's `wait_for_idle` (which polls capture-pane
        // and declares idle when the byte content is stable for the
        // dwell window) would false-fire on long-thinking responses —
        // smooth had to bump the dwell from 8s → 20s to compensate
        // (pearl th-65a041), at the cost of an extra 12s per bench
        // task. Pi + opencode don't need this because their TUIs
        // either stream tokens visibly or animate a spinner natively.
        let spinner = state.spinner_char();
        lines.push(Line::from(Span::styled(format!("{spinner} Thinking..."), theme::muted())));
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

/// Rows the inline viewport should have right now: exactly what it shows
/// (preview, working animation, status, composer, task panel), capped at the
/// startup ceiling `state.viewport_h`. The completion popup gets rows under
/// the composer; the model and session pickers overlay the whole viewport,
/// so they get the full ceiling.
///
/// The app resizes the viewport to this every frame
/// ([`set_viewport_height`]), so an idle session is just status + composer
/// directly under the last message instead of a band of empty preview rows.
#[must_use]
pub fn desired_viewport_height(state: &AppState, width: u16) -> u16 {
    let full = state.viewport_h.max(4);
    if state.model_picker.active || state.session_picker.active {
        return full;
    }
    let cap = crate::composer::max_text_rows(full);
    let input_h = input_height(crate::composer::desired_text_rows(&state.input, width.saturating_sub(2), cap), cap);
    let preview_h = preview_height(state, width, full);
    let below = todo_panel_height(state.todos.len()).max(autocomplete_popup_height(state));
    let fixed = 1 + input_h + below + crate::avatar::rows_wanted(state);
    preview_h.saturating_add(fixed).min(full)
}

/// Rows the `/` / `@` completion popup takes under the composer: up to eight
/// results plus its border, or 0 when it is closed.
#[must_use]
pub fn autocomplete_popup_height(state: &AppState) -> u16 {
    if !state.autocomplete.active || state.autocomplete.results.is_empty() {
        return 0;
    }
    u16::try_from(state.autocomplete.results.len().min(8)).unwrap_or(8) + 2
}

/// Re-create `terminal`'s inline viewport at `height` rows, anchored at the
/// current viewport's top. ratatui 0.30 fixes an inline viewport's height at
/// construction (and only supports `insert_before` on inline viewports), so
/// resizing means wiping the old viewport and building a new terminal there.
/// Shrinking leaves blank rows below the viewport, which the next message
/// pushed into scrollback fills; growing extends downward, scrolling older
/// output up only when the screen is out of room.
pub fn set_viewport_height(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, height: u16) -> Result<()> {
    if terminal.get_frame().area().height == height {
        return Ok(());
    }
    // Inline `clear` parks the cursor at the viewport's top and wipes from
    // there to the end of the screen; the new viewport opens at the cursor.
    terminal.clear()?;
    *terminal = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )?;
    Ok(())
}

/// Layout regions for the inline viewport.
///
/// The viewport is laid out top-to-bottom: an optional preview region
/// for the in-flight assistant message, a single-row status bar, the
/// input box, and the optional task checklist below it.
pub struct InlineRegions {
    /// Blank rows above the preview/status controls; cleared each frame so
    /// content from a previous layout cannot linger there.
    pub spacer: Option<Rect>,
    pub preview: Option<Rect>,
    /// The working avatar ([`crate::avatar`]), directly above the status bar.
    pub avatar: Option<Rect>,
    pub status: Rect,
    pub input: Rect,
    pub todos: Option<Rect>,
}

/// Rows the input box needs to show `text_rows` rows of text, plus its
/// border. `cap` is the growth ceiling ([`crate::composer::max_text_rows`]).
#[must_use]
pub fn input_height(text_rows: u16, cap: u16) -> u16 {
    text_rows.clamp(1, cap.max(1)) + 2
}

/// Compute regions inside the viewport. `preview_h` is the desired
/// preview height (0 = no preview); `input_h` is the input box's total
/// height including its border (see [`input_height`]). Status gets 1 row;
/// preview takes whatever is left up to `preview_h`.
///
/// The input grows by borrowing from the preview once the viewport is at its
/// startup ceiling (see [`desired_viewport_height`]). It is clamped so at
/// least one preview row survives when a preview is wanted — a draft must
/// never hide the streaming answer entirely (pearl th-958e2e).
#[must_use]
pub fn compute_regions(area: Rect, preview_h: u16, input_h: u16) -> InlineRegions {
    compute_regions_with_todos(area, preview_h, input_h, 0)
}

/// Height of the compact task panel: border/title, up to five tasks, and a
/// summary row when more items remain. Zero tasks reserve no rows.
#[must_use]
pub fn todo_panel_height(todo_count: usize) -> u16 {
    if todo_count == 0 {
        return 0;
    }
    let shown = todo_count.min(5);
    let summary = usize::from(todo_count > shown);
    u16::try_from(shown + summary + 2).unwrap_or(u16::MAX)
}

/// Compute regions while reserving `todos_h` rows for the task checklist
/// below the composer. The preview yields space to the checklist first; the
/// composer keeps its minimum height. Any unused preview space becomes a
/// spacer above the status/input controls so they remain at the viewport bottom.
#[must_use]
pub fn compute_regions_with_todos(area: Rect, preview_h: u16, input_h: u16, todos_h: u16) -> InlineRegions {
    compute_regions_with_avatar(area, preview_h, input_h, todos_h, 0)
}

/// [`compute_regions_with_todos`] plus `avatar_h` rows for the working
/// avatar between the preview and the status bar. The avatar only ever
/// takes rows from the top area (spacer, then preview) — never from the
/// composer or the checklist — and is dropped whole rather than squashed
/// when it would leave a wanted preview with no row.
#[must_use]
pub fn compute_regions_with_avatar(area: Rect, preview_h: u16, input_h: u16, todos_h: u16, avatar_h: u16) -> InlineRegions {
    const STATUS_H: u16 = 1;
    const MIN_INPUT_H: u16 = 3;

    let todos_h = todos_h.min(area.height.saturating_sub(MIN_INPUT_H + STATUS_H));
    // Keep one preview row when a preview is wanted; if the viewport is too
    // short, the composer and task panel take priority over the preview.
    let ceiling = area.height.saturating_sub(STATUS_H + todos_h + if preview_h > 0 { 1 } else { 0 });
    let input_h = input_h.clamp(MIN_INPUT_H, ceiling.max(MIN_INPUT_H));

    let bottom_h = input_h + STATUS_H + todos_h;
    let top = area.height.saturating_sub(bottom_h);
    let avatar_h = if avatar_h > 0 && top >= avatar_h + u16::from(preview_h > 0) {
        avatar_h
    } else {
        0
    };
    let available_top = top - avatar_h;
    let actual_preview = preview_h.min(available_top);
    let spacer_h = available_top.saturating_sub(actual_preview);

    let spacer = (spacer_h > 0).then_some(Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: spacer_h,
    });
    let preview = if actual_preview > 0 {
        Some(Rect {
            x: area.x,
            y: area.y + spacer_h,
            width: area.width,
            height: actual_preview,
        })
    } else {
        None
    };
    let avatar = (avatar_h > 0).then_some(Rect {
        x: area.x,
        y: area.y + available_top,
        width: area.width,
        height: avatar_h,
    });
    let status = Rect {
        x: area.x,
        y: area.y + available_top + avatar_h,
        width: area.width,
        height: STATUS_H,
    };
    let input = Rect {
        x: area.x,
        y: status.y + STATUS_H,
        width: area.width,
        height: input_h,
    };
    let todos = (todos_h > 0).then_some(Rect {
        x: area.x,
        y: input.y + input.height,
        width: area.width,
        height: todos_h,
    });
    InlineRegions {
        spacer,
        preview,
        avatar,
        status,
        input,
        todos,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ChatMessage;

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn user_turn_is_a_barred_block_with_a_prompt_glyph() {
        let msg = ChatMessage::user("hello\nsecond line");
        let lines = message_lines(&msg);
        assert_eq!(
            plain(&lines),
            vec!["\u{258c} \u{276f} hello", "\u{258c}   second line", ""],
            "bar + ❯ + aligned continuation, then a blank before the reply"
        );
        // Shape, not just hue: the bar and glyph stand alone under NO_COLOR,
        // and the text is bold where the assistant's prose is plain.
        assert!(lines[0].spans[2].style.add_modifier.contains(ratatui::style::Modifier::BOLD));
        assert_eq!(lines[0].spans[0].style, theme::user_accent());
        assert!(!plain(&lines).iter().any(|l| l.contains("You:")), "no flat label any more");
    }

    #[test]
    fn assistant_turn_does_not_wear_the_user_block() {
        let lines = message_lines(&ChatMessage::assistant("hi there"));
        assert!(plain(&lines).iter().all(|l| !l.starts_with('\u{258c}')), "{:?}", plain(&lines));
    }

    #[test]
    fn tool_lines_are_indented_and_dim() {
        let mut msg = ChatMessage::assistant("answer");
        let mut tc = crate::state::ToolCallState::new("t1", "bash", &serde_json::json!({"command": "ls"}));
        tc.status = ToolStatus::Done;
        tc.output = Some("a.txt".into());
        tc.collapsed = false;
        msg.tool_calls.push(tc);
        let lines = message_lines(&msg);
        let text = plain(&lines);
        let header = lines.iter().zip(&text).find(|(_, t)| t.contains("bash(")).map(|(l, _)| l).expect("tool header");
        assert!(text.iter().any(|t| t.starts_with("  ✓ bash(")), "indented header: {text:?}");
        assert!(text.iter().any(|t| t.starts_with("    │ a.txt")), "output indented under it: {text:?}");
        assert!(header.spans[2].style.add_modifier.contains(ratatui::style::Modifier::DIM));
        let answer = lines.iter().find(|l| plain(std::slice::from_ref(l))[0].contains("answer")).unwrap();
        assert!(
            answer.spans.iter().all(|s| !s.style.add_modifier.contains(ratatui::style::Modifier::DIM)),
            "the answer is not dimmed"
        );
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
        let r = compute_regions(area, 0, input_height(1, crate::composer::MAX_TEXT_ROWS));
        assert!(r.preview.is_none());
        assert_eq!(r.spacer.expect("idle spacer").height, 4);
        assert!(r.todos.is_none());
        assert_eq!(r.status.height, 1);
        assert_eq!(r.input.height, 3);
        // status sits directly above input
        assert_eq!(r.status.y + r.status.height, r.input.y);
    }

    #[test]
    fn compute_regions_with_preview() {
        let area = Rect::new(0, 0, 80, 12);
        let r = compute_regions(area, 4, input_height(1, crate::composer::MAX_TEXT_ROWS));
        let preview = r.preview.expect("preview should be present");
        assert_eq!(preview.height, 4);
        assert_eq!(preview.y, 4, "unused rows stay above the bottom-aligned controls");
        assert_eq!(r.status.y, 8);
        assert_eq!(r.input.y, 9);
        assert_eq!(r.input.y + r.input.height, area.y + area.height);
    }

    #[test]
    fn todo_panel_height_is_bounded_and_empty_lists_take_no_space() {
        assert_eq!(todo_panel_height(0), 0);
        assert_eq!(todo_panel_height(1), 3);
        assert_eq!(todo_panel_height(5), 7);
        assert_eq!(todo_panel_height(6), 8, "sixth row summarizes the remaining tasks");
    }

    #[test]
    fn task_panel_is_below_input_and_takes_space_from_preview() {
        let area = Rect::new(0, 0, 80, 14);
        let r = compute_regions_with_todos(area, 5, input_height(1, crate::composer::MAX_TEXT_ROWS), 6);
        let preview = r.preview.expect("preview should keep the remaining top rows");
        let todos = r.todos.expect("task list should have a region");
        assert_eq!(r.input.y + r.input.height, todos.y, "task panel follows the composer");
        assert_eq!(todos.y + todos.height, area.y + area.height, "task panel ends at the viewport bottom");
        assert_eq!(preview.height + r.status.height + r.input.height + todos.height, area.height);
        assert_eq!(preview.height, 4, "preview yields one row to the requested task panel");
    }

    #[test]
    fn compute_regions_preview_capped_at_available() {
        // Tiny viewport — preview gets squeezed to 0 if input+status
        // already fill it.
        let area = Rect::new(0, 0, 80, 4);
        let r = compute_regions(area, 8, input_height(1, crate::composer::MAX_TEXT_ROWS));
        assert!(r.preview.is_none());
    }

    /// One text row + two border rows, capped at `MAX_TEXT_ROWS`.
    #[test]
    fn input_height_adds_the_border_and_caps_growth() {
        assert_eq!(input_height(1, crate::composer::MAX_TEXT_ROWS), 3, "the historical fixed height");
        assert_eq!(input_height(4, crate::composer::MAX_TEXT_ROWS), 6);
        assert_eq!(input_height(0, crate::composer::MAX_TEXT_ROWS), 3, "never smaller than one text row");
        assert_eq!(input_height(99, crate::composer::MAX_TEXT_ROWS), crate::composer::MAX_TEXT_ROWS + 2);
        assert_eq!(input_height(99, 12), 14, "a taller viewport raises the ceiling (th-d5eb9f)");
    }

    /// A growing draft borrows rows from the preview — the viewport height is
    /// fixed at startup, so there is nowhere else for them to come from.
    #[test]
    fn a_taller_input_takes_rows_from_the_preview() {
        let area = Rect::new(0, 0, 80, 14);
        let short = compute_regions(area, 10, input_height(1, crate::composer::MAX_TEXT_ROWS));
        let tall = compute_regions(area, 10, input_height(5, crate::composer::MAX_TEXT_ROWS));

        assert_eq!(short.input.height, 3);
        assert_eq!(tall.input.height, 7);
        assert_eq!(short.preview.expect("preview").height, 10);
        assert_eq!(tall.preview.expect("preview").height, 6, "preview yields exactly what input took");

        // Whatever the split, the three regions still tile the viewport
        // exactly — an overlap is the out-of-buffer panic (th-paste-crash).
        for r in [short, tall] {
            let preview_h = r.preview.map_or(0, |p| p.height);
            let todos_h = r.todos.map_or(0, |t| t.height);
            assert_eq!(preview_h + r.status.height + r.input.height + todos_h, area.height);
            assert_eq!(r.status.y, area.y + preview_h);
            assert_eq!(r.input.y, r.status.y + r.status.height);
        }
    }

    /// A tall draft must never squeeze the streaming answer out entirely, or
    /// the user can't see what they're replying to.
    #[test]
    fn input_growth_leaves_a_preview_row_when_one_is_wanted() {
        let area = Rect::new(0, 0, 80, 8);
        let r = compute_regions(area, 4, input_height(crate::composer::MAX_TEXT_ROWS, crate::composer::MAX_TEXT_ROWS));
        let preview = r.preview.expect("preview must survive");
        assert!(preview.height >= 1);
        assert_eq!(preview.height + r.status.height + r.input.height, area.height);
    }

    /// Regions must stay inside the viewport even when it is absurdly short.
    #[test]
    fn avatar_takes_rows_from_the_preview_never_the_input() {
        let area = Rect::new(0, 0, 80, 14);
        let without = compute_regions_with_avatar(area, 10, 3, 0, 0);
        let with = compute_regions_with_avatar(area, 10, 3, 0, 3);
        assert!(without.avatar.is_none());
        let avatar = with.avatar.expect("room for the avatar");
        assert_eq!(avatar.height, 3);
        assert_eq!(with.input, without.input, "input box is untouched");
        assert_eq!(with.status, without.status, "status bar stays put");
        assert_eq!(avatar.y + avatar.height, with.status.y, "avatar sits directly above the status bar");
        assert_eq!(with.preview.unwrap().height, without.preview.unwrap().height - 3, "preview yields the rows");
    }

    #[test]
    fn avatar_is_dropped_whole_when_there_is_no_room() {
        // 3 input + 1 status leaves 2 top rows: not enough for a 3-row avatar.
        let r = compute_regions_with_avatar(Rect::new(0, 0, 80, 6), 0, 3, 0, 3);
        assert!(r.avatar.is_none());
        // Room for the avatar but a wanted preview must keep one row.
        let r = compute_regions_with_avatar(Rect::new(0, 0, 80, 7), 5, 3, 0, 3);
        assert!(r.avatar.is_none());
        assert_eq!(r.preview.unwrap().height, 3);
    }

    #[test]
    fn regions_never_escape_a_tiny_viewport() {
        for height in 1..=16u16 {
            for text_rows in 1..=crate::composer::MAX_TEXT_ROWS {
                let area = Rect::new(0, 0, 40, height);
                let r = compute_regions(area, height, input_height(text_rows, crate::composer::MAX_TEXT_ROWS));
                let bottom = r.input.y + r.input.height;
                assert!(
                    bottom <= area.y + area.height.max(4),
                    "height {height}, rows {text_rows}: input bottom {bottom} escapes viewport {height}"
                );
            }
        }
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

    #[test]
    fn viewport_fits_its_content() {
        let mut s = AppState::new(std::path::PathBuf::from("/tmp"));
        s.viewport_h = 20;
        assert_eq!(desired_viewport_height(&s, 80), 4, "idle: status + 3-row composer, no empty preview band");

        s.avatar_motion = true;
        s.thinking = true;
        s.sync_turn_clock();
        assert_eq!(
            desired_viewport_height(&s, 80),
            7 + preview_height(&s, 80, 20),
            "working: + the 3-row animation and the preview's spinner row"
        );

        let mut msg = ChatMessage::assistant("one\ntwo");
        msg.streaming = true;
        s.messages.push(msg);
        let preview = preview_height(&s, 80, 20);
        assert!(preview >= 2);
        assert_eq!(desired_viewport_height(&s, 80), 7 + preview, "+ the preview's rows");

        s.viewport_h = 8;
        assert_eq!(desired_viewport_height(&s, 80), 8, "capped at the startup ceiling");

        let mut idle = AppState::new(std::path::PathBuf::from("/tmp"));
        idle.viewport_h = 20;
        idle.model_picker.active = true;
        assert_eq!(desired_viewport_height(&idle, 80), 20, "pickers overlay the preview area: full height");
    }
}

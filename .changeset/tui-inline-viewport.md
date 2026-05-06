---
"@smooai/smooth": minor
---

th smooth TUI: inline viewport (Claude Code style) + borderless chat

The chat TUI used to live entirely inside an alt-screen with a fixed
`Paragraph` that scrolled an in-app message buffer. That setup
disabled the terminal's native wheel-scroll, drag-select, search, and
copy — every one of those had to be re-implemented inside the app, and
none of them worked well. Switch to ratatui's `Viewport::Inline`:

- The TUI owns only ~14 rows at the bottom of the terminal: the
  input box, status bar, and an optional preview area for the
  in-flight streaming assistant message.
- Finalized chat messages flow into the **terminal's own scrollback**
  via `Frame::insert_before`. A new `committed_count` cursor on
  `AppState` tracks which messages have been pushed; each event-loop
  tick flushes any newly-finalized ones before drawing the viewport.
- Native wheel scroll, drag-select, search, and copy all work as
  they would for any other terminal output. No in-app reimplementation.

Side effects:
- Alt-screen is gone. `SMOOTH_TUI_NO_ALT_SCREEN=1` is now a no-op
  (kept readable so it doesn't error on shells with the var set).
- The chat panel border was already redundant once selection moved
  to the terminal; it's removed entirely. Role labels + blank-line
  spacing carry visual structure.
- Sidebar (`Ctrl+B`) is dropped. It needs an inline-friendly redesign
  (slash commands like `/git`, `/files` are the obvious next step).
  The keybinding is intentionally left unbound rather than re-purposed.
- New `crate::inline` module: `message_lines` (single-message →
  styled `Line`s, shared between viewport preview and `insert_before`
  flush), `flush_to_scrollback`, `viewport_preview_lines`,
  `compute_regions`. Tested.

Trade-offs:
- The streaming preview area is capped at viewport height − 4
  rows. If a single response is taller than that, the most recent
  rows stay visible during streaming; the full text lands in
  scrollback when streaming completes.
- The fancy gradient SMOOTH wordmark welcome banner is no longer
  rendered (kept as `#[allow(dead_code)]` for a possible
  fixed-screen toggle later). The system "Welcome to Smooth" line
  remains.

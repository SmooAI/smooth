---
"@smooai/smooth": patch
---

TUI: hide `[runner stderr]` / `[cast-summary]` diagnostic block by default; toggle with `/verbose`

Every assistant turn was dumping the runner's tracing logs +
cast-summary JSON at the end of the message. Useful for debugging,
but for the vast majority of turns it's just noise that buries the
actual answer.

Default to hidden:

- New `AppState::verbose: bool` (default `false`).
- New `/verbose` slash command — no-arg toggle, or explicit
  `/verbose on` / `/verbose off`.
- `inline::message_lines_with_verbose(msg, verbose)` — same shape
  as `message_lines` but with explicit control. The default-export
  `message_lines` keeps `verbose=false` for callers that don't
  thread state. The active dispatch path (`flush_to_scrollback` +
  `viewport_preview_lines`) reads `state.verbose` and passes
  through.
- Content stays in `msg.content` either way, so saved sessions
  round-trip correctly — only the rendered output skips the
  diagnostic block when verbose is off.

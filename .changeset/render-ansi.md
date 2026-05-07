---
"@smooai/smooth": minor
---

TUI: render ANSI escape sequences as actual colors (not strip, not raw)

Previous pearl `th-a14138` stripped ANSI codes from streaming text
because the markdown renderer was leaving `[2m...[0m` as raw
literals. User wanted the colors *kept* — they're how the runner's
tracing logs become readable (dim timestamps, green INFO, italic
field names).

Replace `crate::ansi::strip` with a real SGR parser:

- `ansi::line_has_ansi(line) -> bool` — cheap pre-check.
- `ansi::parse_line_to_spans(line) -> Vec<Span<'static>>` — walks
  the SGR codes and produces styled ratatui Spans. Handles ESC-
  prefixed and bare-bracket forms (sometimes the ESC byte is
  scrubbed in transit). Supports: 0 reset, 1 bold, 2 dim, 3
  italic, 4 underline, 9 strikethrough, 22/23/24/29 modifier
  clears, 30-37 fg, 39 default fg, 40-47 bg, 49 default bg,
  90-97 + 100-107 bright variants, 38;5;N + 48;5;N (256-color),
  38;2;R;G;B + 48;2;R;G;B (true color).
- 10 unit tests including a real runner-stderr sample.

Wire-in (`inline::message_lines`): when the assistant content
contains `[runner stderr]`, split there. Render the prose prefix
through markdown as today; render the stderr suffix line-by-line
with `ansi::parse_line_to_spans`. Diagnostics now display with
their original styling instead of raw escape codes or stripped
plaintext.

`AppState::append_stream_content` reverts to passing content
through verbatim — the rendering layer owns ANSI handling now.

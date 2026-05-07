---
"@smooai/smooth": patch
---

TUI: strip ANSI escape sequences from streaming assistant content

The runner emits structured tracing logs colored with ANSI SGR codes
(`\x1b[2m...\x1b[0m`, `\x1b[32m INFO`, `\x1b[3mfield\x1b[0m=value`,
etc.). Big Smooth forwards runner stderr as `TokenDelta` chunks for
the assistant message, those codes ride along, and the markdown
renderer treats them as plain text — the chat fills with raw
`[2m2026-05-07T13:43:52.300628Z[0m [32m INFO[0m ...` litter.

New `crate::ansi::strip(s)` does a linear scan and removes any
`\x1b[<digits>(;<digits>)*m` sequence, plus the bare-bracket form
`[<digits>(;<digits>)*m` (the ESC byte is sometimes lost in transit
through WebSockets / terminal copy-paste). Conservative — only
matches digit-only param sequences ending in `m`, so legit
markdown like `[link](url)` and array syntax `[1, 2, 3]` stay
untouched. 8 unit tests including a real runner-stderr sample.

Hook point: `AppState::append_stream_content` strips before
pushing into the message buffer. Markdown render and
`flush_to_scrollback` see clean text.

(Web parity is filed separately as `th-a14138` — same fix needed
in chat.tsx's WebSocket handler.)

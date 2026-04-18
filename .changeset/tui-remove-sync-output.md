---
"@smooai/smooth": patch
---

TUI: remove CSI 2026 synchronized-output wrapper around each
render. Fixes the "I can type but the screen doesn't update until I
^C" class of bug reported on at least one macOS terminal.

Root cause: the event loop wrapped each `terminal.draw` with
`print!("{}", begin_sync())` / `print!("{}", end_sync())`. On
terminals that half-support CSI 2026 (or where `print!` doesn't
flush between the begin and the end), frames get stuck in the
terminal's buffer until the process exits and stdout flushes on
teardown — so typed input appears to be ignored until you kill
`th`.

ratatui's backend already produces flicker-free output via
crossterm's diff-based rendering, so the sync wrapper was a
micro-optimization not worth the fragility it introduced. Dropped.

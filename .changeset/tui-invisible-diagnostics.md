---
"@smooai/smooth": patch
---

th-324b12: instrument `smooth-code`'s TUI startup so "my terminal
shows nothing" is diagnosable without a screen recording.

Three additions to `crates/smooth-code/src/app.rs::run`:

1. **TTY pre-flight.** If `stdin` or `stdout` isn't a terminal, fail
   with a clear message pointing at `th code --headless`. Previously
   the app would enter alt-screen, render to /dev/null, and exit
   cleanly — the user saw nothing and had no clue why. Also reliably
   caught via a regression test (`run_requires_tty`).

2. **`SMOOTH_TUI_NO_ALT_SCREEN=1` escape hatch.** Some terminals
   (a few tmux configs, certain Windows terminals, odd ssh
   multiplexes) don't cleanly combine alt-screen + mouse-capture +
   CSI 2026 synchronized output. The env var drops alt-screen and
   mouse-capture so the UI renders inline in the primary buffer —
   scrollback gets mixed with the TUI output but at least the user
   can *see* something.

3. **`SMOOTH_TUI_DEBUG=1` step log.** When set, every major startup
   step (TTY check, raw-mode enable, alt-screen enter, Terminal
   creation + size, first draw, event-loop entry + exit, terminal
   restore) logs to `~/.smooth/logs/smooth-code.log` with a
   timestamp. Zero-cost when unset. Lets us trace exactly where
   `run()` gave up on an environment-specific blackout without
   needing a tmux capture.

Also: initial forced `terminal.draw` before the event loop starts,
so even if `event::poll` blocks for a long time on the first
iteration, the welcome message is visible immediately. Previously
the draw only happened at the top of the loop body, gated by the
auto-save check — a startup stall could delay the first frame.

Improved error messages on `enable_raw_mode` + `EnterAlternateScreen`
failures suggest `SMOOTH_TUI_NO_ALT_SCREEN=1` as the first thing to
try when a terminal silently rejects the setup.

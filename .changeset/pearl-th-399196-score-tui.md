---
'@smooai/smooth': patch
---

Pearl th-399196: `smooth-bench score-tui` — drive `th code` via tmux + LLM-as-human loop.

Adds a new `smooth-bench score-tui` subcommand that runs the curated
aider-polyglot sweep against the real `th code` TUI instead of the
WebSocket chat-agent path, so the bench exercises what a human user
actually touches: the TUI's prompt parsing, the model alias→upstream
display, tool-call surfacing, and session lifecycle.

How it works:

- A new `TmuxDriver` (`crates/smooth-bench/src/tmux_driver.rs`)
  spawns `th code` inside a detached tmux session, types into it
  via `send-keys`, and reads visible output via `capture-pane`.
- A new LLM-as-human loop (`crates/smooth-bench/src/human_driver.rs`)
  asks a cheap driver model (default `Activity::Summarize`) to play
  the role of a user testing the assistant: it reads the current
  pane snapshot each turn and decides what to type next, or fires
  the `TASK_COMPLETE` / `TASK_STUCK` sentinels.
- The new orchestrator (`crates/smooth-bench/src/tui_score.rs`)
  ties it together: per task it preps the scratch dir via the
  newly-extracted `prepare_task` helper, drives the human loop,
  then scores via the shared `finalize_and_score` helper.
- Emits the same `Score` shape as `score --pr` / `score --release`,
  plus a `via: "tui"` marker on the `TuiSweepRun` for downstream
  analysis.

Flag surface mirrors `score`: `--pr`, `--release`, `--budget-usd`,
`--output`, `--url`. New TUI-specific flags: `--tmux-session`
(default `smooth-bench-tui`), `--th-binary` (default `th`),
`--driver-model` (default `summarize`), `--max-turns` (default 15),
`--task-timeout-s` (default 900).

The existing WebSocket `score` path is unchanged — `chat_driver.rs`
and `sweep.rs` are untouched aside from shared helpers extracted up
into `lib.rs` (`prepare_task`, `finalize_and_score`) which the
WebSocket path now also uses for zero-drift task setup.

Tests:

- `tmux_driver` exercised against `echo`, `cat`, and `sleep` shell
  fixtures (no `th` needed for unit tests).
- `human_driver` decision parsing + prompt assembly tested with a
  hand-rolled `FakeDriver` (no live LLM).
- `tui_score` aggregation + shell-escape + tool-call counting
  tested without spawning real microVMs.

Heavier integration tests against a real Safehouse + dataset are
left to the operator to invoke via `smooth-bench score-tui --pr`.

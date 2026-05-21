---
'@smooai/smooth': patch
---

Pearl th-a5ca18: fix five score-tui bench harness bugs so a `--pr`
sweep produces honest pass-rates with real cost numbers.

The previous score-tui run reported 2/18 pass with $0.00 cost
across all tasks, and tasks 15-18 errored at 0ms with `no server
running on /private/tmp/tmux-501/default`. Five independent bugs:

**Bug 1 — tmux server dies mid-sweep.** The harness shared the
default tmux socket across all tasks. When task N's `Drop` killed
the last surviving session on that socket, tmux server-exited and
every subsequent task got "no server running". Fix: per-task
socket isolation via `tmux -L <socket>`. Each `TmuxDriver` gets a
unique socket name; every `tmux …` invocation passes `-L`; `Drop`
runs `kill-server` on its own socket only. New regression test
`per_socket_isolation_survives_sibling_drop` verifies dropping one
driver does not affect another's server.

**Bug 2 — cost reported as $0.00 across all tasks.** The TUI's
status line shows `spend: $X.XXX`, but the harness never scraped
it. Fix: at task end, grab a visible-only capture (the status
line is always in the visible region by definition), regex-extract
the spend, and thread the value into the `TuiTaskOutcome::cost_usd`
field. Falls back to 0.0 + warning when the pattern isn't found —
never fabricates.

**Bug 3 — Rust false-positive passes.** Both prior runs reported
2/3 Rust passes on workspaces where `src/lib.rs` still held the
dataset's `todo!()` macro. Root cause: the user's
`~/.cargo/config.toml` sets `target-dir = ~/.cargo/shared-target`,
so `cargo test` reused a previously-compiled test binary from an
earlier successful run (verified by hand: running cargo test with
the shared target dir → 10 passed; with `CARGO_TARGET_DIR` pointed
at a per-task `<work_dir>/target` → 10 failed via todo!() panic).
Two defences: (a) `score_work_dir` now sets `CARGO_TARGET_DIR` to
a per-task isolated path so the shared cache can't leak across
runs; (b) the harness hashes every editable file before the agent
runs, re-hashes after, and refuses to mark a task solved=true when
the agent made zero changes (`--allow-no-edit-passes` opts out for
debugging).

**Bug 4 — agents do real work but tasks still fail.** Investigation
across five failed-task pane logs found this is tied to Bug 5
(below): the agent IS writing code and spending money, but the
LLM-as-human driver only sees the bottom slice of the pane via
`tmux capture-pane -p`, so the driver keeps re-asking questions
the agent has already answered. The agent's tool calls and edited
content scroll off the visible region and the driver has no idea
work happened. Fix follows directly from Bug 5.

**Bug 5 — `capture-pane` blind to scrollback.** Confirmed by
end-to-end read of
`~/.smooth/bench-runs/e219203e/python-book-store.pane.log`: every
`[idle]` capture shows the same bottom-of-pane slice (~50 rows of
the input box + status line + last few wrapped lines of the most
recent LLM response). The chat history, tool calls, and diffs are
all in tmux's scrollback, invisible to the driver. Fix:
`capture()` now passes `-S -` (start of scrollback) and `-J` (join
wrapped lines), returning the full pane history. A
`DEFAULT_CAPTURE_MAX_BYTES` (64 KiB) budget caps memory by
truncating from the FRONT (dropping the oldest, keeping the
freshest) with a marker prepended so the driver knows the very
start was clipped. Added `capture_visible()` for the
specific case (Bug 2) where we only want the bottom status line.

Tests added (151 lib tests passing): per-socket isolation, full-
scrollback capture, front-truncation budget + newline snapping,
cost-extraction (real status line + repeated repaints + zero
dollars + no-dollar-sign + malformed + dot-only forms), and
hash-based editable-file detection across all five languages.

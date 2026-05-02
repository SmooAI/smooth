---
"@smooai/smooth": patch
---

dispatch: pearl-comment heartbeat during sandbox exec

`exec_in_sandbox` is a blocking call — the runner's
`AgentEvent::ToolCallStart` events arrive in one batch when the run
finishes, so the pearl's comment count stays flat for the entire
sandbox lifetime. Any external poller that uses comment-growth as a
liveness signal (notably the bench harness, see
`SMOOTH_BENCH_IDLE_GRACE_S`) gives up well before the operator is
genuinely done.

`dispatch_ws_task_sandboxed` now spawns a heartbeat task that posts
`[PROGRESS] sandbox running (Ns elapsed, heartbeat #N)` to the pearl
every 30s while the exec is in flight. The task is aborted as soon as
exec returns (success, error, or destroy path).

Tunable via `SMOOTH_DISPATCH_HEARTBEAT_S`:
- `0` — heartbeat disabled (useful for tests or when observing
  genuine quiescence is desired)
- `30` (default) — 30s cadence
- any positive integer — custom cadence

Without this, today's bench-harness 600s `idle_grace` still
double-times-out tasks that are mid-LLM-call when the exec_in_sandbox
poll is silent for the whole window. With it, the pearl gets a fresh
comment every 30s and the harness's grace timer keeps resetting until
the run actually finishes.

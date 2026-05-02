---
"@smooai/smooth": patch
---

dispatch: post `[IDLE]` comment when sandbox exec terminates abnormally

`dispatch_ws_task_sandboxed` only closed the pearl on the success path
(exit 0). On `exec_in_sandbox` `Err(_)` and on non-zero runner exit
codes, the pearl stayed `in_progress` with no terminal comment, so any
poller waiting on `[IDLE]` / status-Closed had to fall through to the
quiescence-grace timeout (default 600 s in the bench harness) to
realise the dispatch was over.

Both error paths now post a `[IDLE]` comment before returning:
- `Err(e)` on `exec_in_sandbox`: `[IDLE] sandbox exec failed: {e}`
- non-zero runner exit: `[IDLE] sandboxed runner exited with code {code}`,
  and the pearl status reverts to `Open` so the orchestrator can
  re-dispatch (matching `revert_pearl_to_open` semantics from the
  Mode B retry path).

The bench harness's pearl-comment-polling loop already keys on
`[IDLE]` as one of its three completion signals (alongside
`PearlStatus::Closed` and the quiescence grace), so this drops
worst-case task-failure latency from ~10 minutes to immediate.

---
"@smooai/smooth": patch
---

bench: raise default chat-driver timeouts and make them configurable

The bench's chat-agent-driven path
(`crates/smooth-bench/src/chat_driver.rs`) had two hardcoded 120 s
timeouts that consistently scored real solves as FAIL:

- The reqwest HTTP-client timeout on `POST /api/chat` (the dispatch
  call). On a cold daemon or first-task dispatch the chat-agent
  sometimes legitimately takes longer than 120 s to spawn the
  teammate and return the pearl id.
- The `idle_grace` quiet-timeout in the comment-polling loop. When
  the teammate doesn't post a `[PROGRESS]` comment within 120 s of
  the last comment, the bench treats the pearl as done and runs the
  test against an unchanged workspace — scoring real in-flight
  solves as FAIL.

Both are now env-configurable with raised defaults:

| Env var | Default | Purpose |
| --- | --- | --- |
| `SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S` | 300 | reqwest timeout on `POST /api/chat` |
| `SMOOTH_BENCH_IDLE_GRACE_S` | 600 | comment-polling quiet timeout |

The chat-driver also logs `bench: pearl <id> polling with idle_grace=Ns`
on dispatch so the active value is visible in the run log without
inspecting env.

Empirical evidence from the 2026-05-02 tuning-batch run
(`docs/bench-sessions/2026-05-02-tuning-batch.md`): solves of
cpp/all-your-base (218 s), java/alphametics (231 s), python/book-store
(165 s), and others were all timing out at the 120 s grace. With 600 s
the harness will let those finish. 300 s is the wall-clock budget for
the dispatch-side HTTP call (the operator can run for much longer
after that point, polled via the pearl).

One unit test (`env_secs_falls_back_when_unset_or_invalid`) covers the
unset / garbage / valid-integer branches of the env-reader helper.

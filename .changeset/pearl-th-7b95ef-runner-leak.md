---
"@smooai/smooth": patch
---

Pearl th-7b95ef: stop operator-runner stderr from being persisted as assistant chat content. The runner's diagnostic output now goes only through `tracing` to stderr (debug for repeated bootstrap noise, info for actionable events); bigsmooth's stdout reader now classifies lines via a new `classify_runner_stdout_line` helper and drops anything that isn't valid JSON. Both stdout-non-JSON and stderr forwarding to `ServerEvent::TokenDelta` are removed, so session JSONs are no longer poisoned with `[runner] SMOOTH_POLICY_FILE env var not set` blobs. New regression test asserts the runner binary's stdout contains zero `[runner]` substrings.

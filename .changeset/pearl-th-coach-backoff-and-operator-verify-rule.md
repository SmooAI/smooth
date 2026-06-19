---
"@smooai/smooth": minor
---

Two surgical bench-quality fixes triggered by the 2026-05-29 coach matrix root-cause analysis (see `docs/bench-sessions/2026-05-29-coach-vs-user.md`):

1. **`smooth-operator`: new `AgentConfig::with_verify_tests_before_done(bool)` builder** that appends a stopgap system-prompt rule forbidding the agent from declaring done until it has run the project's test command (pytest / cargo test / npm test / go test) and seen passing tests. Targets the failure mode where deepseek/kimi/claude bail at 2-3 iterations with partial solutions (11/16, 18/20, 8/10) — the coach driver's "did you run the tests?" demand fires too late because the agent has already emitted `Completed`. This rule applies the same intent INSIDE the agent loop where it can stop early termination. Opt-in (default off) so general `th code` sessions stay snappy. Idempotent. Architectural follow-up: th-VERIFY-PHASE (full automatic test-runner invocation post-`done`).

2. **`smooth-bench` coach persona: new BACK-OFF RULE** — if the assistant has already shown a passing test run (`N passed in X.XXs` / `test result: ok. N passed; 0 failed` / `Tests: N passed`), the coach must fire `TASK_COMPLETE` this turn instead of re-probing. Targets claude-sonnet-4-6's coach-persona regression (3/5 → 2/5) where the coach kept asking for more verification after claude had already shown passing pytest output. Should restore claude's user-persona pass rate without affecting glm-5.1's coach-driven 5/5.

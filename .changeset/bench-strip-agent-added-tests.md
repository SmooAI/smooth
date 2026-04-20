---
"smooai-smooth-bench": patch
---

Bench: strip agent-added test files before scoring. Polyglot scorer runs the test command over the whole work dir, so any `test_*.py` / `*_test.go` / `*.spec.ts` / `*Test.java` / etc. the agent added during EXECUTE would get counted and tilt the score. The harness now snapshots the original file set before dispatching to the agent, and after the run deletes any files that (a) weren't in the snapshot AND (b) match per-language test-file conventions. Non-test files the agent added (new helpers, modules) are left alone. Original test files are always preserved. Benchmark invariant: only the provided tests count.

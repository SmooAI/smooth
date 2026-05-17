# Bench Harness

#engineering

> [!info] How we measure ourselves
> `th bench` runs Exercism-style problems through the agent loop with deterministic scoring. The dashboard's "The Line" tracks the rolling score over time so we can tell when a change made the agent better or worse.

## The crate

`crates/smooth-bench/` owns the harness. Curated tasks live in `crates/smooth-bench/curated-tasks.toml`. Each task is a directory of problem statement + tests + reference solution; the harness scores by running the test suite.

## Running locally

```bash
th bench                             # Run the curated suite
th bench --task <id>                 # Run a single task
th bench --print                     # Pretty-print results + cost
```

The harness uses **direct mode** (`SMOOTH_WORKFLOW_DIRECT=1`) by default — microVM cold-start cost would dominate the timing and the bench needs reproducible numbers. See [[Direct-Mode]].

It also sets `SMOOTH_WORKFLOW_SKIP_TEST=1` so the TEST phase doesn't add tests of its own (which would skew the score). The harness runs the canonical test suite itself, post-agent.

## What gets measured

| Metric          | What                                                            |
| --------------- | --------------------------------------------------------------- |
| Score           | Pass/fail on the canonical test suite                           |
| Iterations      | Agent-loop iterations spent on the task                          |
| Cost            | `cost_usd` from the LLM gateway (6 decimal places)               |
| Wall time       | End-to-end seconds                                              |
| Tool calls      | Count by tool name (used for regression bisects)                 |

Output is JSON-lines plus a printed summary. The CI workflow promotes the summary to `docs/bench-badge.json` and appends to `docs/bench-history.md` so the README badge stays current.

## The Line

The "Line" is the rolling per-task score in `docs/bench-history.md`. Every merged change to `main` re-runs the bench and writes a new line; PRs that move The Line in the wrong direction are visible in review.

## Pitfalls

- **Token estimation:** the runner estimates token usage when the gateway omits it from the response. Cost math depends on it. See pearl th-eff0d0 commit history.
- **Repo Dolt vs global:** the bench prefers the repo's local Dolt over the global registry. Don't write bench-only state into your global pearls.
- **CMake / `[METRICS]` capture:** C++ tasks need `-DEXERCISM_RUN_ALL_TESTS` and the work dir named after the task. The harness handles this; if you add a new task type, replicate that contract.

## Related

- [[Direct-Mode]]
- [[Architecture-Overview]]
- [[../bench-history]]

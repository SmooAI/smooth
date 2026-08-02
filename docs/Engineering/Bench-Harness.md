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

The harness talks to the same host-process daemon everything else does (`th up`; the microVM mode is gone — [[../Decisions/ADR-004-remove-microvm-sandbox-stack]]). It needs the native `smooth-operative` binary in `target/release/` — build it with `cargo build -p smooth-operative --release` before running.

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

## Agentic conversation suite (`smooth-bench convo`)

Pearl th-f19853. The scored suites above ask "did the agent solve the task?".
This one asks "was Big Smooth any good to talk to?" — the failure mode behind
the ical incident, where three contradictory calendar answers arrived in one
conversation and no single turn was obviously wrong.

An LLM **driver** plays a user across several turns on ONE canonical-protocol
session (same `sessionId` throughout, so the agent's own memory is under test),
then an LLM **judge** grades the whole thread 1–5 on helpfulness, correctness,
tool use, and **consistency across turns**, plus a rubric PASS/FAIL.

```bash
# whole suite — spawns its own `th daemon` on :8791, tears it down after
cargo run -p smooai-smooth-bench -- convo

# one scenario, against a Big Smooth you already have running
cargo run -p smooai-smooth-bench -- convo --only rapid-correction \
  --url http://127.0.0.1:8788 --token "$SMOOTH_LOCAL_TOKEN"

# stochastic — take a rate, not an anecdote
cargo run -p smooai-smooth-bench -- convo --trials 3
```

Driver/judge credentials come from `SMOOAI_GATEWAY_KEY`, falling back to the
first OpenAI-compatible provider in `~/.smooth/providers.json` (the same store
the daemon reads). Scenarios live in `crates/smooth-bench/convo-scenarios.toml`;
transcripts land as JSON-lines in `~/.smooth/bench-runs/convo-*/`.

**Deliberately not in `cargo test`** — every scenario is several live LLM turns.
Only the pure parsing/rendering logic is unit-tested.

### `expect_fail` scenarios

`rapid-correction` fires a correction 1.5s into the first turn — the th-3a912a
interrupt gap — and is marked `expect_fail`. While the gap is open it records
`XFAIL` and the suite stays green; the day interrupts land it records `XPASS`
and exits non-zero, which is the signal to drop the flag and keep it as a
plain regression test.

## Related

- [[Architecture-Overview]]
- [[../bench-history]]

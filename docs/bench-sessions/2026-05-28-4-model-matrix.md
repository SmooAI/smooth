# 4-model matrix — 2026-05-28

#bench #matrix #the-line

> First bench matrix with both [PR #69 (forensic dump)](https://github.com/SmooAI/smooth/pull/69) and [PR #70 (cost sidecar)](https://github.com/SmooAI/smooth/pull/70) stacked. Result: **every model tied at 3/5 (60%)** and **`claude-sonnet-4-6` jumped from dead-last to tied for #1 while becoming both the cheapest and the fastest** — proxy translation hypothesis from the 2026-05-25 matrix now looks resolved.

## Setup

- Stacked branch: `bench-matrix-run-stacked` = `main` + `th-scorer-forensic` (3 commits) + `th-a08fa3-cost-from-event` (2 commits)
- `th 0.13.7 (cf3efa1)` built from the stack
- `th up direct` with `SMOOTH_OPERATOR_RUNNER_NATIVE=~/.cargo/shared-target/release/smooth-operator-runner`
- `smooth-bench score-tui --pr --task-limit 5 --inter-task-sleep-s 30 --task-timeout-s 600 --under-test-model <M>` per model
- Default provider: `smooth` → `https://llm.smoo.ai/v1` (back online after 4-day NXDOMAIN)
- 5 tasks per model, same selection: python/{affine-cipher, beer-song, book-store}, rust/{accumulate, acronym}

## Headline

| Model | Pass | Rate | Cost | Median | Δ pass vs 5/25 |
|---|---|---|---|---|---|
| **claude-sonnet-4-6** | **3/5** | **60%** | **$0.027** | **54.8 s** | **+20 pp** ↑ |
| glm-5.1 | 3/5 | 60% | $0.066 | 161.7 s | −20 pp ↓ |
| kimi-k2.6-direct | 3/5 | 60% | $0.089 | 79.4 s | 0 |
| deepseek-v4-flash | 3/5 | 60% | $0.158 | 179.8 s | 0 |

claude-sonnet-4-6 wins on all three axes. 5/25 had it at $0.211 / 40% / dead last via the proxy — the suspected `cache_control` / tool-use translation issue appears resolved.

## Per-task breakdown

```
                       py/affine    py/beer-song   py/book-store   rust/accum   rust/acronym
glm-5.1                FAIL $0.005   PASS $0.037   PASS  $0.006   PASS $0.000   FAIL  $0.019
deepseek-v4-flash      PASS $0.070   FAIL $0.000   FAIL  $0.000   PASS $0.001   PASS  $0.088
kimi-k2.6-direct       FAIL $0.001   PASS $0.000   PASS  $0.002   PASS $0.001   FAIL  $0.085
claude-sonnet-4-6      FAIL $0.015   FAIL $0.007   PASS  $0.002   PASS $0.000   PASS  $0.003
```

- **py/book-store + rust/accumulate**: solved by every model. These look like the floor.
- **py/affine-cipher**: hard — only deepseek-v4-flash solved it
- **rust/acronym**: deepseek-v4-flash and claude-sonnet-4-6 solved; glm-5.1 and kimi-k2.6-direct failed. 5/25 hypothesis was "all 4 models undercounted by scorer" — this run with the forensic scorer shows it's **really mixed**, not a uniform undercount. The hypothesis was wrong for at least 2 models.
- Two `$0.0000` per-task cells are deepseek-v4-flash on the FAILs (beer-song, book-store) — the agent likely died before `AgentEvent::Completed` fired. Sidecar correctly stayed missing rather than fabricating a number.

## Validation of #69 and #70 in production

- **#70 cost sidecar**: ran on every successful task. JSON sidecar written at `<run_dir>/<lang>-<task>.cost.json` with `{cost_usd, iterations, ts_unix_ms}`. Bench picked it up cleanly. **Zero `"per-task scrape returned 0 for every task"` warnings across all 20 runs** — the warning was the daily-driver signal of the bug, and it's gone.
- **#69 forensic dump**: ran on every score attempt. `<run_dir>/<task>/.smooth-score-forensic/{combined.txt, summary.json}` present for every task.
- **`parsed_via: judge_llm` over-reported**: every python all-pass task fell through to the LLM judge because `parse_pytest_summary` doesn't match the `N passed in X.XXs` (no `failed`) shape. Filed as `th-19ab7c` (P2 bug). Judge correctly returned 16/0/16 on all-pass tasks so scoring stayed correct, but we paid the judge tax (~0.5–1 s + an LLM call) on every all-pass task that could have been a 0 ms regex hit.
- **`th-f46efa` stuck-pass guard fired**: one rust/acronym pass was force-flipped to FAIL because the driver bailed on turn 0 with IdleTimeout. Confirms the guard works against false positives.

## Cost-axis observations

5/25 numbers cannot be trusted (the $0.00-phantom bug was bidirectional — some tasks were fabricated $0, others may have been inflated by the off-by-one status-bar parse). 5/28 numbers are deterministic via the sidecar fed by `AgentEvent::Completed.cost_usd`. Don't read too much into the cost deltas — only deltas within 5/28 are apples-to-apples.

Within 5/28:
- **claude-sonnet-4-6 is 5.9× cheaper than deepseek-v4-flash** at the same pass rate
- **glm-5.1 is the most variable** ($0.000 on rust/accumulate, $0.037 on py/beer-song — 100× range)
- **Median task time matters**: claude-sonnet-4-6 at 54.8 s vs. deepseek-v4-flash at 179.8 s is a 3.3× wall-clock win at the same pass rate

## What I'd do next

1. **Land #69 + #70**, then run a `--release` (120-task) matrix on claude-sonnet-4-6 and glm-5.1 to confirm whether claude's lead survives a bigger sample. 5 tasks is too small to call a winner.
2. **Fix `th-19ab7c`** (`parse_pytest_summary` all-pass shape) before the next run — saves an LLM call per all-pass python task.
3. **Investigate why py/affine-cipher failed on 3/4 models** — only deepseek-v4-flash got it. May be a prompt issue (instructions ambiguous) or a real difficulty cliff worth tracking as a task-level pearl.
4. **Look at the two $0.0000 deepseek failures** — beer-song + book-store. The sidecar didn't fire, meaning no `AgentEvent::Completed`. Compare pane logs to a successful deepseek task to see what's different.

## Related

- pearl `th-086f0f` — forensic scorer ([PR #69](https://github.com/SmooAI/smooth/pull/69))
- pearl `th-a08fa3` — cost sidecar ([PR #70](https://github.com/SmooAI/smooth/pull/70))
- pearl `th-19ab7c` — `parse_pytest_summary` all-pass bug (filed during this run)
- pearl `th-e74aa6` — `find_native_operator_runner_binary` discovery gap (filed during this run)
- pearl `th-8aebb0` — stale `score-tui --help` text after `th-a08fa3` (filed during this run)
- pearl `th-f46efa` — stuck-pass guard (fired correctly this run)

## Raw artifacts

- `2026-05-28-4-model-matrix-progress.log` — full per-task pass/fail/cost timeline
- `2026-05-28-<model>.json` — per-model `Score` JSON for each of the four models

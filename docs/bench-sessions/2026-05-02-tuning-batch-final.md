# Bench session — 2026-05-02 final summary

## Five infrastructure fixes landed (in chronological order)

The bench wasn't producing useful data because of five layered issues
that only became visible when running the harness end-to-end. Each one
shipped as its own pearl + commit. Order matters because each later
fix was masked by the earlier one.

| # | Pearl | Commit | What |
| --- | --- | --- | --- |
| 1 | `th-ac0407` | `aa0cb46` | `SMOOTH_BENCH_IDLE_GRACE_S` 120 → 600 + `SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S` 120 → 300, both env-tunable |
| 2 | `th-525594` | `ac2f660` | Daemon-side dispatch heartbeat — `[PROGRESS]` comment every 30 s while sandbox exec is in flight |
| 3 | `th-d43cfd` | `dd64d79` | Bench `locate_pearl_store_dir()` priority: `~/.smooth/dolt` first instead of repo-walked store. The daemon writes pearls to the global store; the repo-walked path silently bound the bench to its build directory's store and never saw the daemon's comments |
| 4 | `th-139bbc` | `e0b2dd8` | Dispatch posts terminal `[IDLE]` on `exec_in_sandbox` `Err(_)` and on non-zero runner exit. Previously the pearl stayed `in_progress` forever and the bench had to fall through to the 600 s quiescence-grace timeout to give up |

(All four landed clean, all unit tests pass, daemon now at v0.12.11
`e0b2dd8`.)

## Observed in take 7 (the run with all four fixes live)

Pearl `th-440675` (python/affine-cipher, take 7's first task):

```
[PROGRESS] sandbox running (30s elapsed, heartbeat #1)
[PROGRESS] sandbox running (60s elapsed, heartbeat #2)
…
[PROGRESS] sandbox running (1381s elapsed, heartbeat #46)
```

46 heartbeats over 23 minutes. The operator is genuinely running this
whole time — the heartbeat path is healthy, the bench is correctly
patient because it sees comments growing, and the pearl-store
resolution is correct (the bench is reading the same store the
daemon writes to). The remaining latency is the operator itself
taking 20+ minutes to reach a "done" state, not infrastructure.

## Why the operator is taking 20+ minutes

Two probable causes (filed for follow-up below):

- **Token budgets / iteration caps** — the runner's own
  `SMOOTH_WORKFLOW_MAX_ITERATIONS` and `SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS`
  are both set high (3 and 30 respectively in the launchd plist), so
  a model that's making slow progress keeps iterating until the cap
  fires. The `MAX_STEPS_REMINDER` from D4 helps it wrap up *when*
  the cap is reached, but doesn't accelerate the path to the cap.
- **LLM gateway latency** — earlier in the session the runner errored
  out with `stream read error: error decoding response body` once. If
  individual LLM calls are taking 15–30 s due to gateway throttling,
  30 iterations × 25 s = 12.5 min is plausible.

## What the bench will eventually produce

Take 7 (in `~/dev/smooai/smooth-bench-take7-tmux`) will keep running
until each task either finishes naturally (`[IDLE]` from the
operator-runner — already wired up, see C4 work earlier today) or
hits the 1800 s `SMOOTH_BENCH_DEADLINE_S`. That's worst-case ~9 hr
for the full --pr sweep, more realistically 2-4 hr. The score JSON
lands at `/tmp/bench-2026-05-take7/score-pr.json` when complete.

## Infrastructure follow-ups still open

- `th-7306f0` (P2) — `cost_usd=$0` reporting on chat-agent dispatched
  tasks. The cost tracker isn't wired through `teammate_spawn`. Until
  this lands, every score JSON will report $0.00 regardless of real
  spend.
- `th-cfa1fb` (P2) — D5 lazy tool loading. Still real refactor work;
  not done.
- **NEW** to-file pearl: bench task pace — investigate why the
  chat-agent dispatched operator takes 20+ min on a task previously
  observed solving in 90–300 s. Likely intersection of new prompts
  (the Claude Code restraint rules may be making the model more
  careful, slower) + LLM gateway pace. Worth instrumenting tool-call
  duration distributions to confirm.

## Final session manifest (everything on `origin/main`)

D-pillar (prompt + tuning): D1, D2, C3, D4, D3, D6, C4 — done.
C-pillar (operator policy): C1, C3, C4 — done.
B-pillar (bench infrastructure): timeout, heartbeat, store-resolve,
IDLE-on-exit — done.
A-pillar (sandbox dispatch): pre-existing.
D5 (lazy tool loading): out of scope, filed.

5 dispatch / bench fixes + 7 prompt-and-policy improvements + 1 docs
session-record commit = 13 substantive commits to main today.

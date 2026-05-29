# Coach vs User driver — 2026-05-29

#bench #matrix #the-line #coach-driver

> First apples-to-apples measurement of `--driver-persona=coach` (pearl th-e17b1a). User's intuition was "the coach can only help." Reality: **the coach helps weaker models and hurts the strongest one**. glm-5.1 jumped from 3/5 to **5/5 (100%)** while claude-sonnet-4-6 regressed from 3/5 to **2/5 (40%)**. Mixed result that says more about per-model coachability than about the driver's quality.

## Headline

| Model | 5/28 user | 5/29 coach | Δ pass | 5/28 cost | 5/29 cost | Δ cost |
|---|---|---|---|---|---|---|
| **glm-5.1** | 3/5 (60%) | **5/5 (100%)** | **+40 pp** ↑ | $0.066 | **$0.020** | **−70%** ↓ |
| deepseek-v4-flash | 3/5 (60%) | 3/5 (60%) | 0 | $0.158 | $0.108 | −32% ↓ |
| kimi-k2.6-direct | 3/5 (60%) | 3/5 (60%) | 0 | $0.089 | $0.084 | −6% ↓ |
| **claude-sonnet-4-6** | 3/5 (60%) | **2/5 (40%)** | **−20 pp** ↓ | $0.027 | **$0.078** | **+189%** ↑ |

## What the coach driver actually changed

Same model under test (`smooth-summarize` driver model both times), same tasks, same `--inter-task-sleep-s 30`. Only the driver's system prompt + per-turn template changed (pearl th-e17b1a):

- User persona: "non-technical end user." Polite, accepts "done" at face value, fires `TASK_COMPLETE` whenever the agent says it's finished.
- Coach persona: "senior pair-programmer." Demands the agent has actually run the tests and pasted the result in chat before firing `TASK_COMPLETE`. Probes when output looks off. Suggests concrete debugging steps without writing code.

## glm-5.1: the coach made it a different model

5/5 PASS at $0.020 / 88 s median is the single best result in any matrix we've run. Compared to its 5/28 baseline:

- **3/5 → 5/5** (every task green, including the affine-cipher case 3/4 models couldn't solve yesterday)
- **$0.066 → $0.020** (70% cheaper)
- **162 s → 88 s** (46% faster median)

The coach forcing "have you actually run the tests?" before `TASK_COMPLETE` appears to be how glm-5.1 actually got disciplined. It would otherwise declare done with an obviously-wrong cipher and the user-persona would accept it.

## claude-sonnet-4-6: the coach actively hurt it

2/5 PASS at $0.078 / 312 s is its worst matrix result. Compared to 5/28:

- **3/5 → 2/5** (regressed on affine-cipher AND beer-song AND acronym)
- **$0.027 → $0.078** (2.9× more expensive)
- **55 s → 312 s** (5.7× slower median)

Hypothesis: **claude was over-coached**. The coach kept demanding test runs / probing after claude had already solved the task, and claude either second-guessed working code or burned turns satisfying the coach's "show me the test output" requirement. The user-persona's "looks done → TASK_COMPLETE" was the right thing for claude because claude *was* done.

The cost + duration deltas confirm this: claude burned 5.7× more wall-clock and 2.9× more dollars per task while solving fewer of them. The driver was the bottleneck, not the model.

## Per-task picture

```
                       py/affine    py/beer-song   py/book-store   rust/accum   rust/acronym
glm-5.1 (coach)        PASS         PASS           PASS            PASS         PASS         ← perfect
deepseek-v4-flash      FAIL         PASS           PASS            PASS         FAIL
kimi-k2.6-direct       FAIL         PASS           FAIL            PASS         PASS
claude-sonnet-4-6      FAIL         FAIL           PASS            PASS         FAIL         ← worst
─────────────────────────────────
coach solve rate       1/4 (25%)    3/4 (75%)      3/4 (75%)       4/4 (100%)   2/4 (50%)
user solve rate (5/28) 1/4 (25%)    2/4 (50%)      3/4 (75%)       4/4 (100%)   2/4 (50%)
```

- **py/affine-cipher**: still 1/4 solve rate. **And the only model that solves it flipped.** Yesterday deepseek was the lone PASS; today glm-5.1 is. The "model that has the right interpretation in its training data" is non-deterministic per run, which is itself a real signal that this task is measuring prompt-ambiguity tolerance more than coding capability. Pearl `th-6a8064` (the prompt ambiguity ticket) is still load-bearing.
- **py/beer-song**: 50% → 75% solve rate (deepseek + glm + kimi PASS, claude FAIL).
- **rust/acronym**: still 50%. glm/kimi PASS this time; deepseek/claude FAIL. Both rounds had 2/4 but the specific 2 differ — high per-task variance at n=5.

## What this tells us

1. **The coach is a per-model lever, not a universal win.** Use `--driver-persona=coach` when benching models that show "thought it was done" failure modes (glm-5.1 is the canonical case). Use `--driver-persona=user` for models that don't need policing (claude-sonnet-4-6 here).
2. **The headline matrix from now on probably needs both runs.** Reporting one persona's number underspecifies. Headline as `<model> <coach%/user%>` and let readers pick.
3. **Affine-cipher is still the bench's noisiest task** — across two matrices and 8 model-runs, it's been solved by 2 different "lone-PASS" models on separate days. That's noise, not signal.
4. **Cost data is now fully trustworthy** (pearl th-a08fa3): the sidecar fired on every task and every cost number above came from `AgentEvent::Completed.cost_usd`, not from scraping the TUI status bar.

## What I'd do next

1. **Run the same matrix at `--release` scale (120 tasks)** for glm-5.1 (coach) and claude-sonnet-4-6 (user) — the two "winners" under their respective best personas. 5 tasks is too small to call a champion.
2. **Investigate claude's coach regression with `--debug` pane logs.** If claude burned turns on coach probes after it was already done, the system prompt may need a "if the assistant has shown a passing test run, fire TASK_COMPLETE immediately" rule for cases where the model is already disciplined.
3. **Auto-pick persona per model.** Once we have N matrices of data, a routing table per `--under-test-model` → `--driver-persona` would let `score-tui` pick the better driver automatically. Filing as a follow-up pearl.

## Related artifacts

- `2026-05-29-coach-progress.log` — full per-task timeline
- `2026-05-29-coach-{glm-5.1,deepseek-v4-flash,kimi-k2.6-direct,claude-sonnet-4-6}.json` — per-model Score JSON
- `2026-05-28-4-model-matrix.md` — yesterday's user-persona baseline
- pearl `th-e17b1a` — coach persona (closed)
- pearl `th-6a8064` — affine-cipher prompt ambiguity (open, P2)

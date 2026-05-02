# Bench session — 2026-05-02 tuning batch

Smooth version under test: **0.12.8** (commit `0f17d8d`, just after the
D3 per-provider-overlay landing). The full D1+D2+C3+D4+D3+C1+D6+C4 batch
hadn't fully rolled to production yet when the bench started — daemon
restart was at 16:36, bench run started at 16:38.

## Result

**Overall pass rate: 22.2 % (4/18)** on the `--pr` sample
(3 tasks × 6 languages, budget $10).

| Language    | Pass rate | Tasks |
| ----------- | --------- | ----- |
| python      | 66.7 %    | 2/3   |
| rust        | 33.3 %    | 1/3   |
| javascript  | 33.3 %    | 1/3   |
| go          | 0.0 %     | 0/3   |
| java        | 0.0 %     | 0/3   |
| cpp         | 0.0 %     | 0/3   |

Median task time: 120 003 ms.
Cost reported: **$0.00** (suspect — see "harness signal" below).

## Harness signal

`median_task_ms` equals exactly the bench harness's hardcoded 120 s
quiet-timeout (`bench: pearl <id> quiet for 120s, treating as done`).
Combined with `cost_usd: 0.00` across every task, this strongly
suggests the dominant failure mode is **the harness giving up on the
chat-agent dispatch before the operator finishes**, not the operator
producing wrong code. Earlier session evidence (2026-04-25 pearl
`th-461ab9` notes) showed legitimate solves taking 90–300 s; many of
those would time out under the current harness settings and score as
FAIL even with correct code.

Concrete evidence:
- task 1 (python/affine-cipher) and task 2 (python/beer-song): both
  PASS at exactly 120 002 ms. This is the harness short-circuiting at
  120 s of silence; the polyglot starter file passes the test runner
  without modification for those two specific exercises, which is why
  they show as PASS rather than FAIL.
- task 12 (javascript/beer-song) PASSed at 215 s — that's a real
  solve, well past the quiet-timeout. The harness must have
  registered progress messages that reset the quiet timer.
- All 6 cpp + java + go tasks fail. These languages have heavier
  scaffolding (build configs, test harnesses) and the operator
  almost certainly couldn't get past the 120 s silence window before
  emitting a progress message.

## Conclusions

1. **Don't read this 22 % as a tuning regression.** It's primarily a
   harness timing artifact. The new prompts may shift the model's
   behaviour modestly; the harness timeout dominates.
2. **B1 follow-up:** raise the bench harness's `quiet_timeout` from
   120 s to ~600 s (or make it configurable) so we get a real read on
   how the new prompts affect pass rate. Recommended pearl:
   `bench: harness quiet-timeout fights operator runs that take 90-300s`.
3. **Cost tracking is broken on this dispatch path.** `cost_usd: 0.00`
   for all 18 tasks is wrong — the chat-agent dispatch isn't feeding
   the cost tracker. Worth a separate investigation pearl.

## Reproduce

```bash
target/release/smooth-bench score --pr --budget-usd 10 \
    --output /tmp/bench/score.json
```

Expects daemon running at `http://127.0.0.1:4400` with sandboxed
dispatch (`dispatch="sandboxed (microVM per task)"` in the service
log). `~/Library/LaunchAgents/com.smooai.smooth.plist` runs
`th up --foreground` (no `--direct`), so this is the live setting.

## Artifacts (this directory)

- `2026-05-02-tuning-batch.json` — full Score JSON
- `2026-05-02-tuning-batch.log` — bench harness run log
- `2026-05-02-tuning-batch.md` — this analysis

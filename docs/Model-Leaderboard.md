# Model Leaderboard

#engineering

> [!info] Which model should Smooth run?
> Scored by `smooth-bench agentic` on 28 scenario(s), 3 trial(s) each.
> Regenerate with `smooth-bench agentic --model A --model B --scoreboard board.json`
> then `scripts/the-line/render-model-scores.sh board.json`.

| model | pass | rate | cost | $/pass | time | safety |
| --- | --- | --- | --- | --- | --- | --- |
| `gpt-5.6-luna` | 25/28 | 89.3% | $0.01333 | $0.000533 | 1471.9s | ⚠️ 3 |
| `deepseek-v4-pro` | 25/28 | 89.3% | $0.013686 | $0.000547 | 1190.8s | ⚠️ 2 |
| `gemini-3.6-flash` | 24/28 | 85.7% | $0.003837 | $0.00016 | 1459.4s | ⚠️ 2 |
| `gpt-5.6-sol-high` | 24/28 | 85.7% | $0.4782 | $0.019925 | 1625.8s | ⚠️ 1 |
| `gpt-5.5` | 24/28 | 85.7% | $10.212845 | $0.425535 | 1301.0s | ⚠️ 2 |
| `qwen-3.7-max-direct` | 23/28 | 82.1% | $0.103703 | $0.004509 | 1449.1s | ⚠️ 2 |
| `gpt-5.4` | 23/28 | 82.1% | $4.61391 | $0.200605 | 930.9s | ⚠️ 2 |
| `gemini-3.5-flash` | 22/28 | 78.6% | $0.00404 | $0.000184 | 1227.2s | ⚠️ 4 |
| `glm-5.2-direct` | 22/28 | 78.6% | $0.095 | $0.004318 | 3270.9s | ⚠️ 2 |
| `claude-fable-5` | 19/25 | 76.0% | $0.74965 | $0.039455 | 1903.5s | ⚠️ 4 |
| `deepseek-v4-flash` | 21/28 | 75.0% | $0.024831 | $0.001182 | 1429.5s | ⚠️ 2 |
| `kimi-k2.7-code-direct` | 21/28 | 75.0% | $0.04804 | $0.002288 | 1506.2s | ⚠️ 3 |
| `claude-sonnet-5` | 21/28 | 75.0% | $0.17471 | $0.00832 | 1757.4s | clean |
| `groq-gpt-oss-20b` | 5/28 | 17.9% | — | — | 5649.4s | ⚠️ 5 |

## Reading this

- **Percentages are per suite.** `convo` and `agentic` measure different
  things; do not compare a number here against one from the other suite.
- **A missing cost (—)** means the run could not price the model at all
  (the gateway publishes no rate for it). It is not $0 — a zero would
  sort first and win a value ranking it never earned.
- **safety** counts trials where the agent breached a safety invariant:
  destroyed data it was told to protect, or leaked a secret. It is
  deliberately separate from the pass rate. A model can fail a scenario
  for skipping a required note while having protected the data perfectly
  — reading the rate as a safety score gets that exactly backwards.
- Cheap models have repeatedly matched expensive ones here. Check `$/pass`,
  not just the rate, before promoting anything into the premium tier.

## Related

- [[Engineering/Bench-Harness]] — how the suites work
- [[Engineering/LLM-Request-Parameters]] — why a model can score 0% for a reason that isn't quality

# Model Leaderboard

#engineering

> [!info] Which model should Smooth run?
> Scored by `smooth-bench convo` on 15 scenario(s), 3 trial(s) each.
> Regenerate with `smooth-bench convo --model A --model B --scoreboard board.json`
> then `scripts/the-line/render-model-scores.sh board.json`.

| model | pass | rate | cost | $/pass | time |
| --- | --- | --- | --- | --- | --- |
| `deepseek-v4-flash` | 0/0 | 0.0% | — | — | 4.7s |
| `gpt-5.5` | 0/0 | 0.0% | — | — | 4.7s |

## Reading this

- **Percentages are per suite.** `convo` and `agentic` measure different
  things; do not compare a number here against one from the other suite.
- **A missing cost (—)** means the run could not resolve spend above the
  shared gateway key's background traffic. It is not $0.
- Cheap models have repeatedly matched expensive ones here. Check `$/pass`,
  not just the rate, before promoting anything into the premium tier.

## Related

- [[Engineering/Bench-Harness]] — how the suites work
- [[Engineering/LLM-Request-Parameters]] — why a model can score 0% for a reason that isn't quality

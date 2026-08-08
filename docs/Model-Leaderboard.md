# Model Leaderboard

#engineering

> [!info] Which model should Smooth run?
> Scored by `smooth-bench convo` on 9 scenario(s), 1 trial(s) each.
> Regenerate with `smooth-bench convo --model A --model B --scoreboard board.json`
> then `scripts/the-line/render-model-scores.sh board.json`.

| model | pass | rate | cost | $/pass | time |
| --- | --- | --- | --- | --- | --- |
| `deepseek-v4-flash` | 8/9 | 88.9% | $0.0188 | $0.0024 | 204.1s |
| `gpt-5.5` | 8/9 | 88.9% | $0.7948 | $0.0994 | 145.4s |
| `claude-sonnet-5` | 7/9 | 77.8% | $0.0615 | $0.0088 | 192.4s |
| `minimax-m2.7` | 7/9 | 77.8% | $0.0836 | $0.0119 | 195.4s |

## Reading this

- **Percentages are per suite.** `convo` and `agentic` measure different
  things; do not compare a number here against one from the other suite.
- ⚠️ **1 trial per scenario** — agent behaviour is stochastic, so a
  one-scenario gap between two models is noise, not a ranking. Re-run with
  `--trials 3` before acting on a close result.
- **A missing cost (—)** means the run could not resolve spend above the
  shared gateway key's background traffic. It is not $0.
- Cheap models have repeatedly matched expensive ones here. Check `$/pass`,
  not just the rate, before promoting anything into the premium tier.

## Related

- [[Engineering/Bench-Harness]] — how the suites work
- [[Engineering/LLM-Request-Parameters]] — why a model can score 0% for a reason that isn't quality

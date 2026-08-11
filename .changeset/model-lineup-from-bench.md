---
'@smooai/smooth': patch
---

Smooth Modes: repick every model from the 3-trial benchmark

The lineup was last set from single-trial runs and, in two slots, from
reputation rather than measurement. `smooth-bench agentic` at 28 scenarios
x 3 trials (a scenario passes only if every trial passed) says:

**The best two models in the lineup are also among the cheapest.**
`gpt-5.6-luna` and `deepseek-v4-pro` both score 89.3% at $0.0005 per
passing scenario. Flash moves to luna from `deepseek-v4-flash` (75.0%) —
better and cheaper, for the mode every session starts in.

**`groq-gpt-oss-20b` is retired.** It scored 17.9%, a quarter of the
next-worst model, breached safety in 5 trials, and — in a slot named Fast —
was the slowest model measured, by 3.5x. It had been chosen for Groq's
reputation for speed and never benchmarked.

**Premium drops from five slots to three.** With cost measured properly
(tokens at the gateway's published rate, not a shared key's spend delta),
`gpt-5.5` scores 85.7% — below the free-tier default — at $10.21 against
luna's $0.013 for the same suite. `flash+` and `plan+` are dropped;
`gemini-3.5-flash` moves down to budget `fast` where its price belongs,
and `gpt-5.6-sol-high` takes `max` from the never-benchmarked
`gpt-5.5-pro`, matching gpt-5.5's score at 1/21st the cost per pass.

**Smoo Jr moves to `claude-sonnet-5`** — the only model with a clean
safety record across 84 trials. It also scores lowest of the working
models, and the two facts are related: it refuses and asks where others
act. For a child's session that trade is the right way round.

Saved preferences pointing at a retired slot fall back to Flash.

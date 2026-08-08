---
'@smooai/smooth': patch
---

bench: weekly model-leaderboard workflow

Publishes the model leaderboard on a schedule instead of whenever someone remembers
to run it. Deliberately multi-trial: at `--trials 1` the suite could not tell
deepseek-v4-flash and gpt-5.5 apart at all — every apparent difference was noise on
one known-flaky scenario. A published number from a single trial is an anecdote
wearing a percentage sign.

Weekly rather than nightly because 3 trials across the suite costs real LLM spend.
`workflow_dispatch` takes models/trials as inputs and gates the commit behind an
explicit `publish` flag, so you can score a candidate model without touching the
published board.

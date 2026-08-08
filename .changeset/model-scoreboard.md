---
'@smooai/smooth': patch
---

bench: publish a per-model benchmark percentage (README badge + leaderboard)

`smooth-bench convo|agentic --scoreboard board.json` emits a pre-rounded scoreboard;
`scripts/the-line/render-model-scores.sh` turns it into `docs/model-scores.json`,
`docs/model-badge.json` (a README shields endpoint) and `docs/Model-Leaderboard.md`.

Separate from The Line's badge on purpose: The Line tracks one model over time, this
ranks models against each other. Folding them together would make a routing change
look like a quality regression.

The renderer refuses to publish an unmeasured cost as `$0` (renders `—`) and warns
when a board came from a single trial, since a one-scenario gap is noise.

First published board (convo, 9 scenarios, 1 trial): deepseek-v4-flash and gpt-5.5
both 88.9%, at $0.019 vs $0.795 — the budget model matches the premium one at 1/42nd
the cost.

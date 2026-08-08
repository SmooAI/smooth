---
'smooth': patch
---

bench: publish the first model leaderboard, and fix two reporting bugs the real data exposed

First multi-trial board with working cost measurement (2 models × 15 scenarios × 3 trials):
gpt-5.5 72.1% at $5.25, deepseek-v4-flash 71.1% at $0.14 — a statistical tie on quality at
**39× the cost per passing scenario**. That is the number the whole exercise existed to produce.

Running it for real surfaced two bugs in the publishing path: raw f64 rendered as
`$5.249943000000227` in the docs table, and `scenario_count` counted trials rather than
distinct scenarios (a 15-scenario suite at 3 trials published as "45 scenarios"). Both
pre-rounded/deduped at the source so the badge, table and JSON cannot disagree.

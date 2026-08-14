---
'@smooai/smooth': patch
---

Show the benchmark board in the README instead of only linking to it.

The README carried a shields badge pointing at `docs/Model-Leaderboard.md`, so the numbers were a
click away and invisible on the landing page. This is a public repo and the measured cost-vs-capability
table is the strongest thing on it: `gpt-5.6-luna` finishes more scenarios than `gpt-5.5` for 1/766th
the cost.

Adds a compact 9-model table to the Model routing section — pass rate, cost per run, cost per pass,
and safety violations — with the three caveats that keep it honest: cost-per-pass is the column that
decides routing, safety is scored independently of pass rate, and an unpriced model renders as
unknown rather than free.

Hand-typed numbers in a file that regenerates weekly rot silently, so `check-readme-board.py`
verifies the table against `docs/model-scores.json` (comparing at the precision the README prints,
since the values span four orders of magnitude) and runs as part of `test-model-scores.sh`.

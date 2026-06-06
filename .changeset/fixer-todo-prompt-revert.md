---
"smooai-smooth": patch
---

fixer.txt: revert todo_list teaching section (regressed v4-pro 3/3 → 0/3)

The "Multi-turn tasks: use `todo_list`" section added in
th-1d6699's commit hurt every model tier tested:

- deepseek-v4-pro: 3/3 perfect → 1/3 partial (0.8) + 2/3 must_preserve
  violations (0.35)
- deepseek-v4-flash: agent hallucinated "tool not in allowlist"
  excuses, didn't actually call the tool

Post-revert v4-pro is back to 3/3 perfect (3,559,751 / 3,559,751 /
3,557,724 bytes freed). The TodoListTool itself stays — it's
architecturally correct and ready for stronger models to pick up
organically. The prompt-injection approach was too prescriptive
and conflicted with the existing destructive-plan discipline. Pearl
th-1d6699 remains in_progress for a re-attempt that demonstrates
the tool via a concrete example rather than a 24-line procedural
sermon.

---
'@smooai/smooth': patch
---

bench: score tool usage, not just pass rate

Pass rate hides how a model works. Two models can both score 100% while one takes
three tool calls per turn and the other twelve with a third erroring — and that is the
difference between an agent you can leave running and one you cannot.

The leaderboard now carries a tool-use block: total calls, error rate, calls per turn,
the judge's 1–5 `tool_use` axis (which was already being captured and then discarded),
and turns that made no tool call at all. That last one catches a model answering from
memory on a suite whose tasks all need tools.

Objective counts sit alongside the judged axis deliberately: a grader looking at the
outcome can call twelve calls and four errors "good tool use". Error rate does not
flatter anyone.

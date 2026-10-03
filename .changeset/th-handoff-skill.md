---
'@smooai/smooth': minor
---

A `th-handoff` skill in the smooth-agent plugin moves a session's work to a fresh session without losing it. `/th-handoff` writes the state into one pearl via `th pearls checkpoint`: what's done, what's open in order, what's blocked on the user, the approvals already given, and which agents won't carry over. Before writing the final note, it makes every live worker push its branch and append to that pearl, then reads the handoff back. `/th-handoff resume <pearl>` is the receiving side: it re-verifies every PR and worktree before acting, and trusts nothing it hasn't re-checked.

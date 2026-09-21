---
'@smooai/smooth': patch
---

SmoothFlow 0.2.5

- **Crash cards show the real exit.** SmoothFlow records each agent's exit code itself instead of relying on tmux, which on some versions misses a fast-exiting process entirely and leaves it unreaped. Crash cards no longer read "exit -1"; a signal kill shows as "killed by signal N" and resumes, and a genuinely unknown exit says so and is **not** auto-resumed — so an agent you quit on purpose is never resurrected as a crash (#620).
- **Approving works for every agent.** Permission prompts now use each agent's own keys, so Approve/Deny from SmoothFlow works for Aider, Goose, Crush and Cline, not just Claude Code (#613).
- **Hook endpoint is authenticated.** Each agent launch gets its own token, so no other local process can forge agent state or approve a permission request on an agent's behalf (#611).
- **Phones reconnect after sign-in.** The relay re-authenticates when Smoo credentials arrive, instead of staying connected-but-unauthenticated and looking "offline" when the app started before login (#610).
- **Diff and PR tabs for shell sessions** in a worktree, gated on the branch rather than the session kind (#612).
- `th harness doctor` flags a Claude Code plugin pinned to an old version at project scope (#613).

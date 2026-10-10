---
'@smooai/smooth': minor
---

`/th-clear` (SMOODEV-3759): hand a harness session off and continue in a fresh context in the same terminal. The new th-clear skill checkpoints the pearl and runs `th harness handoff arm`; after the user's `/clear`, a SessionStart hook (`th harness handoff claim`) injects the handoff automatically. It does nothing unless armed: a one-shot token keyed on the directory and the harness process. `th harness budget` meters the context from the transcript and nudges once at `harness.context_budget_warn` (220k), then blocks the turn's Stop once at `harness.context_budget` (250k) so the agent hands off.

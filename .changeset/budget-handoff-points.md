---
'@smooai/smooth': patch
---

The context budget hook suggests `/th-clear` only when work is finished (a merge or a pearl close), once per context segment, and never stops a session mid-task. Auto-compaction keeps long sessions going; past the budget in compact mode the agent checkpoints and keeps working.

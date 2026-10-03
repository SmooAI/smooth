---
"@smooai/smooth": patch
---

th-handoff: the handoff ends with a copy-pasteable launch command for the harness the user opens next (Claude Code, Codex, OpenCode), and the plain resume prompt for `th code` or any harness without a prompt argument.

Resume without an id: `/th-handoff resume` lists in-progress pearls with checkpoints, newest first (`--cwd .` narrows to this repo), and resumes the one the user picks.

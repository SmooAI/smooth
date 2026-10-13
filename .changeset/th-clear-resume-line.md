---
'@smooai/smooth': patch
---

`th harness handoff arm`, the context-budget hook and `/th-clear` now hand the user an explicit, copy-pasteable pair, `/clear` then `/th-handoff resume <pearl>` (`/new` and `$th-handoff` in Codex), instead of "type /clear, then go". The armed token still resumes on its own when the SessionStart hook claims it; the resume line is the fallback when it doesn't.

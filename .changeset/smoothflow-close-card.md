---
'@smooai/smooth': patch
---

SmoothFlow macOS: the finished-session inbox card sends `flow.close` (th-883ce9). **Close…** opens a confirm sheet naming what will happen — close pearl `<id>`, remove worktree `<path>` + delete branch `<branch>` (each a toggle; the main checkout is never offered) — and the frame carries a client `seq` the engine echoes as `flow.error.ref`, so a refusal (dirty / unmerged worktree, nothing touched) lands on that card with **Force close**. The mock server answers `flow.close` (+ `POST /api/flow/sessions/{id}/close`) and refuses an unmerged fixture until forced; XCUITests cover both paths.

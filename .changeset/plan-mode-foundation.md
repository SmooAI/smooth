---
'@smooai/smooth': minor
---

Big Smooth: real Plan ⇄ Auto execution modes (daemon + tools foundation). Adds a per-conversation `SessionModes` store and a `GET`/`POST /api/session/mode` route so a face can flip a conversation between **Plan** (read-only) and **Auto** (execute). In Plan mode the daemon's tool provider filters every mutating tool out of the per-turn set (deny-by-default, mirroring the family RBAC filter) — the model literally cannot obtain `edit_file`/`bash`/`send_file`/… — so Plan mode is a hard read-only guarantee. Two new tools ride the existing directive sink: `present_plan` (surface a proposed plan for the user to accept → flips to Auto and executes, or revise) and `todo_write` (maintain a live task list). Persona updated to teach both modes and tools. Face wiring (shift+tab in th code, Plan/Auto chip on desktop + mobile, present-plan/todo rendering) follows in subsequent PRs. Part 1 of the coding-harness glow-up.

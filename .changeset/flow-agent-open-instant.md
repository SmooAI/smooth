---
'@smooai/smooth': patch
---

SMOODEV-4267: A new SmoothFlow agent's row and tab appear immediately, in `starting`, before its pearl worktree is made and before inference runs. A failure to make the worktree or launch shows on the row. Clients can attach, resize and type during the launch. `POST /api/flow/sessions` takes `wait: true` for callers that need the process before the reply, which `th flow new` uses. Agents read as ready sooner, and a pasted prompt lands sooner, because a booting agent is checked every 200 ms. Harness manifests are cached until a manifest file changes.

---
'@smooai/smooth': patch
---

SmoothFlow: `flow.close {id, close_pearl, remove_worktree, force}` (+ `POST /api/flow/sessions/{id}/close`, `th flow close`) finishes a session for good — closes its pearl through `th pearls close`, removes the worktree and branch once the branch is merged (ancestry or a merged PR; squash merges count), drops the row and broadcasts `flow.session.removed`. A dirty or unmerged worktree is refused with nothing touched unless `force`; the main checkout is never removed (th-e126cc).

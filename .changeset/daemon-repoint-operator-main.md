---
'smooai-smooth-daemon': patch
---

EPIC th-c89c2a: the local-flavor seams landed on smooth-operator `main` (#108),
so the daemon's operator path-deps (`smooth-operator-server`,
`smooth-operator-svc`) now point at the canonical `../smooth-operator` checkout
instead of the temporary `smooth-local-flavor` worktree (now removed). Same
local-dev two-repo path pattern as the engine dep. (Closes th-845d79; full
CI-mergeability still needs git-rev/published deps for the engine + operator —
th-d3537a.)

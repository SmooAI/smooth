---
'@smooai/smooth': minor
---

Family AI M2.5 — per-role calendar allowlist (within-tool resource RBAC).

M1/M2 gated tools whole (a role has the `calendar` tool or it doesn't). M2.5 adds
the next grain: a parent can restrict WHICH calendars a `role:child` principal may
see and touch. `RoleProfile` gains an optional `calendars` allowlist (fail-closed:
absent ⇒ all, `[]` ⇒ none, populated ⇒ only those), bound onto the `calendar` /
`calendar_delete` tool instances at construction from the authenticated role —
never a tool argument, so a child can't widen it. Reads are bound to the allowed
set via injected `-c` flags, out-of-set calendars are rejected on read/add/update,
`add` must target an allowed calendar, and the calendar listing is filtered to the
allowed names. Delete/show/update-in-place carry no verifiable calendar target and
lean on the read boundary (documented ceiling).

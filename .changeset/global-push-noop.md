---
"@smooai/smooth": patch
---

`th pearls push/pull` is a no-op on the global store

Project pearl stores are designed to sync via Dolt remotes
(per-project board for the team). The global store at
`~/.smooth/dolt` holds personal-scope state (sessions, memories,
private pearls) and isn't meant to sync — making `th pearls push`
fail there with "no configured push destination" was just noise.

Now `th pearls push/pull` from the global store prints a one-line
informational message and exits 0 instead of erroring. Project
stores still surface the error so a missing remote on a shared
board is obvious.

Detection: canonical-path comparison against `~/.smooth/dolt`.
Error matching is heuristic (looks for "no configured push
destination", "no upstream", "remote not found", etc.) so
unrelated SQL/lock errors still propagate.

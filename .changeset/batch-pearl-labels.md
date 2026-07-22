---
'@smooai/smooth': patch
---

smooth-pearls: kill the N+1 label query in list-style reads. `ready`/`list`/
`blocked`/`search`/`due_scheduled` each fetched labels with one Dolt query
**per pearl** — every `th prime` / `th pearls ready` at session start cold-booted
Dolt ~40 times (~5.7s). A single `WHERE pearl_id IN (…)` batch collapses that to
2 queries: `th pearls ready` against a 1200-pearl store drops from 5.7s to ~0.7s.

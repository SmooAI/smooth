---
'@smooai/smooth': minor
---

th pearls: schedulable pearls that speak up when due (th-01aa6a)

Pearls gain an optional `scheduled_at`. `th pearls schedule <id> <when>` sets it
(relative `+2h`/`30m`/`2d`/`1w`/`tomorrow`/`now` or absolute `2026-07-10 09:00`/RFC3339,
UTC); omit `<when>` to clear. `th pearls due` lists pearls whose time has arrived, and
the session-priming hook (`th prime`) now surfaces a `⏰ Scheduled & due` section above
`Ready to work` so a scheduled pearl automatically speaks up at the next session start
once it comes due. `th pearls show` / `ready` / `list` render a `⏰` marker for
scheduled pearls. Existing Dolt stores migrate idempotently (`ADD COLUMN IF NOT EXISTS`).

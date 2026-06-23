---
'smooai-smooth-daemon': patch
---

Phase 5 (EPIC th-c89c2a): durable schedule store. Adds a `ScheduleStore`
trait (`upsert`/`list`/`due(now)`/`delete`) with an in-memory backend and a
SQLite-backed `SqliteScheduleStore` over a new `schedules` table, sharing the
daemon's connection — so scheduled tasks survive a restart. `due` narrows to
enabled rows in SQL then applies the precise `is_due` `DateTime` check in Rust
(avoiding rfc3339 fractional-second string-compare edges). Wired onto
`Stores` and `AppState`. Tested: in-memory upsert/list/due/delete +
disabled-exclusion, and SQLite persist-across-reopen with kind/timestamp
round-trip. The scheduler tick loop + dispatch + the `th`/API surface follow.

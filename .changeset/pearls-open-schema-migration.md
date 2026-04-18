---
"@smooai/smooth": patch
---

Fix "Dolt store" showing red on the dashboard while green on
`th status`. Pre-existing pearl stores were created before the
`config` table was part of the schema (added in the retire-sqlite
change), and only `PearlStore::init` ran `ensure_schema`. `open()`
skipped it entirely, so `get_config("__health_check")` in the health
handler ran `SELECT v FROM config WHERE k = ...` against a missing
table, failed, and flipped `database.status` to `"down"`.

`PearlStore::open` now runs an idempotent schema-migration check: a
single `SHOW TABLES` query against the open store; if any required
later-added table is missing, it re-runs the full `CREATE IF NOT
EXISTS` pass and commits. On an up-to-date store it's a single
round-trip. Concurrent migrators are safe — duplicate commits are
logged and swallowed.

Added a regression test
(`test_open_migrates_missing_config_table`) that simulates a legacy
store by dropping `config`, reopens via `open()`, and verifies
`get_config` / `set_config` work without error.

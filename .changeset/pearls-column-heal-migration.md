---
'@smooai/smooth': patch
---

Fix pearls schema migration: `scheduled_at` / `tool_calls` never added to pre-existing stores (th-eba7b4)

`pearls.scheduled_at` (th-01aa6a) and `session_messages.tool_calls` (th-880f2c)
shipped their migration as `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, which the
embedded Dolt engine rejects as a syntax error — the failure was swallowed by
`let _ = …`, so the columns were silently never added to existing databases and
`th pearls due` errored `table "p" does not have column "scheduled_at"`. Both
now heal through the `column_exists`-gated `COLUMN_HEALS` loop in
`migrate_schema` (the same proven pattern as `pearl_comments.seq`), with a
regression test that drops the columns and asserts a reopen re-adds them.

---
'smooai-smooth-pearls': patch
---

Fix pearl_comments.seq column-level heal that silently never applied. Dolt has no `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, so the original heal errored on every open and the failure was swallowed as a debug log — pre-messaging stores kept a seq-less `pearl_comments` and `th pearls show` blew up with "column seq could not be found". migrate_schema now probes `information_schema.columns` via a new `column_exists` helper and runs the bare `ALTER` only when the column is genuinely absent. (pearl th-f89a3c, surfaced restoring the smooblue store.)

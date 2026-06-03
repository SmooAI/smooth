---
'@smooai/smooth': minor
---

Harden `th jira sync` against the duplication + silent-failure class
that produced ~1,400 duplicate pearls (th-df54c4, th-5a18e6, th-ce1d85).

**Storage invariant.** Pearls now carry a nullable `jira_key` column with
a `UNIQUE` index. Duplicate Jira-linked pearls are now physically
impossible — a second pearl claiming the same key fails the insert/link
instead of silently being created. NULLs are exempt, so non-Jira pearls
are unaffected. Existing stores are healed on open (column + index added
via an `information_schema`-gated migration, since Dolt rejects
`ALTER TABLE ... ADD COLUMN IF NOT EXISTS`).

**Idempotent sync.** The pull phase now decides "create vs. skip" with an
indexed `get_by_jira_key` lookup instead of `title.contains(key)` against
a list capped at 100 (the root cause: any ticket past the first 100 was
never seen as tracked and got recreated every run). New tickets are
created via `create_for_jira`, which sets the key in one atomic INSERT so
the UNIQUE index guards at creation time. Re-running a sync is now a
no-op.

**No more title mangling.** The push phase records the Jira key in the
`jira_key` column instead of rewriting the pearl title to
`SMOODEV-XXXX: …`. It skips already-linked pearls (and legacy
title-prefixed ones for back-compat).

**Fail loud.** Sync now exits non-zero and prints a failure count when any
ticket fails (Dolt lock contention, etc.), instead of exiting 0 behind a
clean "N pulled" summary — which is how 26 lock failures snowballed into
~1,400 duplicates across re-runs.

New `PearlStore` methods: `get_by_jira_key`, `set_jira_key`,
`create_for_jira`. `Pearl` gains a `jira_key: Option<String>` field.

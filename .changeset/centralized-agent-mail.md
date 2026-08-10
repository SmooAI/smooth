---
'@smooai/smooth': minor
---

Agent mail moves off per-repo Dolt onto a machine-level SQLite store.

`th agent` and `th msg` used to live in the pearl store, so the mailbox you got
depended on which worktree you were standing in — agents on the same repo
routinely could not reach each other. Dolt's single writer also wedged the whole
store under concurrent agents (`Error 1105: database is read only`), and every
send paid a ~0.7s cold boot plus a git push for a message that matters for
minutes. Mail now lives in one SQLite file per machine (`~/.smooth/mail.db`), so
every agent on the host shares one mailbox, sends are instant local writes, and
concurrent writers just queue.

Along with the move: read state is per-recipient (acking a broadcast no longer
consumes everyone else's copy), messages carry a type
(`note|request|result|handoff|cancel`) and priority, agents publish presence
(`idle|working|waiting|offline` plus a task and their repo/worktree/branch) and
are reaped to `offline` when their process dies. New: `th msg send <to>
<body...>` positional form, `th msg ack [--all]`, `th msg watch --once`, `th
agent whoami`, `th agent status`, `th agent claim`, and `agents`/`msgs` aliases.
`--no-push`/`--pull`/`--no-pull` still parse but are no-ops. Old per-repo
mailboxes are not migrated. See ADR-010.

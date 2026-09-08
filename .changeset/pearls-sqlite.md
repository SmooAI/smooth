---
'@smooai/smooth': minor
---

`th pearls` now stores every project's pearls in one machine-global SQLite file (`~/.smooth/pearls.db`) instead of an embedded Dolt database per project (pearl th-d3e842). Why: Dolt's history and branching were never used, its single-writer lock wedged the store under parallel agents, every read cost ~0.7s of cold boot, `NOW()` returned local time, `ADD COLUMN IF NOT EXISTS` was a syntax error, and pearls created inside a linked git worktree vanished with it. Rows are keyed by the canonical project root (the main checkout even from a worktree), so all of those go away and `th pearls ready` answers in milliseconds.

- `th pearls migrate-from-dolt [PATH]` imports a legacy `.smooth/dolt` store (all tables, ids and timestamps preserved, idempotent, Dolt dir untouched). `dolt.rs`/`dolt_server.rs`/`go/smooth-dolt` survive only for this command and are deleted in th-c6ba83.
- `th pearls init` is now "ensure db + register project"; `push`/`pull` print an exit-0 notice (sync is th-ddce81); `log`, `remote`, `gc`, `doctor`, `migrate-from-beads` are removed.
- `~/.smooth/registry.json` drops entries whose path no longer exists.
- The dead Dolt `Mailbox`/`AgentRegistry` types (superseded by `mail.db`, ADR-010) are removed.

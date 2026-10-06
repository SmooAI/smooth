# Data Layout

#architecture

> [!info] From CLAUDE.md
> Where pearls, mail, auth, providers and plugins live on disk, plus the pearl/Jira workflow reference. Moved verbatim out of `CLAUDE.md` §5–§6; see also [[Architecture/Data-Storage]] and [[Architecture/Pearls]].

## 5. Data

### Pearls (`~/.smooth/pearls.db`)

Pearl data for **every project on the machine** lives in one SQLite file,
`~/.smooth/pearls.db` (WAL; `$SMOOTH_PEARLS_DB` overrides — tests point it at
a tempdir). Every row carries a `project` column = the canonical project root,
resolved from any cwd as the **main checkout** even inside a linked git
worktree (`git rev-parse --git-common-dir`'s parent), so pearls created in a
worktree no longer vanish with it. Pearl th-d3e842 replaced the embedded
per-project Dolt store (deleted in th-c6ba83): reads went from ~0.7s to ~10ms, concurrent agents
queue on SQLite's lock instead of wedging "database is read only", and
`ADD COLUMN IF NOT EXISTS` / `NOW()`-timezone footguns are gone.

Tables: `pearls`, `pearl_dependencies`, `pearl_labels`, `pearl_comments`,
`pearl_history`, `memories`, `config` — all keyed `(project, …)`. Ids stay
`th-xxxxxx` and are unique per project. Timestamps are fixed-width UTC RFC3339
text; queries compare against a Rust `Utc::now()` literal, never SQLite `now`.

> **Dolt is gone** (PR #522 retired it, pearl th-c6ba83 deleted the shim).
> A straggler machine with a legacy `.smooth/dolt` store must run
> `th pearls migrate-from-dolt` with **th ≤ 0.42.x** BEFORE upgrading; newer
> builds cannot read it. `th pearls push` / `pull` print a notice and exit 0 —
> cross-machine sync against Smoo Projects is pearl th-19cca5.

### Global (`~/.smooth/`)

- `pearls.db` — Every project's pearls (SQLite; see above)
- `registry.json` — Multi-project registry. A plain store open registers the project only when its root is a git repo other than `/` or `$HOME` (hooks open the store from any cwd, so scratch dirs are not projects — th-92e046); `th pearls init` registers any directory explicitly. Entries whose path is gone, or that are not git repos unless explicit, are pruned on open
- `smooth.db` — Legacy SQLite. No migration command ships any more (`th pearls migrate-from-sqlite` was removed); the file is unread and safe to delete.
- `mail.db` — Agent mail + the agent roster (SQLite; `$SMOOTH_MAIL_DB` overrides). Machine-level on purpose — see [ADR-010](../Decisions/ADR-010-centralized-agent-mail.md)
- `agent-sessions/<session_id>` — Handle each harness session registered under (written by the smooth-agent SessionStart hook, rewritten by `th agent claim`/`rename`)
- `audit/` — Rotating tool usage logs per actor
- `providers.json` — LLM credentials
- `auth/` — **legacy** Smoo AI session tree (pre-SMOODEV-1739). Live sessions moved to `~/.config/smooth/auth/` (see §1a); these files remain only as a migration backup.
- `mcp.toml` — MCP server configs (see `docs/extending.md`)
- `plugins/<name>/plugin.toml` — CLI-wrapper tool manifests

### Project-scoped (`<repo>/.smooth/`)

- `mcp.toml` — Project-specific MCP servers; merged with global,
  project wins on name collision
- `plugins/<name>/plugin.toml` — Project-specific plugins; same
  merge rules

---

## 6. Pearl Tracking — SQLite + Jira Integration

**Philosophy**: Built-in pearl tracking (`th pearls`) is the primary work
tracker. Jira (SMOODEV project) is the external source of truth for project
management.

**Pearls is the only spelling.** There are no `th issues` or `th beads`
aliases.

**Storage**: one SQLite file, `~/.smooth/pearls.db`, for every project
(see §5). `~/.smooth/registry.json` tracks all projects. Run `th pearls` from
anywhere inside a repo — worktrees included — and it hits that repo's project.

**Naming lineage**: beads → issues → pearls.

### Quick reference

```bash
th pearls init                        # Ensure pearls.db exists + register this project
th pearls create --title="Title" --description="..."
th pearls list --status=open          # All open pearls
th pearls list --status=in_progress   # Active work
th pearls show <id>                   # Pearl details with dependencies
th pearls update <id> --status=in_progress   # Claim work
th pearls close <id1> <id2> ...       # Close completed pearls
th pearls ready                       # Show ready pearls (open, no blockers)
th pearls checkpoint <id> --note "…" --next "…"   # Record a handoff checkpoint (worktree/branch/HEAD/dirty auto-collected)
th pearls show <id> --handoff [--json]   # Handoff packet: what / where / what happened / next
th pearls prime --in-progress [--cwd .]  # Handoff packets for in-progress pearls (this worktree's with --cwd)
th pearls blocked                     # Show blocked pearls
th pearls projects                    # List all registered pearl projects
th pearls push / pull                 # Exit-0 notice — sync is pearl th-19cca5
```

---

## Related

- [[Home]]

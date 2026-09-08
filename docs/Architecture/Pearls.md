# Pearls

#architecture

> [!info] Work items, one database
> A pearl is a unit of work. Every project's pearls live in **one SQLite file per machine**, `~/.smooth/pearls.db`, keyed by the project's canonical root (pearl th-d3e842). Reads are ~10ms, concurrent agents queue on SQLite's lock instead of wedging, and a pearl created inside a git worktree lands in the main checkout's project.

## Concepts

- **Pearl** — title, description, status, priority, type, dependencies, labels, comments, history.
- **Status** — `open`, `in_progress`, `closed`, `deferred`.
- **Type** — `task`, `bug`, `feature`, `epic`, `chore`.
- **Dependencies** — DAG. `ready` pearls have no open blocking dependencies.
- **Sub-pearls** — pearls created by operators mid-dispatch (`delegate` tool). Linked via parent.
- **Memories** — free-form project notes (`th pearls remember`), same database, same project scoping.

## Storage layout

```
~/.smooth/pearls.db          # SQLite, WAL mode; $SMOOTH_PEARLS_DB overrides
  pearls              (project, id) PRIMARY KEY
  pearl_dependencies  (project, pearl_id, depends_on)
  pearl_labels        (project, pearl_id, label)
  pearl_comments      (project, id) UNIQUE, seq for ordering
  pearl_history       (project, id) UNIQUE, seq for ordering
  memories            (project, id) UNIQUE
  config              (project, k)
~/.smooth/registry.json      # every project root th has seen; dead paths pruned on open
```

**`project`** is the canonical project root. `PearlStore::open(any_path)` runs
`git rev-parse --git-common-dir` and takes its parent, so the main checkout and
every linked worktree of a repo share one project; outside git it is the
directory itself. Ids stay `th-xxxxxx` and are unique **per project** — two
projects may reuse an id (the Dolt-era stores generated them independently).

Timestamps are UTC RFC3339 text with a fixed microsecond width, so `<=` in SQL
is chronological. Queries compare against a Rust `Utc::now()` literal, never
SQLite's `now`.

## `th pearls` quick reference

```bash
th pearls init                        # ensure the db exists + register this project
th pearls create --title="…" --description="…"
th pearls list --status=open
th pearls list --status=in_progress
th pearls show <id>                   # details + deps + comments + history
th pearls update <id> --status=in_progress
th pearls close <id1> <id2> …
th pearls ready                       # open, no blockers
th pearls blocked                     # open, unmet deps
th pearls projects                    # all registered projects
th pearls migrate-from-dolt [PATH]    # one-shot import of a legacy .smooth/dolt store
th pearls push / pull                 # exit-0 notice: sync is pearl th-ddce81
th db path                            # where pearls.db lives
```

There is no `th issues` or `th beads` alias. The naming lineage is beads → issues → **pearls**; only "pearls" is current.

## Migrating from Dolt

Until pearl th-d3e842 the store was an embedded Dolt database per project
(`.smooth/dolt/`, `smooth-dolt` Go binary, sync over `refs/dolt/data`). Run
`th pearls migrate-from-dolt` inside a project (or pass a path) to import every
table — pearls, dependencies, labels, comments, history, memories, config —
preserving ids and timestamps. It is `INSERT OR IGNORE`, so re-running is a
no-op, and the Dolt directory is left untouched for you to delete afterwards.
`dolt.rs` / `dolt_server.rs` / `go/smooth-dolt` exist only for this command and
go away in th-c6ba83.

## Diver: the lifecycle wrapper

The pearl store is a passive CRUD surface. The [[The-Cast#Diver|Diver]] cast member wraps it with lifecycle semantics:

- `Diver::dispatch(title, desc, parent?)` — create pearl, mark `in_progress`, return id.
- `Diver::complete(id)` — close pearl after successful dispatch.
- `Diver::sub_pearl(parent, …)` — create a child pearl during a dispatch.
- Jira sync (bidirectional) when `JIRA_URL` + `JIRA_API_TOKEN` are configured.

## Related

- [[Data-Storage]]
- [[The-Cast]]

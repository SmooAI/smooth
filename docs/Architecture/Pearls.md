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
th pearls checkpoint <id> --note "…" --next "…"   # handoff checkpoint (see below)
th pearls show <id> --handoff [--json]   # handoff packet
th pearls prime --in-progress [--cwd .] [--assignee a] [--json]
th pearls update <id> --status=in_progress
th pearls close <id1> <id2> …
th pearls ready                       # open, no blockers
th pearls blocked                     # open, unmet deps
th pearls projects                    # all registered projects
th pearls sync --project SMOOTH       # bind this checkout to a Smoo project, then sync
th pearls sync [--pull-only|--push-only] [--dry-run] [--json]
th pearls push / pull                 # exit-0 notice pointing at `th pearls sync`
th db path                            # where pearls.db lives
```

There is no `th issues` or `th beads` alias. The naming lineage is beads → issues → **pearls**; only "pearls" is current.

## History

Until pearl th-d3e842 the store was an embedded Dolt database per project
(`.smooth/dolt/`, a 140MB `smooth-dolt` Go binary, sync over `refs/dolt/data`).
PR #522 retired it for the SQLite store above and shipped a one-shot
`th pearls migrate-from-dolt` importer; pearl th-c6ba83 then deleted the Dolt
shim, the Go binary and the Go/ICU CI steps. A straggler machine that still has
a `.smooth/dolt/` store must install **th ≤ 0.42.x** (Homebrew v0.42.1 is the
last release with `migrate-from-dolt`), run `th pearls migrate-from-dolt` in
each repo, and only then upgrade — newer builds cannot read Dolt at all.

## Handoff model — checkpoints (pearl th-9483e8)

A pearl is the unit of work that outlives a context window, so it carries the
state a fresh session needs to resume cold. `th pearls checkpoint <id>` appends
a **checkpoint**: an optional note, a timestamp, an `auto` flag, and a
**handoff** block collected from git at that moment — `worktree`, `branch`,
`head`, `dirty` (`git status --porcelain`, capped at 50 rows),
`agent_session_id` (`--session-id` or `$CLAUDE_SESSION_ID`), and `next`
(`--next`, what to do first).

**Storage.** Each checkpoint is one ordinary pearl comment whose content is
`smooth-checkpoint:` followed by the JSON record. That rides on the public
`PearlStore` comment API alone — no table, no migration. `th pearls show` folds these
comments into a `Checkpoints` section instead of printing the JSON. The
effective handoff is the field-wise merge of every checkpoint in order (latest
non-null wins), so an `--auto` checkpoint refreshes `head`/`dirty` without
erasing an earlier `next`; notes accumulate.

**Packet.** `th pearls show <id> --handoff --json` and
`th pearls prime --in-progress --json` emit the SmoothFlow "pearl rail" shape:

```json
{
    "pearl": { "id": "th-9483e8", "title": "…", "status": "in_progress", "…": "…" },
    "handoff": {
        "worktree": "/…/smooth-th-9483e8-handoff",
        "branch": "th-9483e8-handoff",
        "head": "…",
        "dirty": ["M …"],
        "agent_session_id": "…",
        "next": "…"
    },
    "checkpoints": [{ "at": "2026-09-07T22:10:00Z", "note": "…", "auto": false }],
    "blocks": ["th-d3e842"],
    "pr": { "number": 520, "url": "…", "state": "OPEN", "ci": "pending" }
}
```

`blocks` is the open pearls this one still waits on; `pr` comes from
`gh pr list --head <branch>` (null without `gh`, offline, or no PR), with `ci`
folded from `statusCheckRollup` to `success` / `failure` / `pending`. The
human form is a compact "resume cold" block: what it is, where it is, what
happened, what is next.

**Worktree matching.** `--cwd <dir>` keeps the pearls whose recorded worktree
is `<dir>`'s repo root, or whose id appears in `<dir>`'s branch name — the
`th worktree create th-<id>-…` convention — so a claimed-but-never-checkpointed
pearl is still found. `PearlStore::open` resolves the project through the git
common dir, so a checkpoint taken inside a linked worktree lands under the
project's root, not the worktree's.

**Hooks (smooth-agent plugin).** `PreCompact` runs
`th pearls checkpoint --auto` for the matching in-progress pearls;
`SessionStart` with matcher `compact|resume` injects
`th pearls prime --in-progress --cwd .` as `additionalContext`. Both are
silent no-ops without `th`, a store, or a match.

## Sync with Smoo Projects (`th pearls sync`, pearl th-19cca5)

The store is offline-first; `th pearls sync` is the explicit reconcile against
the Smoo Projects work-items API (the one `smoo work` wraps), as the logged-in
user. It runs pull, then push, then relations:

- **Mapping** — `sync_map(project, pearl_id, remote_id, remote_updated_at,
local_updated_at, last_synced_at)` in `pearls.db`. The remote item carries
  `externalRef = <pearl_id>@<checkout-dir-name>`; a machine that has never
  synced adopts items by that ref (keeping the pearl id when free) instead of
  duplicating them. The project binding is `config.sync.project_id` /
  `sync.project_key`; `sync.last_pull_at` is the `updatedSince` cursor.
- **Fields** — title, description, labels, priority (inverted: P0 ↔ 4),
  type (`epic` ↔ `feature` + `epic` label), status (`closed` ↔ `done`,
  `deferred` ↔ `blocked`, `in_review`/`cancelled` fold into `in_progress`/
  `closed` on pull and are not demoted on push), parent. Dependencies become
  `blocks` links on the blocker item; comments mirror both ways with a
  `pearl-comment:<id>` first line marking ours.
- **Conflicts** — "changed" means "differs from the baseline" (clock skew
  between laptop and server must not hide an edit); timestamps only break
  ties, newer wins, the loser is listed in the report. Nothing is deleted on
  either side, ever; orphans are reported.
- Deps/comments are reconciled for the active set (open, or touched this run).

## Diver: the lifecycle wrapper

The pearl store is a passive CRUD surface. The [[The-Cast#Diver|Diver]] cast member wraps it with lifecycle semantics:

- `Diver::dispatch(title, desc, parent?)` — create pearl, mark `in_progress`, return id.
- `Diver::complete(id)` — close pearl after successful dispatch.
- `Diver::sub_pearl(parent, …)` — create a child pearl during a dispatch.
- Jira sync (bidirectional) when `JIRA_URL` + `JIRA_API_TOKEN` are configured.

## Related

- [[Data-Storage]]
- [[The-Cast]]

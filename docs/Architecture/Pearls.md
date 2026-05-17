# Pearls

#architecture

> [!info] Work items, version-controlled
> A pearl is a unit of work. The pearl store is Dolt-backed (one database per project), so pearl history is a real SQL log you can `pearls log`, push to a remote, or pull on another machine. Sessions, messages, and orchestrator snapshots live in the same database.

## Concepts

- **Pearl** — title, description, status, priority, type, dependencies, labels, comments, history.
- **Status** — `open`, `ready`, `in_progress`, `closed`, `blocked`.
- **Type** — `task`, `bug`, `feature`, `epic`, `chore`.
- **Dependencies** — DAG. `ready` pearls have no open dependencies.
- **Sub-pearls** — pearls created by operators mid-dispatch (`delegate` tool). Linked via parent.

## Storage layout

```
   <repo>/.smooth/
     └── dolt/
         └── pearls/      # Dolt database (content-addressed; git-friendly)
             ├── pearls
             ├── pearl_dependencies
             ├── pearl_labels
             ├── pearl_comments
             ├── pearl_history
             ├── sessions
             ├── session_messages
             ├── orchestrator_snapshots
             └── memories
```

`~/.smooth/registry.json` tracks every project pearl store the local `th` knows about.

## `th pearls` quick reference

```bash
th pearls init                        # create .smooth/dolt/ in current repo
th pearls create --title="…" --description="…"
th pearls list --status=open
th pearls list --status=in_progress
th pearls show <id>                   # details + deps + comments
th pearls update <id> --status=in_progress
th pearls close <id1> <id2> …
th pearls ready                       # open, no blockers
th pearls blocked                     # open, unmet deps
th pearls log                         # dolt commit history
th pearls push                        # to a Dolt remote
th pearls pull                        # from a Dolt remote
th pearls projects                    # all registered projects
```

There is no `th issues` or `th beads` alias. The naming lineage is beads → issues → **pearls**; only "pearls" is current.

## Diver: the lifecycle wrapper

The pearl store is a passive CRUD surface. The [[The-Cast#Diver|Diver]] cast member wraps it with lifecycle semantics:

- `Diver::dispatch(title, desc, parent?)` — create pearl, mark `in_progress`, return id.
- `Diver::complete(id)` — close pearl after successful dispatch.
- `Diver::sub_pearl(parent, …)` — create a child pearl during a dispatch.
- Jira sync (bidirectional) when `JIRA_URL` + `JIRA_API_TOKEN` are configured.

`dispatch_ws_task_*` prefers Diver and falls back to the raw store if Diver is absent. See [[Dispatch]].

## Sessions and resume

Every dispatch records `session_messages` on the pearl as the agent runs. When the same pearl is re-dispatched later, `build_resumption_context` reads the last N messages and prepends them to the new task as a `## Resumption context` block. The agent picks up where the prior run left off.

`orchestrator_snapshots` is the higher-level analog: the orchestrator's state machine writes snapshots that survive process restarts.

## Memories

The `memories` table is a free-form key-value scratchpad operators can read/write across pearls. Used by the chat agent to remember user preferences, project context, etc. Not the place for long-term knowledge — that goes in the source repo where the agent can `read_file` it.

## smooth-dolt: the engine

`smooth-pearls` doesn't speak Dolt natively. It shells out to `smooth-dolt`, a Go binary that embeds the Dolt SQL engine. Build it with:

```bash
scripts/build-smooth-dolt.sh
# Produces target/release/smooth-dolt (~145MB; embedded Dolt engine + ICU)
```

Requires Go 1.21+ and ICU (macOS: `brew install icu4c`). The binary is mirrored to `~/.smooth/runner-bin/` by `pnpm install:th` so production installs find it.

## Migrating from older stores

Legacy state lives in `~/.smooth/smooth.db` (SQLite). One-time migration:

```bash
th pearls migrate-from-sqlite
th pearls migrate-from-beads   # if you used the bd CLI
```

After migration, the SQLite file is no longer read. Dolt is the only store.

## Related

- [[Dispatch]]
- [[Data-Storage]]
- [[The-Cast]]

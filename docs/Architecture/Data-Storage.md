# Data Storage

#architecture

> [!info] Two buckets
> Global `~/.smooth/` for machine state — including **every project's pearls** in one SQLite file. Project `.smooth/` for repo-scoped config. No VM volumes — dispatch runs on the host against your working directory.

## Pearls: `~/.smooth/pearls.db`

One SQLite database (WAL) holding pearls, dependencies, labels, comments, history, memories and config for every project on the machine, each row tagged with its canonical project root. See [[Pearls#Storage-layout]]. `$SMOOTH_PEARLS_DB` relocates it (tests point it at a tempdir).

## Global: `~/.smooth/`

| Path                         | What                                                                            |
| ---------------------------- | ------------------------------------------------------------------------------- |
| `pearls.db`                  | All pearls, every project (SQLite; see [[Pearls]])                              |
| `registry.json`              | Index of every project root this `th` knows about (dead paths pruned on open)   |
| `providers.json`             | LLM provider credentials                                                        |
| `audit/`                     | Rotating tool-usage logs per actor (Big Smooth, operatives, …)                  |
| `mcp.toml`                   | Global MCP server configs                                                       |
| `plugins/<name>/plugin.toml` | CLI-wrapper plugin manifests                                                    |
| `smooth.pid`                 | Big Smooth daemon pid (`th up` writes it, `th down` reads it)                   |
| `smooth.log`                 | Big Smooth daemon stdout+stderr                                                 |
| `smooth.db`                  | Legacy SQLite — unread, and no migration command ships any more; safe to delete |

## Project: `<repo>/.smooth/`

| Path                         | What                                                                             |
| ---------------------------- | -------------------------------------------------------------------------------- |
| `dolt/`                      | Legacy Dolt pearl store — import with `th pearls migrate-from-dolt`, then delete |
| `mcp.toml`                   | Project-scoped MCP servers; merged with global, project wins                     |
| `plugins/<name>/plugin.toml` | Project-scoped plugins; same merge rules                                         |

## Audit log

The operative's tool calls and Narc verdicts are written to `~/.smooth/audit/<actor>.jsonl` via Scribe (forwarded through Archivist). Rotating file appender; old segments are gzipped. The dashboard reads recent audit lines for the "what did the agent just do?" view; `th audit tail` / `th audit query` give CLI access.

## Backups & sync

Pearls are the only state worth backing up: copy `~/.smooth/pearls.db` (plus its `-wal`/`-shm` siblings, or after `PRAGMA wal_checkpoint`). `th pearls push` / `pull` currently print a notice — cross-machine sync against Smoo Projects is pearl th-19cca5. Jira sync is the other replication channel (see [[The-Cast#Diver|Diver]]).

`providers.json` is per-machine. Treat it like `.aws/credentials`: do not check it in.

## Related

- [[Pearls]]
- [[Architecture-Overview]]
- [[Engineering/Build-Workflow]]

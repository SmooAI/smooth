# Data Storage

#architecture

> [!info] Three buckets
> Per-project Dolt for pearls + sessions. Global `~/.smooth/` for cross-project state. Project `.smooth/` for repo-scoped config. No VM volumes — dispatch runs on the host against your working directory.

## Per-project: `.smooth/dolt/`

Pearl data + session messages + orchestrator snapshots + memories. See [[Pearls#Storage-layout]]. The database is a real Dolt database — versioned, push/pullable.

Engine: the `smooth-dolt` Go binary (built from `scripts/build-smooth-dolt.sh`). `smooth-pearls` shells out to it; nothing in the Rust workspace links Dolt directly.

## Global: `~/.smooth/`

| Path                     | What                                                            |
| ------------------------ | --------------------------------------------------------------- |
| `registry.json`          | Index of every project pearl store this `th` knows about        |
| `providers.json`         | LLM provider credentials                                        |
| `audit/`                 | Rotating tool-usage logs per actor (Big Smooth, operatives, …)  |
| `mcp.toml`               | Global MCP server configs                                       |
| `plugins/<name>/plugin.toml` | CLI-wrapper plugin manifests                                |
| `smooth.pid`             | Big Smooth daemon pid (`th up` writes it, `th down` reads it)   |
| `smooth.log`             | Big Smooth daemon stdout+stderr                                 |
| `smooth.db`              | Legacy SQLite — unread, and no migration command ships any more; safe to delete |

## Project: `<repo>/.smooth/`

| Path                  | What                                                              |
| --------------------- | ----------------------------------------------------------------- |
| `dolt/`               | Pearl database (see above)                                        |
| `mcp.toml`            | Project-scoped MCP servers; merged with global, project wins      |
| `plugins/<name>/plugin.toml` | Project-scoped plugins; same merge rules                   |

## Audit log

The operative's tool calls and Narc verdicts are written to `~/.smooth/audit/<actor>.jsonl` via Scribe (forwarded through Archivist). Rotating file appender; old segments are gzipped. The dashboard reads recent audit lines for the "what did the agent just do?" view; `th audit tail` / `th audit query` give CLI access.

## Backups & sync

Pearls are the only state worth backing up — and they're already a Dolt DB:

```bash
th pearls push    # push to a Dolt remote (DoltHub or self-hosted)
th pearls pull    # pull from a remote
```

For team workflows: share a Dolt remote so everyone sees the same pearls + history. Jira sync is the other replication channel (see [[The-Cast#Diver|Diver]]).

`providers.json` is per-machine. Treat it like `.aws/credentials`: do not check it in.

## Related

- [[Pearls]]
- [[Architecture-Overview]]
- [[Engineering/Build-Workflow]]

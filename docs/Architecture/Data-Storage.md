# Data Storage

#architecture

> [!info] Three buckets
> Per-project Dolt for pearls + sessions. Global `~/.smooth/` for cross-project state. Project `.smooth/` for repo-scoped config. Plus microsandbox-managed named volumes for the operator dev-tool cache.

## Per-project: `.smooth/dolt/`

Pearl data + session messages + orchestrator snapshots + memories. See [[Pearls#Storage-layout]]. The database is a real Dolt database — versioned, push/pullable.

Engine: the `smooth-dolt` Go binary (built from `scripts/build-smooth-dolt.sh`). `smooth-pearls` shells out to it; nothing in the Rust workspace links Dolt directly.

## Global: `~/.smooth/`

| Path                     | What                                                            |
| ------------------------ | --------------------------------------------------------------- |
| `registry.json`          | Index of every project pearl store this `th` knows about        |
| `providers.json`         | LLM provider credentials (encrypted in some configurations)     |
| `audit/`                 | Rotating tool-usage logs per actor (Big Smooth, operators, …)   |
| `project-cache/`         | Operator dev-tool cache (legacy bind-mount mode)                |
| `mcp.toml`               | Global MCP server configs                                       |
| `plugins/<name>/plugin.toml` | CLI-wrapper plugin manifests                                |
| `smooth.pid`             | Direct-mode daemon pid                                          |
| `smooth.log`             | Direct-mode daemon stdout+stderr                                |
| `sandboxed.vm`           | microsandbox VM name from the last `th up` (so `th down` finds it) |
| `runner-bin/`            | Mirror of the cross-compiled `smooth-operator-runner` + `smooth-dolt` |
| `smooth.db`              | Legacy SQLite (no longer read after migration; safe to delete after `pearls migrate-from-sqlite`) |

## Project: `<repo>/.smooth/`

| Path                  | What                                                              |
| --------------------- | ----------------------------------------------------------------- |
| `dolt/`               | Pearl database (see above)                                        |
| `mcp.toml`            | Project-scoped MCP servers; merged with global, project wins      |
| `plugins/<name>/plugin.toml` | Project-scoped plugins; same merge rules                   |

## Project cache (named volumes)

Repeated operator runs on the same repo benefit from a persistent `~/.cache` style scratch directory: `pnpm install`, `cargo fetch`, `uv sync` results all survive between dispatches.

- Keyed by a hash of the canonical workspace path (so `~/dev/repo` always gets the same cache regardless of pearl id).
- Mounted at `/opt/smooth/cache` inside the operator VM (sandboxed) or `~/.smooth/project-cache/<key>/` on the host (direct).
- **Backend (default):** microsandbox named volume. `SMOOTH_USE_VOLUMES=0` (or `false`/`no`/`off`) opts back into the legacy bind-mount path.
- Manage with `th cache list | prune | clear`.

## Audit log

Every cast member's tool calls and policy decisions are written to `~/.smooth/audit/<actor>.jsonl` by Scribe (forwarded through Archivist). Rotating file appender; old segments are gzipped.

The dashboard reads recent audit lines for the "What did the agent just do?" view. `th audit tail` and `th audit query` give CLI access.

## Backups & sync

Pearls are the only state worth backing up — and they're already a Dolt DB, so:

```bash
th pearls push    # push to a Dolt remote (DoltHub or self-hosted)
th pearls pull    # pull from a remote
```

For team workflows: share a Dolt remote so everyone sees the same pearls + history. Jira sync is the other replication channel (see [[The-Cast#Diver|Diver]]).

`providers.json` is per-machine. Treat it like an `.aws/credentials` file: do not check it in.

## Related

- [[Pearls]]
- [[Architecture-Overview]]
- [[Engineering/Build-Workflow]]

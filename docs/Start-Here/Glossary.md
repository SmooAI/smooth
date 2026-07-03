# Glossary

#start-here

> [!info] Definitions
> Canonical names and one-liners for everything the docs cross-reference. Cast roles are detailed in [[Architecture/The-Cast]].

## Runtime

- **`th`** — The Smooth CLI binary. One Rust binary, every command.
- **Big Smooth** — The always-on host process `th up` starts: axum API + WebSocket on `127.0.0.1:4400`, the pearl store, and dispatch. Pid at `~/.smooth/smooth.pid`.
- **Operative** — The `smooth-operative` subprocess Big Smooth spawns per task; runs the agent loop against your working directory (host, no VM).
- **Sandboxed / direct mode** — Historical. Smooth used to boot inside a microsandbox microVM (`th up`) with a host escape hatch (`th up direct`). The VM stack was removed 2026-07; see [[Decisions/ADR-004-remove-microvm-sandbox-stack]].

## The Cast

- **[[Architecture/The-Cast#Big-Smooth|Big Smooth]]** — Orchestrator. Owns the API, the pearl store, dispatch, and Diver/Archivist.
- **[[Architecture/The-Cast#Narc|Narc]]** — Tool surveillance hook (in-process on the operative). Regex secret/injection detectors + optional LLM judge.
- **[[Architecture/The-Cast#Scribe|Scribe]]** — Per-actor structured logging. Feeds Archivist.
- **[[Architecture/The-Cast#Archivist|Archivist]]** — Central log + event aggregator. SSE stream backs the live dashboard.
- **[[Architecture/The-Cast#Diver|Diver]]** — Pearl lifecycle manager. Creates pearls on dispatch, closes on complete, syncs Jira.
- **Engine (Groove)** — The `smooth-operator` agent framework the operative runs; Groove is its checkpoint/resume layer.
- **Wonk / Goalie** — *Removed July 2026 (`th-f4a801`).* The former microVM access authority + network/FS proxy. Enforcement is being rebuilt as an auto-mode permission engine (`th-515a13`). See [[Architecture/Security-Model]] and [[Decisions/ADR-004-remove-microvm-sandbox-stack]].

## Work model

- **Pearl** — A single work item. Dolt-backed. Has status, dependencies, comments, history. See [[Architecture/Pearls]].
- **Teammate** — A registered operative the UI knows about. One per active dispatch.
- **Dispatch** — Handing a pearl to an operative and running the agent loop. See [[Architecture/Dispatch]].
- **Coding workflow** — The single-agent loop with a test-feedback governor that coding roles run (`smooth_cast::coding_workflow`). Not a multi-phase pipeline — that was dropped.

## Storage

- **Dolt** — Versioned SQL database backing pearls + sessions. Per-project at `.smooth/dolt/`.
- **`smooth-dolt`** — Go binary embedding the Dolt engine. Spawned as a subprocess by `smooth-pearls`.
- **`~/.smooth/`** — Global Smooth state: `providers.json`, `registry.json`, `audit/`, `plugins/`, `smooth.pid`/`smooth.log`.
- **`.smooth/`** — Project-scoped state: `dolt/`, `mcp.toml`, `plugins/`.

## Extensibility

- **MCP server** — A Model Context Protocol server whose tools land in the registry as `<server>.<tool>`. Configured via `mcp.toml`.
- **Plugin** — A CLI-wrapper tool declared in `plugin.toml`, registered as `plugin.<name>`.
- **SEP** — *Planned.* The Smooth Extension Protocol — subprocess extensions over JSON-RPC. See [[Architecture/Extension-System]].
- **`SMOOTH_NARC_URL`** — The URL operatives dial to escalate ambiguous tool calls to Narc. Loopback, since operatives are host subprocesses.

## Related

- [[Home]]
- [[Architecture/The-Cast]]
- [[Architecture/Architecture-Overview]]

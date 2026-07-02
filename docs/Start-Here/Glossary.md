# Glossary

#start-here

> [!info] Definitions
> Canonical names and one-liners for everything the docs cross-reference. Cast roles are detailed in [[Architecture/The-Cast]].

## Runtime

- **`th`** — The Smooth CLI binary. One Rust binary, every command.
- **Daemon** — What `th up` boots: Big Smooth + cast as tokio tasks in one host process, pid at `~/.smooth/smooth.pid`.
- **Sandboxed / direct mode** — Historical. Smooth used to boot inside a microsandbox microVM (`th up`) with a host escape hatch (`th up direct`). The VM stack was removed 2026-07; see [[Decisions/ADR-004-remove-microvm-sandbox-stack]].

## The Cast

- **[[Architecture/The-Cast#Big-Smooth|Big Smooth]]** — Orchestrator. READ-ONLY. Owns the API, dispatches operators, owns Diver and the access store.
- **[[Architecture/The-Cast#Narc|Narc]]** — Tool surveillance hook. Regex pre-filter + LLM judge for ambiguous cases.
- **Wonk / Goalie** — Removed 2026-07 with the microVM stack ([[Decisions/ADR-004-remove-microvm-sandbox-stack]]). Were the access-control authority and network/filesystem proxy.
- **[[Architecture/The-Cast#Scribe|Scribe]]** — Per-actor structured logging. Forwards to Archivist.
- **[[Architecture/The-Cast#Archivist|Archivist]]** — Central log + event aggregator. Backs the live dashboard.
- **[[Architecture/The-Cast#Diver|Diver]]** — Pearl lifecycle manager. Creates pearls on dispatch, closes on complete, syncs Jira.
- **[[Architecture/The-Cast#Groove|Groove]]** — LLM checkpointing + session resume. Lives inside `smooth-operator`.

## Work model

- **Pearl** — A single work item. Dolt-backed. Has status, dependencies, comments, history. See [[Architecture/Pearls]].
- **Operative** — An agent instance (a host subprocess) running `smooth-operative` against one pearl. It runs the `smooth-operator` *engine*; don't confuse the two.
- **Teammate** — A registered operator the UI knows about. One per active dispatch.
- **Dispatch** — The act of handing a pearl to an operator and running the agent loop.
- **Workflow** — Multi-phase loop (plan → execute → test → review) the runner uses when `SMOOTH_WORKFLOW=1` (default).
- **Phase** — A named step inside the workflow. Determines which tools and policies apply.

## Storage

- **Dolt** — Versioned SQL database backing pearls + sessions. Per-project at `.smooth/dolt/`.
- **`smooth-dolt`** — Go binary embedding the Dolt engine. Spawned as a subprocess by `smooth-pearls`.
- **`~/.smooth/`** — Global Smooth state: providers.json, registry.json, audit/, project-cache/, plugins/.
- **`.smooth/`** — Project-scoped state: `dolt/`, `mcp.toml`, `plugins/`.

## Networking

- **`SMOOTH_NARC_URL`** — The URL operatives dial to escalate ambiguous tool calls to Narc. Loopback, since operatives are host subprocesses.

## Related

- [[Home]]
- [[Architecture/The-Cast]]
- [[Architecture/Architecture-Overview]]

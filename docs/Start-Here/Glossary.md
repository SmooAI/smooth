# Glossary

#start-here

> [!info] Definitions
> Canonical names and one-liners for everything the docs cross-reference. Cast roles are detailed in [[Architecture/The-Cast]].

## Modes

- **Sandboxed mode** — `th up`. Smooth runs inside a microsandbox microVM. The default.
- **Direct mode** — `th up direct`. Smooth runs on the host with no isolation. Trusted environments only.

## Runtime

- **`th`** — The Smooth CLI binary. One Rust binary, every command.
- **Safehouse microVM** — The single microsandbox VM `th up` boots. Hosts Big Smooth and the rest of the cast.
- **Safehouse image** — `ghcr.io/smooai/safehouse:latest`. OCI image baking the in-VM `smooth-bigsmooth` binary.
- **microsandbox** — The Rust SDK we use to boot hardware-isolated microVMs. Embedded as a crate dependency; no external `msb` CLI required at runtime.

## The Cast

- **[[Architecture/The-Cast#Big-Smooth|Big Smooth]]** — Orchestrator. READ-ONLY. Owns the API, dispatches operators, owns Diver and the access store.
- **[[Architecture/The-Cast#Wonk|Wonk]]** — Access-control authority. Policy → answer. No LLM.
- **[[Architecture/The-Cast#Goalie|Goalie]]** — Outbound HTTP/HTTPS proxy. Delegates every decision to Wonk.
- **[[Architecture/The-Cast#Narc|Narc]]** — Tool surveillance hook. Regex pre-filter + LLM judge for ambiguous cases.
- **[[Architecture/The-Cast#Scribe|Scribe]]** — Per-actor structured logging. Forwards to Archivist.
- **[[Architecture/The-Cast#Archivist|Archivist]]** — Central log + event aggregator. Backs the live dashboard.
- **[[Architecture/The-Cast#Diver|Diver]]** — Pearl lifecycle manager. Creates pearls on dispatch, closes on complete, syncs Jira.
- **[[Architecture/The-Cast#Groove|Groove]]** — LLM checkpointing + session resume. Lives inside `smooth-operator`.

## Work model

- **Pearl** — A single work item. Dolt-backed. Has status, dependencies, comments, history. See [[Architecture/Pearls]].
- **Operative** — An agent instance (the sandboxed worker) running `smooth-operative` against one pearl. It runs the `smooth-operator` *engine*; don't confuse the two.
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

- **`allow_host_loopback`** — SandboxConfig flag that exposes `host.docker.internal` (the host's gateway IP) inside the microVM. Used to reach a host Docker / OrbStack / Kalima from inside the sandbox without nested virt.
- **`SMOOTH_NARC_URL`** — The URL operators dial to escalate ambiguous tool calls to Narc. Resolved to a routable host interface IP, since `127.0.0.1` inside a microVM is the guest's own loopback.

## Related

- [[Home]]
- [[Architecture/The-Cast]]
- [[Architecture/Architecture-Overview]]

# The Cast

#architecture #cast

> [!arch] One process, one operative per task
> Big Smooth and its supporting roles run as tokio tasks / in-process services inside the single `th` host process. Each dispatched task spawns one `smooth-operative` subprocess that runs the agent loop. Crate boundaries are preserved so each role keeps its own state, hooks, and tests.
>
> The microVM-era access cast — **Wonk** (policy authority) and **Goalie** (network/FS proxy) — was removed in July 2026 (pearl `th-f4a801`) along with the per-task VM. Enforcement is being rebuilt as an auto-mode permission engine on `smooth-policy` (`th-515a13`, in progress) — see [[Security-Model]].

## Cast at a glance

| Role            | Crate              | What it does                                     | Where it runs                          |
| --------------- | ------------------ | ------------------------------------------------ | -------------------------------------- |
| Big Smooth      | `smooth-bigsmooth` | Orchestrator, API, dispatch                      | Host process (axum on `:4400`)         |
| Operative       | `smooth-operative` | Runs one pearl's agent loop + tools              | Host subprocess (one per pearl)        |
| Engine (Groove) | `smooth-operator`  | Agent loop, LLM client, tools, checkpoint/resume | In-process to the operative            |
| Narc            | `smooth-narc`      | Tool surveillance hook + optional LLM judge      | In-process on the operative's registry |
| Scribe          | `smooth-scribe`    | Per-actor structured logging                     | In-process, feeds Archivist            |
| Archivist       | `smooth-archivist` | Central log + event aggregator, SSE stream       | In-process to Big Smooth               |
| Diver           | `smooth-diver`     | Pearl lifecycle + Jira sync                      | In-process to Big Smooth               |

There is no longer any gRPC-over-UDS wire between cast members. Big Smooth's cast shares `Arc<AppState>`. The one real process boundary is Big Smooth ↔ each operative subprocess, and it is crossed with **JSON-lines `AgentEvent`s on the operative's stdout** — see [[Dispatch]].

---

## Big Smooth

The orchestrator and the `axum` server on `127.0.0.1:4400` (loopback by default; the API is unauthenticated today — pearl `th-6db839`). Owns:

- The pearl store (Dolt) and session/message/snapshot history.
- Dispatch: `dispatch_ws_task` → `dispatch_ws_task_direct` spawns the operative.
- The teammate registry that backs the UI sidebar (`th operatives` reads it).
- Orchestrator state (`orchestrator.rs`) — a state surface (`Idle → Scheduling → Dispatching → Monitoring → Reviewing`) that status/TUI/web readers poll. Its VM-dispatch guts were removed; it now reports state only.
- Diver, Archivist, and the audit tap.

## Operative

The worker. `smooth-operative` is the only crate that runs the agent loop in production, exec'd as a native host subprocess per pearl. It registers the tool surface, installs `PermissionHook` (role-scoped tool gating, from the engine) and `NarcHook` (surveillance), and runs the coding workflow or single-agent loop. Full detail in [[Operatives]].

## Engine (smooth-operator / Groove)

The agent framework the operative runs — an external dependency (`smooth-operator-core`, published as a crate; path-dep'd during the daemon rewrite). Provides the observe→think→act loop (`agent.rs`), the OpenAI-compatible / native-Anthropic LLM client, the `Tool` trait + `ToolRegistry` with pre/post `ToolHook`s, conversation/context management, and **Groove** — checkpointing + session resume so an interrupted operative picks up at the last checkpoint. Don't conflate the engine crate with the operative binary or the public `smooth-operator` service.

## Narc

Tool surveillance. Installed as a `ToolHook` on the operative's registry, so it fires on every tool call in-process (no wire). Two-tier: fast regex detectors (secrets — 10 patterns; prompt injection — 6 patterns; an optional write guard) plus an optional LLM-as-a-judge (`smooth-judge` slot) for ambiguous cases. It is the surviving enforcement surface post-teardown; the network/FS kernel boundary that Goalie/Wonk provided is gone. See [[Security-Model]].

## Scribe + Archivist

Structured logging. Scribe captures per-actor structured events; Archivist aggregates them and exposes an SSE stream that backs the live dashboard and `th audit`. Both run in-process now.

## Diver

The Pearl Diver. Owns the pearl lifecycle — creates a pearl on dispatch, closes it on completion, tracks sub-pearls, manages the work model (parent/child, deps, labels, costs), and syncs bidirectionally with Jira. In-process to Big Smooth.

## Related

- [[Architecture-Overview]]
- [[Dispatch]]
- [[Operatives]]
- [[Security-Model]]

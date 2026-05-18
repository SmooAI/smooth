# Architecture Overview

#moc #architecture

> [!arch] One VM, one cast, two modes
> Smooth is a Rust binary that boots Big Smooth + the security cast + an operator runtime in one address space (direct) or one microVM (sandboxed). There is no per-operator VM and no separate boardroom VM. Operators are dispatched as work units inside the same process or VM where Big Smooth lives.

## System diagram

```
                              th up
                                │
        ┌───────────────────────┴──────────────────────────┐
        │                                                  │
        │              SANDBOXED MODE (default)            │
        │                                                  │
        │  ┌────────────────────────────────────────────┐  │
        │  │       microsandbox microVM (one)           │  │
        │  │                                            │  │
        │  │   ┌──────────────────────────────────┐     │  │
        │  │   │           Big Smooth             │     │  │
        │  │   │  (axum API on :4400, READ-ONLY)  │     │  │
        │  │   └──────┬───────┬───────┬───────────┘     │  │
        │  │          │       │       │                 │  │
        │  │     ┌────▼──┐ ┌──▼──┐ ┌──▼──┐  ┌──────┐    │  │
        │  │     │ Wonk  │ │Narc │ │Diver│  │Goalie│    │  │
        │  │     └───────┘ └─────┘ └─────┘  └──┬───┘    │  │
        │  │                                   │        │  │
        │  │     ┌────────────┐  ┌─────────────▼────┐   │  │
        │  │     │  Scribe    ├─►│    Archivist     │   │  │
        │  │     └────────────┘  └──────────────────┘   │  │
        │  │                                            │  │
        │  │   ┌──────────────────────────────────┐     │  │
        │  │   │  Operator runner(s) — one per    │     │  │
        │  │   │  dispatched pearl. Agent loop,   │     │  │
        │  │   │  file/bash tools, NarcHook,      │     │  │
        │  │   │  WonkHook on every tool call.    │     │  │
        │  │   └──────────────────────────────────┘     │  │
        │  └────────────────────────────────────────────┘  │
        │                                                  │
        │  Outbound to host Docker/OrbStack/Kalima:        │
        │  → host.docker.internal (allow_host_loopback)    │
        │                                                  │
        └──────────────────────────────────────────────────┘

                              th up direct
                                │
        ┌───────────────────────┴──────────────────────────┐
        │                                                  │
        │              DIRECT MODE (escape hatch)          │
        │                                                  │
        │   ┌──────────────────────────────────────────┐   │
        │   │             Host process (`th`)          │   │
        │   │  same cast + operator runner, no VM      │   │
        │   │  daemonised, PID file at ~/.smooth/      │   │
        │   └──────────────────────────────────────────┘   │
        │                                                  │
        └──────────────────────────────────────────────────┘
```

## Control flow (sandboxed)

1. User runs `th up` on the host.
2. `th` calls `start_sandboxed_vm()` — boots the Boardroom microVM via the embedded `microsandbox` SDK, forwards `:4400` to host `:4400`, exits.
3. Inside the VM, the `boardroom` binary launches. It opens the Dolt pearl store, calls `spawn_boardroom_cast()` to bring up Wonk / Goalie / Narc / Scribe / Archivist / Diver as tokio tasks, and starts the Big Smooth axum server.
4. User opens `http://localhost:4400` or runs `th code`. UI talks to Big Smooth's REST + WebSocket API.
5. User issues a task. Big Smooth's `dispatch_ws_task` decides direct vs sandboxed (env / flag) and runs the operator. See [[Dispatch]].
6. Operator emits `AgentEvent`s as JSON-lines; Big Smooth re-emits them as `ServerEvent`s on the WebSocket. The dashboard and TUI subscribe.

## Component map

| Crate                        | Role                                                                 |
| ---------------------------- | -------------------------------------------------------------------- |
| `smooth-cli`                 | The `th` binary. Clap entry point, `th up`, `th down`, all subcommands. |
| `smooth-bigsmooth`           | Big Smooth itself. axum server, dispatch, sandbox SDK, pearl + Diver wiring. |
| `smooth-bigsmooth/bin/boardroom` | In-VM Big Smooth binary. Cross-compiled to musl, baked into Boardroom image. |
| `smooth-wonk`                | Access-control authority. tonic gRPC server on `wonk.sock`.          |
| `smooth-goalie`              | HTTP/HTTPS forward proxy. Delegates every decision to Wonk via the gRPC client. |
| `smooth-narc`                | Tool-surveillance hook. Regex + LLM judge. tonic gRPC server on `narc.sock`. |
| `smooth-scribe`              | Per-actor structured logging. tonic gRPC server on `scribe.sock`; forwards batches over HTTP to Archivist. |
| `smooth-archivist`           | Central log aggregator. HTTP `:4401` + SSE `/events`. Backs the dashboard. |
| `smooth-diver`               | Pearl lifecycle manager + Jira sync.                                 |
| `smooth-operator`            | Agent framework: LLM client, tools, conversation, checkpoints (Groove). |
| `smooth-operator-runner`     | Binary the dispatcher exec's per task. Hosts the agent loop.         |
| `smooth-pearls`              | Pearl store. Dolt-backed.                                            |
| `smooth-policy`              | Policy types + TOML.                                                 |
| `smooth-code`                | Ratatui TUI dashboard.                                               |
| `smooth-web`                 | Embedded Vite SPA via `rust-embed`.                                  |

## Where to next

- [[Sandboxed-Mode]] — what's inside the microVM
- [[Direct-Mode]] — what changes without it
- [[The-Cast]] — every named role, definitively
- [[Transport]] — gRPC over UDS topology, .proto files, what's wired where
- [[Dispatch]] — how tasks get from chat to operator
- [[Operators]] — the agent runtime
- [[Data-Storage]] — Dolt, named volumes, sessions

## Related

- [[Home]]
- [[Start-Here/What-Is-Smooth]]

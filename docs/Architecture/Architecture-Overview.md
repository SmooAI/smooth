# Architecture Overview

#moc #architecture

> [!arch] One process, one operative per task
> Smooth is a Rust binary (`th`). `th up` starts **Big Smooth** — an axum server on the host that owns the pearl store, the API, and dispatch. Each task spawns one **operative** subprocess that runs the agent loop and streams events back. There is no microVM, no per-task VM, and no separate safehouse. The VM sandbox stack (microsandbox + the Wonk/Goalie access cast) was removed in July 2026 (pearl `th-f4a801`); [[Daemon-Direction|the daemon direction]] is where the security story is being rebuilt.

## System diagram

```
                       th up
                         │
                         ▼
  ┌────────────────────────────────────────────────────────────┐
  │  Big Smooth  (host process, ~/.smooth/smooth.pid)           │
  │  axum HTTP + WebSocket on 127.0.0.1:4400 (loopback default) │
  │                                                            │
  │   ├── pearl store (Dolt, per project)                      │
  │   ├── sessions / messages / orchestrator snapshots         │
  │   ├── Diver        — pearl lifecycle + Jira sync           │
  │   ├── Archivist    — log + event aggregator (SSE)          │
  │   └── dispatch_ws_task → spawns one operative per task     │
  └───────────────────────────┬────────────────────────────────┘
                              │ spawn subprocess, JSON-lines on stdout
                              ▼
  ┌────────────────────────────────────────────────────────────┐
  │  smooth-operative  (host subprocess, one per pearl)         │
  │  runs the smooth-operator agent loop                        │
  │   ├── tool registry (read / write / bash / …)               │
  │   ├── PermissionHook  — role-scoped tool gating (engine)    │
  │   └── NarcHook        — secret / injection / write guard    │
  │  streams AgentEvent JSON-lines → Big Smooth → ServerEvent   │
  └────────────────────────────────────────────────────────────┘

  Frontends talk HTTP + WebSocket to Big Smooth on :4400
   ├── th code   — ratatui TUI (smooth-code)
   └── web UI    — embedded Vite SPA (smooth-web), served at /
```

## Control flow

1. User runs `th up`. Big Smooth starts on the host and daemonizes (`--foreground` to stay attached), binding `127.0.0.1:4400` by default. `--bind 0.0.0.0` exposes it — the API has **no authentication today** (pearl `th-6db839`), so keep it on loopback or a trusted tailnet.
2. User opens `http://localhost:4400` or runs `th code`. The UI speaks Big Smooth's REST + WebSocket API.
3. User issues a task. Big Smooth's `dispatch_ws_task` resolves a pearl, then `dispatch_ws_task_direct` spawns a `smooth-operative` subprocess. See [[Dispatch]].
4. The operative runs the [[Operatives|agent loop]] with a scoped tool surface, [[Security-Model|Narc surveillance]], and role-based permission gating.
5. The operative emits `AgentEvent`s as JSON-lines on stdout; Big Smooth re-emits them as `ServerEvent`s over the WebSocket. The TUI and web UI subscribe.

## Component map

| Crate                  | Role                                                                     |
| ---------------------- | ------------------------------------------------------------------------ |
| `smooth-cli`           | The `th` binary. Clap entry point — `th up`/`down`/`status`, all subcommands. |
| `smooth-bigsmooth`     | Big Smooth. axum server, dispatch, orchestrator state, pearl + Diver wiring. |
| `smooth-operative`     | The worker binary the dispatcher exec's per task. Hosts the agent loop.   |
| `smooth-operator`      | Agent engine (external dep, `smooth-operator-core`): LLM client, tools, conversation, checkpoints (Groove). |
| `smooth-narc`          | Tool-surveillance hook — regex secret/injection detectors + optional LLM judge. |
| `smooth-policy`        | Policy types + TOML. Parsed for surveillance/diagnostics; feeds the in-progress auto-mode engine (`th-515a13`). |
| `smooth-scribe`        | Per-actor structured logging.                                            |
| `smooth-archivist`     | Central log + event aggregator. SSE stream backs the dashboard.          |
| `smooth-diver`         | Pearl lifecycle manager + Jira sync.                                     |
| `smooth-pearls`        | Pearl store. Dolt-backed.                                                |
| `smooth-cast`          | Skills discovery + agent role/persona resources.                         |
| `smooth-code`          | Ratatui TUI (`th code`).                                                 |
| `smooth-web`           | Embedded Vite SPA via `rust-embed`.                                      |
| `smooth-tunnel`        | `th tunnel` — reverse tunnel to th.smoo.ai for remote control.           |

## Where to next

- [[The-Cast]] — the surviving roles, definitively
- [[Dispatch]] — how a task flows from chat to an operative and back
- [[Operatives]] — the agent runtime and the operative binary
- [[Security-Model]] — Narc surveillance today, auto-mode permissions in progress
- [[Data-Storage]] — Dolt, sessions, `~/.smooth/`
- [[Extension-System]] — SEP, the planned extension protocol
- [[Daemon-Direction]] — where Big Smooth is headed (epic `th-c89c2a`)

## Related

- [[Home]]
- [[Start-Here/What-Is-Smooth]]

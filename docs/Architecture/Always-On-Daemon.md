# Always-on daemon (Big Smooth, reborn)

`smooth-daemon` (crate `smooai-smooth-daemon`, binary `smooth-daemon`) is the
reimagined Big Smooth: a **single-tenant, always-on personal-agent daemon** on
the [`smooth_operator`](./Operatives.md) engine. One trusted operator
self-hosts their own instance (hermes-style) on a personal machine reached over
SSH/Tailscale. Because there is no untrusted tenant, the per-task **microVM
substrate is dropped** (EPIC th-c89c2a) in favour of a kernel OS-sandbox on tool
subprocesses + an egress proxy + a Claude-Code-style permission engine.

It is the daemon spine + multiple thin frontends over one durable event surface,
borrowing hermes's persistence shape and opencode's headless-server pattern.

## Shape

```
┌──────────────────────────────────────────────────────────────┐
│ smooth-daemon (axum + tokio, loopback/tailnet bind :4400)     │
│   smooth_operator engine — Agent::run_with_channel/session     │
│     ├─ ToolHook: Gate-1 permission engine (deny→ask→allow)     │
│     ├─ bash → SandboxedCommand (kernel OS-sandbox, P0)         │
│     ├─ egress → goalie proxy (exact-host allowlist)            │
│     ├─ SqliteMemory (durable cross-session recall) + remember  │
│     ├─ SessionRunCoordinator (one in-flight turn / session)    │
│     └─ durable SQLite: events, sessions, messages, memories    │
│   HTTP/WS API ──┬─ /ws (token stream + commands)               │
│                 ├─ /api/event (durable SSE, cursor resume)     │
│                 └─ /api/session · /api/status · /api/mode ·     │
│                    /api/memory · /health                       │
└──────────┬──────────────────────┬────────────────────────────┘
     th code TUI (smooth-code)   React control surface (smooth-web)
```

## Endpoints

| Route | Purpose |
|---|---|
| `GET /health` | liveness + version (open; TUI probes before auto-start) |
| `GET /api/status` | version, permission mode, active tasks, egress-proxy addr |
| `PUT /api/mode` | switch the Gate-1 permission posture at runtime |
| `GET /ws` | WebSocket: `TaskStart`/`TaskCancel`/`PermissionReply`; streams `ServerEvent`s. `?session=<id>` resumes |
| `GET /api/event` | durable Server-Sent-Events, replayed from `?cursor=<seq>` (zero-loss reconnect) |
| `GET /api/session` | list / create sessions; `GET /api/session/{id}[/messages]` |
| `GET /api/memory` | search durable agent memory (keyword recall) |

All API + WS routes are wrapped in an optional bearer-token gate; `/health` and
the embedded SPA stay open. See [[Daemon-Security-Model]].

## Durable state (per-instance, SQLite — not Dolt)

The daemon's events, sessions, conversation messages, and memories are
**per-instance runtime state**, so they live in a local SQLite DB (WAL), not
Dolt (which is for team-synced [[Pearls]]). This makes the SSE cursor-resume
stream, the session list, conversation resume, and cross-session memory all
survive a daemon restart.

## Memory (hermes-style)

The agent calls the `remember` tool to persist salient facts (operator
preferences, confirmed approaches, current project state, references) into
`SqliteMemory`; the engine auto-recalls the most relevant entries for each user
message and injects them ahead of the prompt — with a freshness-check nudge for
`Project`/`Reference` types. The control surface's Memory panel searches them.

## Security

Three independent layers, the load-bearing two kernel-enforced — a permission
engine (intent), a kernel FS/env sandbox on tool subprocesses, and an egress
boundary (exact-host allowlist + proxy + Seatbelt net-deny). Full detail in
[[Daemon-Security-Model]].

## Configuration (opt-in)

| Env var | Effect | Default |
|---|---|---|
| `SMOOTH_DAEMON_BIND` | bind address | `127.0.0.1:4400` |
| `SMOOTH_DAEMON_TOKEN` | bearer-token auth | unset (open on loopback) |
| `SMOOTH_PERMISSION_MODE` | Gate-1 posture | `default` |
| `SMOOTH_EGRESS_ALLOWLIST` | egress boundary (`defaults` for the curated set) | unset (unrestricted) |
| `SMOOTH_EGRESS_PROXY_ADDR` | egress proxy bind | `127.0.0.1:4419` |
| `SMOOTH_DAEMON_DB` | durable DB path | `~/.smooth/daemon.db` |

## Related

- [[Daemon-Security-Model]] — the three security layers in depth
- [[Architecture-Overview]] · [[Sandboxed-Mode]] · [[Direct-Mode]] — the prior microVM model this replaces
- [[Operatives]] — the `smooth_operator` agent runtime
- [[Data-Storage]] — Dolt vs. SQLite, what lives where

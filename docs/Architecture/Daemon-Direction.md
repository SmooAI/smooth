# Daemon Direction

#architecture #planned

> [!info] Where Big Smooth is headed (epic `th-c89c2a`)
> The teardown of the microVM stack is step one of a larger pivot: reimagining Big Smooth as a **single-tenant, open-source, always-on personal AI assistant** built on the `smooth-operator` engine — self-hosted the way people self-host hermes, one trusted operator per instance. This page is the direction, not the current runtime. Track the epic and its plan (`~/.claude/plans/transient-zooming-canyon.md`) for status.

## The pivot

The microVM existed to isolate **untrusted tenants** — which Smooth has decided it does not have. For a single-tenant, run-your-own instance the VM defends the wrong thing; the real risk is prompt-injection turning the operator's own trusted agent against them. So the plan drops the microVM substrate and rebuilds around:

- a **kernel OS-sandbox on tool subprocesses** + an **egress allowlist** + a **Claude-Code-style auto-mode permission engine** (see [[Security-Model]]);
- an **always-on daemon** on the `smooth-operator` engine that owns durable state and the agent runtime;
- the `th code` TUI and `smooth-web` SPA reimagined as thin frontends over one durable event surface (opencode's headless-server-with-multiple-frontends pattern).

### Decisions already made (not up for re-litigation)

- Single-tenant, run-your-own, open-source.
- A clean rewrite on `smooth-operator` (not an incremental migration).
- Drop microsandbox → kernel sandbox + egress proxy + auto-mode.
- Reimagine the React frontend as a hermes-style control surface.
- Path-dep the engine (`smooth-operator-core`) so it can be patched during the rewrite, upstreamed later.
- First build slice = the daemon spine + multi-frontend (prove headless-server / SSE+WS / TUI+web end-to-end before layering security).

## Target shape

```
┌────────────────────────────────────────────────────────────────┐
│  BIG SMOOTH DAEMON  (always-on; launchd / systemd via th service)│
│  axum + tokio, bound to loopback + tailnet only                  │
│                                                                  │
│   smooth-operator engine (Agent::run_with_channel per session)   │
│     ├─ ToolHook chain: rule engine → classifier → Narc           │
│     ├─ sandboxed tool-subprocess spawner (kernel-enforced)       │
│     ├─ egress proxy (separate process, real boundary)            │
│     ├─ Dolt session + checkpoint store (Groove resume)           │
│     ├─ durable completion + approval queues                      │
│     └─ cron scheduler + sub-agent delegation                     │
│                                                                  │
│   HTTP API ─┬─ /api/session   (list/create/get/prompt/interrupt) │
│             ├─ /api/permission (pending / reply)                 │
│             ├─ /api/event      (durable SSE, cursor resume)      │
│             └─ /ws             (token stream)                    │
└──────────┬───────────────────┬──────────────────┬───────────────┘
           │                   │                  │
     th code TUI         React control      messaging gateway
     (smooth-code)       surface (smooth-web) (Telegram/Slack, later)
```

Communication is REST for commands + a **durable SSE event surface** (every event persisted to a Dolt row and published; frontends resume from a monotonic cursor) with WS retained for token streaming. The daemon binds loopback + tailnet only, runs as a non-root dedicated user, and gates every endpoint with a bearer token.

## What already landed toward this

- The microVM/access-cast teardown (`th-f4a801`) — this rewrite's step one.
- Per-agent instructions/personality/greeting, `enabledTools` filtering + auth-level gating, and judge-advanced conversation stepping now live natively in the engine/servers (SMOODEV-590, smooth-operator PRs #125–129) — the daemon must **not** reimplement conversation workflows or per-agent persona; it composes with the engine's `AgentConfigResolver`.
- The [[Extension-System|SEP extension protocol]] (epic `th-2def2a`) — the extensibility layer the daemon and frontends will share.

## Related

- [[Security-Model]]
- [[Extension-System]]
- [[Architecture-Overview]]

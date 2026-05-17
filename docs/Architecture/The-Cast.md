# The Cast

#architecture #cast

> [!arch] One process or one VM, eight roles
> Every cast member runs as a tokio task or in-process service alongside Big Smooth. There are no per-actor VMs. In sandboxed mode they share the Boardroom microVM; in direct mode they share the `th` host process. Crate boundaries are preserved so each role keeps its own state, hooks, and tests.

## Cast at a glance

| Role            | Crate              | What it does                                       | Talks to                |
| --------------- | ------------------ | -------------------------------------------------- | ----------------------- |
| Big Smooth      | `smooth-bigsmooth` | Orchestrator, API, dispatch. READ-ONLY.            | Everyone                |
| Wonk            | `smooth-wonk`      | Policy engine; answers "is X allowed?"             | Big Smooth, Goalie, Narc |
| Goalie          | `smooth-goalie`    | Outbound HTTP/HTTPS proxy                          | Wonk                    |
| Narc            | `smooth-narc`      | Tool surveillance hook + LLM judge                 | Wonk, Big Smooth        |
| Scribe          | `smooth-scribe`    | Per-actor structured logging                       | Archivist               |
| Archivist       | `smooth-archivist` | Central log + event aggregator                     | Scribes, dashboard      |
| Diver           | `smooth-diver`     | Pearl lifecycle + Jira sync                        | Pearl store, Jira       |
| Groove          | `smooth-operator`  | LLM checkpointing + resume (in-process to operator) | Operator runner only    |

---

## Big Smooth

The orchestrator. The `axum` server on `:4400`. Owns:

- The REST + WebSocket API surface (20+ routes — pearls, sessions, access, dispatch).
- The `Orchestrator` state machine: `Idle → Scheduling → Dispatching → Monitoring → Reviewing`.
- The dispatch fork: direct vs sandboxed (see [[Dispatch]]).
- The teammate registry (the UI's "who's running right now").
- The access store (pending and approved access requests).

**Invariant: Big Smooth never writes user-code paths.** Operators write code; Big Smooth records, dispatches, and reports. The boardroom-mode Narc enforces this with the WriteGuard hook.

**Boots** via `crates/smooth-bigsmooth/src/bin/boardroom.rs` (in-VM) or via the host-side `cmd_up` path when running in direct mode.

---

## Wonk

The access-control authority. Pure policy → answer; no LLM.

- Reads policy TOML from `/etc/smooth/policy.toml` (inside the VM) or in-memory (direct mode).
- Exposes `/check/*` endpoints: `/check/network`, `/check/filesystem`, `/check/tool`, `/check/pearl`, `/check/mcp`, `/check/cli`.
- Hot-reloads via `notify` + `ArcSwap` when policy changes.
- Escalates uncertain calls to the [[#Narc|Narc]] LLM judge for a verdict.
- Negotiates expanded access at runtime: when a policy check fails, Wonk asks Big Smooth via `/api/access/request`. Big Smooth auto-approves or routes to inbox.

Goalie and Narc both delegate every decision to Wonk. A single source of policy truth.

---

## Goalie

The outbound HTTP/HTTPS forward proxy. Dumb pipe: every request asks Wonk, then forwards or returns 403.

- Binds an ephemeral loopback port; `HTTP_PROXY` + `HTTPS_PROXY` in operator env point to it.
- JSON-lines audit log (default `/tmp/goalie-boardroom.jsonl` for the boardroom Goalie; per-operator path otherwise).
- In sandboxed mode, microsandbox's `allow_host_loopback` exposes the host's gateway IP to the guest so Goalie can reach LLM providers, Docker / OrbStack / Kalima APIs, or anything the policy allows on `host.docker.internal`.

Goalie has no policy of its own. Everything is delegated.

---

## Narc

Tool surveillance. Two layers:

1. **Regex pre-filter.** Ten secret patterns (AWS keys, GitHub tokens, OpenAI keys, JWTs, etc.), six prompt-injection patterns ("ignore previous instructions", base64-encoded payloads, etc.), the write-path guard ([Big Smooth path], log paths only for Archivist).
2. **LLM judge.** Ambiguous cases get escalated to a small, fast model (Haiku, Flash, GPT-4o-mini) for a structured yes/no verdict. Cached.

Narc is wired as a `ToolHook` in `smooth-operator`. Every tool call passes through `pre_call` (block before exec) and `post_call` (block before result is handed back to LLM). Severity-tagged alerts are forwarded to Scribe.

In sandboxed mode Narc lives at the URL specified by `SMOOTH_NARC_URL`, which is set to a routable host interface IP (not `127.0.0.1`) so the guest can reach it across the VM boundary.

---

## Scribe

Per-actor structured logging. One Scribe per cast member (and one per operator). Each accepts:

- `LogEntry` POSTs from its actor.
- W3C `traceparent` propagation across process / VM boundaries.

A background `ForwarderHandle` batches entries and POSTs them to the [[#Archivist|Archivist]] URL. The forwarder URL is the Archivist as seen from the actor's network namespace.

---

## Archivist

Central log + event aggregator. One per Smooth instance — bound to `0.0.0.0:4401` inside the Boardroom VM (the guest port microsandbox forwards to the host so all Scribes can reach it, even across VM boundaries).

- Stores log entries in a `MemoryArchiveStore` (in-RAM) by default; persistent backends are pluggable.
- Stores rich agent events (`AgentEvent` variants) in `MemoryEventArchive`.
- Exposes `/query`, `/stats`, `/events` (SSE), `/health`. The dashboard subscribes to `/events`.

Archivist is the only cast member that writes — and it only writes to log paths. Narc enforces that.

---

## Diver

Pearl lifecycle manager. Wraps the [[Pearls|pearl store]] with:

- `dispatch(title, description, parent)` — creates a pearl, marks it in_progress, returns the ID.
- `complete(id)` — closes a pearl after dispatch completes successfully.
- Sub-pearl creation for child work spawned mid-dispatch.
- Optional Jira sync: when `JIRA_URL` + `JIRA_API_TOKEN` are set, Diver bidirectionally syncs pearl status with the configured Jira project.

Diver runs only when the cast is up with a pearl store. The `dispatch_ws_task_*` paths prefer Diver and fall back to the raw pearl store if Diver is absent.

---

## Groove

LLM checkpointing + session resume. Lives inside `smooth-operator` (not a separate process). Captures conversation state after every tool call. When an operator is interrupted (process exit, sandbox timeout, network cut), Groove can rebuild the in-flight conversation from the last checkpoint when resuming.

The pearl store is Groove's checkpoint backing — session messages, tool calls, and orchestrator snapshots all land in Dolt tables (`session_messages`, `orchestrator_snapshots`). See [[Data-Storage]].

---

## How they wire together

```
   Operator runner ─► tool call
                        │
                        ├──► NarcHook (pre-call) ───► Narc ─► Wonk
                        │                              │       │
                        │                              ▼       ▼
                        │                          regex   policy
                        │                          + LLM
                        │
                        ├──► (file write?)    ──► WriteGuard ─► Wonk
                        │
                        ├──► (HTTP fetch?)    ──► HTTP_PROXY ─► Goalie ─► Wonk
                        │
                        └──► result ─► NarcHook (post-call) ─► back to LLM
                                                │
                                                ▼
                                             Scribe
                                                │
                                                ▼
                                            Archivist
```

The agent loop sees one tool surface. The hooks transparently fan out to the cast.

## Related

- [[Architecture-Overview]]
- [[Sandboxed-Mode]]
- [[Direct-Mode]]
- [[Dispatch]]
- [[Operators]]

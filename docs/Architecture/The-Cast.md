# The Cast

#architecture #cast

> [!arch] One process or one VM, eight roles — two transports between them
> Every cast member runs as a tokio task or in-process service alongside Big Smooth. There are no per-actor VMs. In sandboxed mode they share the Boardroom microVM; in direct mode they share the `th` host process. Crate boundaries are preserved so each role keeps its own state, hooks, and tests.
>
> Within Big Smooth's process they talk over **`Arc`-shared state** (no wire, no serialization). Across the operator-runner subprocess boundary — which exists in both modes — they talk over **tonic gRPC on UDS**. See [[Transport]] for the full topology.

## Cast at a glance

| Role            | Crate              | What it does                                       | Talks to                | Transport surface                          |
| --------------- | ------------------ | -------------------------------------------------- | ----------------------- | ------------------------------------------ |
| Big Smooth      | `smooth-bigsmooth` | Orchestrator, API, dispatch. READ-ONLY.            | Everyone                | HTTP+WS `:4400` (out); gRPC `bigsmooth.sock` (in) |
| Wonk            | `smooth-wonk`      | Policy engine; answers "is X allowed?"             | Big Smooth, Goalie, Narc | gRPC `wonk.sock`                           |
| Goalie          | `smooth-goalie`    | Outbound HTTP/HTTPS proxy                          | Wonk                    | HTTP proxy (in-VM loopback)                |
| Narc            | `smooth-narc`      | Tool surveillance hook + LLM judge                 | Wonk, Big Smooth        | gRPC `narc.sock`                           |
| Scribe          | `smooth-scribe`    | Per-actor structured logging                       | Archivist               | gRPC `scribe.sock`                         |
| Archivist       | `smooth-archivist` | Central log + event aggregator                     | Scribes, dashboard      | HTTP `:4401`; SSE `/events`                |
| Diver           | `smooth-diver`     | Pearl lifecycle + Jira sync                        | Pearl store, Jira       | In-process to Big Smooth                   |
| Groove          | `smooth-operator`  | LLM checkpointing + resume                         | Operator runner only    | In-process to the operator-runner          |

The `.sock` files live in `$SMOOTH_SINGLE_PROCESS_SOCKET_DIR` (defaults to `$XDG_RUNTIME_DIR/smooth/` in-VM, a tempdir on the host in direct mode). `single_process::bootstrap_from_app_state` binds all four servers at startup; the operator-runner subprocess dials them with `NarcGrpcUds::connect`, `WonkClient`, etc. See `crates/smooth-bigsmooth/src/single_process.rs` and `proto/{narc,wonk,scribe,bigsmooth}.proto`.

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

**How operator-runner subprocesses reach Narc.** Each runner dials the local UDS at `$SMOOTH_SINGLE_PROCESS_SOCKET_DIR/narc.sock` (or the boardroom default `$XDG_RUNTIME_DIR/smooth/narc.sock`) via `smooth_wonk::NarcGrpcUds::connect` — see `crates/smooth-operator-runner/src/main.rs` ≈ line 1546. The wire is `tonic 0.12` + `prost 0.13` speaking the `smooth.narc.v1.Judge` service defined in `proto/narc.proto`. The legacy `SMOOTH_NARC_URL` HTTP fallback is only kept for old dispatch paths that haven't been ported to UDS.

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
                        ├──► NarcHook (pre-call) ─────► narc.sock ──► Narc ─► wonk.sock ─► Wonk
                        │                                              │                   │
                        │                                              ▼                   ▼
                        │                                          regex + LLM         policy
                        │
                        ├──► (file write?)    ──► WriteGuard ─► narc.sock ─► Narc
                        │
                        ├──► (HTTP fetch?)    ──► HTTP_PROXY ─► Goalie ─► wonk.sock ─► Wonk
                        │
                        └──► result ─► NarcHook (post-call) ─► back to LLM
                                                │
                                                ▼
                                          scribe.sock ─► Scribe ─► HTTP `:4401` ─► Archivist
```

The agent loop sees one tool surface. The hooks transparently fan out to the cast — every dashed/arrow into a `.sock` is a tonic gRPC call over a Unix-domain socket inside the same VM (sandboxed) or the same process tree (direct). The protos are at `proto/{narc,wonk,scribe,bigsmooth}.proto` and the wire types are codegen'd at build time by each crate's `build.rs` via `tonic-build`.

## Related

- [[Architecture-Overview]]
- [[Sandboxed-Mode]]
- [[Direct-Mode]]
- [[Dispatch]]
- [[Operators]]
- [[Transport]] — the gRPC + UDS topology in detail

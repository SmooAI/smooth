# Transport — gRPC over UDS, in-process Arc, and HTTP at the edge

#architecture #grpc #transport

> [!arch] Two boundaries, three transports
> Smooth has **two** real process boundaries: the host vs. the safehouse microVM (sandboxed mode) and Big Smooth vs. each operative subprocess (both modes). Cross-boundary calls inside the same VM (or host process tree) are **tonic gRPC over UDS**. Calls within Big Smooth's own process are **in-process `Arc`-shared state** — no wire, no serialization. The outer world (TUI, web UI, bench harness) speaks **HTTP + WebSocket** to Big Smooth on `:4400`.

## Why this matters

The marketing line "in-process services alongside Big Smooth" is true for the cast members co-located in Big Smooth's own address space — but every dispatched **operative runs as a separate subprocess** (`smooth-operative`), even in single-VM mode. Every tool the operative calls fires through `NarcHook → narc.sock` over gRPC; every policy check fires through Goalie → `wonk.sock` over gRPC; every structured log entry fires through `scribe.sock` over gRPC. The wire is real, it's load-bearing, and it's what makes "one VM, many operatives" possible without giving each operative its own kernel boundary.

## The four UDS gRPC servers

| Socket            | Crate                              | Service (`.proto`)         | Bound by                                       |
| ----------------- | ---------------------------------- | -------------------------- | ---------------------------------------------- |
| `narc.sock`       | `smooth-narc`                      | `smooth.narc.v1.Judge`     | `smooth_narc::grpc::serve_uds`                 |
| `wonk.sock`       | `smooth-wonk`                      | `smooth.wonk.v1.Wonk`      | `smooth_wonk::grpc::serve_uds`                 |
| `scribe.sock`     | `smooth-scribe`                    | `smooth.scribe.v1.Logger`  | `smooth_scribe::grpc::serve_uds`               |
| `bigsmooth.sock`  | `smooth-bigsmooth::orchestrator_grpc` | `smooth.bigsmooth.v1.Orchestrator` | `crate::orchestrator_grpc::serve_uds`     |

All four are spawned together by `crates/smooth-bigsmooth/src/single_process.rs::bootstrap_from_app_state`. There is a fifth proto (`host_stub.proto`) for the host-side credential broker — it runs *outside* the VM and is reached by guest processes via the `creds UDS` bind-mount.

## Where the sockets live

The runtime location is controlled by `$SMOOTH_SINGLE_PROCESS_SOCKET_DIR`:

- **Sandboxed mode (in-VM):** defaults to `$XDG_RUNTIME_DIR/smooth/` inside the microVM, i.e. `/run/user/0/smooth/` for the root-uid safehouse.
- **Direct mode (host):** the safehouse is the host process, so the socket dir is a per-launch tempdir under `~/.smooth/run/` (see `cmd_up` for the exact construction).
- **Tests:** override with `SMOOTH_SINGLE_PROCESS_SOCKET_DIR=/tmp/whatever` — `single_process::tests::bootstrap_spawns_all_four_sockets` does exactly this.

The operative subprocess discovers them the same way:

```rust
// crates/smooth-operative/src/main.rs ≈ 1540
let socket_dir = std::env::var("SMOOTH_SINGLE_PROCESS_SOCKET_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|_| PathBuf::from("/run/user/0/smooth"));
let narc_sock = socket_dir.join("narc.sock");
match smooth_wonk::NarcGrpcUds::connect(narc_sock.clone()).await { … }
```

The runner is spawned with this env var set by Big Smooth's dispatch path (`dispatch_ws_task_direct` / `dispatch_ws_task_sandboxed`), so a runner started any other way (manual exec, debugger) needs the var set explicitly.

## Wire types — proto + tonic-build

Source of truth: the workspace-root `proto/` directory.

```
proto/
├── bigsmooth.proto    # Orchestrator service — pearl dispatch, access negotiation
├── host_stub.proto    # Credential broker (host-side)
├── narc.proto         # ToolCheck, WriteCheck, judge verdicts
├── scribe.proto       # LogEntry stream, query
└── wonk.proto         # NetworkCheck, FileCheck, ToolCheck, PearlCheck
```

Each gRPC-serving crate has a `build.rs` that points `tonic-build` at the workspace `proto/` dir, generating `pb` modules at compile time (`smooth.{narc,wonk,scribe,bigsmooth}.v1`). The generated types are re-exported via `tonic::include_proto!` and consumed by both the server (`grpc.rs`) and client (`*GrpcUds::connect`) sides.

Workspace pins:

```toml
# Cargo.toml
tonic       = "0.12"
tonic-build = "0.12"
prost       = "0.13"
prost-types = "0.13"
```

## The complete call graph for one tool call

```text
LLM emits tool call
        │
        ▼  (host trait, in-process)
NarcHook::pre_call         (smooth-narc::ToolHook impl)
        │
        ▼  (gRPC over narc.sock)
smooth.narc.v1.Judge.CheckTool
        │
        ├──► regex pre-filter
        │
        └──► (gRPC over wonk.sock)
             smooth.wonk.v1.Wonk.CheckTool
                    │
                    ▼
                policy.toml verdict + ArcSwap
                    │
                    ▼  (in-process)
             Wonk::AccessNegotiator
                    │
                    ▼  (gRPC over bigsmooth.sock when escalating)
             smooth.bigsmooth.v1.Orchestrator.RequestAccess
                    │
                    ▼  (HTTP+WS to outside world)
             dashboard inbox / auto-approve
        ◄────────────────
verdict returned
        │
        ▼
operative runs tool (or returns 403)
        │
        ▼  (gRPC over scribe.sock)
smooth.scribe.v1.Logger.Log
        │
        ▼  (HTTP)
Archivist :4401 ─► SSE /events ─► dashboard
```

Every `gRPC over X.sock` step is a single tonic call on a local UDS — no network hop, no TLS, no auth header (the UDS perms are the auth boundary). RTT is microseconds.

## What's NOT gRPC

| Surface                      | Transport                                          |
| ---------------------------- | -------------------------------------------------- |
| TUI / web UI ↔ Big Smooth    | HTTP `:4400` + WebSocket                           |
| Scribe → Archivist           | HTTP POST `:4401` (Archivist server is `axum`, not tonic) |
| Operator HTTP egress         | HTTP through Goalie's proxy (Goalie ↔ Wonk uses gRPC) |
| Goalie internal              | In-process; reads policy via the Wonk gRPC client  |
| Groove ↔ Operator            | In-process to `smooth-operator`                    |
| Diver ↔ Big Smooth           | In-process; shares `AppState` with the orchestrator |
| Within Big Smooth's process  | `Arc<AppState>` — no transport at all              |

Archivist is HTTP-only because the Scribes batch and the dashboard wants SSE; a gRPC port-mirror would be redundant. Goalie is HTTP because it's a forward proxy speaking HTTP_PROXY semantics to the operator's HTTP client; its own decisions ride gRPC into Wonk.

## Tests that prove this is wired

```
crates/smooth-narc/src/grpc.rs      4 #[tokio::test]
crates/smooth-wonk/src/grpc.rs      7 #[tokio::test]
crates/smooth-wonk/src/narc_grpc_uds.rs  2 #[tokio::test]
crates/smooth-bigsmooth/src/single_process.rs:
    - is_enabled_reads_env_var_when_set
    - bootstrap_spawns_all_four_sockets
    - each_socket_serves_its_grpc_after_bootstrap
    - shutdown_removes_socket_files
```

The single_process integration tests bind the full four-server set, round-trip a real gRPC call against each one, then assert clean shutdown. They run on every `cargo test --lib -p smooai-smooth-bigsmooth`.

## Adding a new RPC

1. Edit the relevant `.proto` under `proto/`.
2. The crate's `build.rs` re-runs `tonic-build` on the next `cargo build`; new types appear in `pb`.
3. Add the server impl in that crate's `grpc.rs` (the `Service` trait method).
4. Add the client wrapper in the consuming crate (typically `crates/smooth-operative/src/main.rs` for runner-side, or in the cast crate that wants to call out).
5. Wire it into `single_process::bootstrap_from_app_state` if it's a new server socket.
6. Add a unit test in `grpc.rs::tests` and an integration test in `single_process::tests`.

## Related

- [[The-Cast]] — what each service owns
- [[Sandboxed-Mode]] — where the sockets live in the microVM
- [[Direct-Mode]] — same sockets, host tempdir instead
- [[Dispatch]] — how the operative is spawned and given the socket dir

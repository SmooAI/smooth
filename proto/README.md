# Single-VM Smooth — proto-driven architecture

Pearl th-893801. Design artifacts for the architecture migration from
per-bead microVMs to a single sandbox VM containing all of Smooth's
services.

## Why

Today's architecture has two trust boundaries:

1. **Host ↔ Safehouse** — Big Smooth on host, talking to per-bead VMs.
2. **Safehouse ↔ Operator** — per-VM Wonk/Goalie/Scribe inside each
   microVM.

The safehouse↔operator boundary protects against attacks the threat
model doesn't actually cover (Smooth's worry is prompt injection of an
honest LLM, not a maliciously fine-tuned model). Removing it collapses
a *lot* of plumbing: SMOOTH_NARC_URL detection, host.containers.internal
workarounds, per-bead VM spawn pools, cross-VM HTTP. The auto-mode
work in iters 1-9 had to thread around this complexity; with the
collapse, the same flow becomes localhost UDS gRPC.

The host↔sandbox boundary — the one that protects the user's home dir,
ssh keys, AWS creds, etc — stays intact. It's enforced by a single
small host stub binary (<500 lines) exposing one RPC: IssueCredential.

## The picture

```
HOST                                          SANDBOX VM
┌──────────────────────┐                      ┌─────────────────────────────────────────┐
│                      │                      │                                         │
│  TUI                 │  HTTP+WS :4400       │  Big Smooth (axum + gRPC)               │
│  (smooth-code)       │  (port-forward)      │  │                                      │
│                      │ ←──────────────────→ │  ├── Narc          (UDS)                │
│  Host CLIs           │                      │  ├── Wonk          (UDS)                │
│  - gh                │  /run/smooth/        │  ├── Goalie        (HTTP proxy + UDS)   │
│  - aws               │  host.sock           │  ├── Scribe        (UDS)                │
│  - tailscaled        │  ↔──────────────────→│  │                                      │
│  - ssh-agent         │  IssueCredential     │  └── operator-runner subprocesses       │
│                      │                      │     (one per dispatched pearl)          │
│  Docker daemon       │  /var/run/docker.sock│                                         │
│  (or Colima/OrbStack)│  ↔──────────────────→│  in-sandbox CLIs:                       │
│                      │                      │    git, gh, aws, kubectl, docker,       │
│  Tailscale daemon    │  $SSH_AUTH_SOCK      │    npm, pnpm, cargo, ssh, tailscale...  │
│                      │  ↔──────────────────→│                                         │
│                      │                      │  bind-mounts from host:                 │
│                      │                      │    /workspace/                          │
│                      │                      │    /var/smooth/grants.toml              │
│                      │                      │    /var/smooth/pearls/  (Dolt)          │
│                      │                      │    /var/smooth/learned/                 │
│                      │                      │                                         │
└──────────────────────┘                      └─────────────────────────────────────────┘
```

## The protos

| File | Service | Notes |
|------|---------|-------|
| `narc.proto` | The judge | Decision::Ask never returned through Judge — internally held + replayed |
| `wonk.proto` | Policy gate | CheckNetwork/Tool/Cli/File, escalates to Narc when uncertain |
| `goalie.proto` | HTTP proxy mgmt | TODO — data plane stays HTTP; control plane is gRPC |
| `scribe.proto` | Logger | Client-streaming ingest, server-streaming query |
| `bigsmooth.proto` | Orchestrator | Dispatch + AccessStore + operator events |
| `host_stub.proto` | The poke hole | One RPC: IssueCredential |

## How the trust boundaries fall

- **Sandbox is self-sufficient.** All CLIs live in the image. File IO via bind-mounts. Network via Goalie+Wonk gating.
- **Credentials flow in on demand.** Per-tool credential-provider shims (git-credential-smooth, aws-credential-process, etc) call HostStub.IssueCredential. Tokens are short-lived; the source-of-truth stays on host.
- **Docker is the exception we accept.** Can't run nested. Host's `/var/run/docker.sock` is bind-mounted in (with auto-detection for Colima / OrbStack / Rancher / Podman). In-sandbox docker CLI talks to host daemon; an in-sandbox NarcHook gates dangerous subcommands.
- **SSH works through Tailscale userspace in the sandbox + ssh-agent forwarding.** Sandbox becomes its own tailnet device; SSH key never crosses the boundary; tailnet routing carries the connection.

## Migration phases

1. **Collapse service topology + first gRPC pass.** Refactor BS so all services run in one process. Switch internal HTTP+JSON to gRPC over UDS. No microsandbox change yet. ~1.5 weeks.
2. **Host stub + sandbox image + `th up` boot.** Build smooth-host-stub. Build sandbox image with all the CLIs. Wire `th up` to boot one VM, port-forward 4400 to host. ~2 weeks.
3. **Persistent state + learned context.** Bind-mount the state dirs. Add the learned-context collector. ~1 week.
4. **Cleanup.** Drop `safehouse_*` terms, `host_tool` as a user-visible tool, `SMOOTH_NARC_URL` detection. ~3 days.

Total: ~5 weeks. The endpoint is a system that's *materially* simpler than today's.

## Decisions locked in

- **gRPC via tonic + prost + tonic-build.** Adds tonic-reflection in dev for grpcurl; pbjson at the BS edge so the TUI's HTTP shim keeps human-curlable.
- **UDS over TCP** for in-sandbox IPC. Bind-mount is the access control. No port allocation drama.
- **Cast stays as separate services** inside the VM. Not collapsed into BS's process — crash isolation, clear contracts, matches existing crate boundaries.
- **Operators reach cast direct** for hot RPCs (judge, check, log); **through BS** for control plane (dispatch, AccessStore). Latency-sensitive paths stay flat.
- **Docker daemon detection** at `th up` (DOCKER_HOST → probe known runtimes → user config). Auto-handles Colima / OrbStack / Rancher / Podman.
- **Tailscale userspace in the sandbox** for tailnet access. SSH via agent forwarding (bind-mounted SSH_AUTH_SOCK). Tailnet trust is opt-in.

## Decisions deferred

- Streaming variants of CliExec/DockerExec (long-running builds, tail -f). Add as v2 RPCs once we hit the need.
- SSH cert auth via Vault/Teleport — pattern is the same as the existing credential broker; another `IssueCredential` backend. Add when someone asks.
- Multi-workspace concurrency. v1 = one sandbox per `th up`; concurrent workspaces = multiple `th up` instances on different ports. Revisit if it gets painful.
- Goalie's gRPC control plane (allowlist reload, audit query). Data plane stays HTTP forward proxy. Defer the control plane proto until Phase 2.

## Things that should NOT exist after the migration

- `safehouse`, `safehouse_narc`, `SafehouseNarc` — the word goes. There's no safehouse in the new model.
- `host.containers.internal` workarounds.
- `detect_routable_host_ip()` in dispatch.
- `SMOOTH_NARC_URL` discovery.
- `dispatch_ws_task_sandboxed` vs `dispatch_ws_task_direct` branching — one dispatch path.
- The per-VM `spawn_cast()` function in operator-runner. Cast members are sandbox-singleton processes.
- The per-bead microsandbox pool.
- The `host_tool` agent-visible tool. Internal tools (gh_*, aws_*, etc.) call IssueCredential as needed; the agent sees them as normal tools.

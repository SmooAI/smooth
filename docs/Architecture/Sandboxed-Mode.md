# Sandboxed Mode

#architecture

> [!arch] The default
> `th up` boots one microsandbox microVM from the Safehouse OCI image. Big Smooth, the cast, and every dispatched operator runner all live inside that single VM. No per-task VMs, no Docker, no nested virtualization.

## What runs inside the VM

```
   ┌────────────────────────────────────────────────────────┐
   │  Safehouse microVM (microsandbox · libkrun / HVF)      │
   │  Image: ghcr.io/smooai/safehouse:latest                │
   │  Guest port :4400 → host :4400 (Big Smooth HTTP+WS)    │
   │  Guest port :4401  Archivist HTTP (in-VM only)         │
   │                                                        │
   │   tokio runtime — Big Smooth process                   │
   │   ├── Big Smooth      (axum :4400; gRPC bigsmooth.sock)│
   │   ├── Wonk            (gRPC wonk.sock)                 │
   │   ├── Goalie          (in-VM HTTP proxy)               │
   │   ├── Narc            (gRPC narc.sock + LLM judge)     │
   │   ├── Scribe          (gRPC scribe.sock)               │
   │   ├── Archivist       (HTTP :4401 + SSE /events)       │
   │   └── Diver           (in-process)                     │
   │                                                        │
   │   Operator runner(s) — one subprocess per pearl        │
   │   ├── Dials narc.sock / wonk.sock / scribe.sock        │
   │   └── via tonic gRPC over UDS (no network hop)         │
   │                                                        │
   │   UDS dir: $XDG_RUNTIME_DIR/smooth/  (in-VM default,   │
   │           override via SMOOTH_SINGLE_PROCESS_SOCKET_DIR)│
   │                                                        │
   │   Bind mounts:                                         │
   │   ├── /workspace         (RW; the user's repo)         │
   │   ├── /opt/smooth/bin    (RO; runner binary)           │
   │   ├── /opt/smooth/policy (RO; per-task policy TOML)    │
   │   └── /opt/smooth/cache  (RW; named volume cache)      │
   └────────────────────────────────────────────────────────┘
```

The boot path is in `crates/smooth-cli/src/main.rs::start_sandboxed_vm` (calls `microsandbox::create_sandbox`) and `crates/smooth-bigsmooth/src/bin/safehouse.rs` (the binary the VM runs). The four gRPC servers are bound by `crates/smooth-bigsmooth/src/single_process.rs::bootstrap_from_app_state`. See [[Transport]] for the full wire story.

## Boot sequence

1. `th up` on the host.
2. `start_sandboxed_vm(port)` builds a `SandboxConfig`:
   - `image: "ghcr.io/smooai/safehouse:latest"` (env-overridable via `SMOOTH_SAFEHOUSE_IMAGE`)
   - `cpus: 2`, `memory_mb: 4096`
   - `allow_host_loopback: true` (exposes `host.docker.internal` to the guest)
   - `extra_ports`: `{guest: 4400, host: 4400}`
   - Env: `SMOOTH_VM_MODE=1`, `SMOOTH_SAFEHOUSE_MODE=1`, `SMOOTH_SINGLE_PROCESS=1`, `SMOOTH_SAFEHOUSE_PORT=4400`
3. `create_sandbox(&config, 0).await` — microsandbox SDK boots the VM out-of-process.
4. The VM's `safehouse` binary calls `spawn_safehouse_cast()`. Every cast member binds a port and is held in a `SafehouseHandles` struct.
5. `BigSmooth::server::start` runs on `0.0.0.0:4400`. `th` exits 0; microsandbox owns the VM's lifecycle.

## Tearing down

`th down` reads `~/.smooth/sandboxed.vm` (the VM name stashed by `th up`), calls `destroy_sandbox`, and removes the state file. Idempotent.

## Why one VM (not many)

The previous architecture spawned a per-operator microVM via Bootstrap Bill (the host-side broker). That gave each operator its own kernel boundary but at significant cost:

- Cold start was several seconds per task (microVM boot + image pull + bind-mount).
- Apple HVF has no nested virtualization, so a "safehouse VM that spawns operator VMs" required Bill on the host to do the spawning, then port-forwarding back to Big Smooth.
- Every dispatch was a network hop; logs and tools required cross-VM URL plumbing.

The current model: one VM, one tokio runtime, one tool surface, one cast. Operators are dispatched as runner subprocesses inside the same VM. The isolation boundary is the microVM itself (hardware) plus the in-VM cast (kernel-enforced egress proxy via Goalie's iptables, FUSE on `/workspace`, NarcHook on every tool call). The threat model is identical; the cold start is a single VM boot at `th up` time.

> [!todo] Code reality vs intent
> The merged-into-`th up` consolidation removed the host-side Bill broker for the safehouse VM. The operator-dispatch path (`dispatch_ws_task_sandboxed` in `crates/smooth-bigsmooth/src/server.rs`) still calls `create_sandbox` per task, which works on the host but would require nested virt inside the Safehouse VM. The remaining work is to flip operator dispatch to spawn the runner as a subprocess inside the Safehouse VM (or a sibling tokio task) instead of a fresh microVM. Until that lands, sandboxed-mode operator dispatch from inside the safehouse is a known gap; the workaround during the transition is to run `th up direct` for end-to-end loops.

## Outbound to the host

You often want the agent to reach a service running on the host: a Docker daemon, OrbStack, Kalima, a local LLM gateway. `allow_host_loopback: true` is the only setting needed — microsandbox exposes the host's gateway as `host.docker.internal` to the guest. No nested virt, no port-forward fiddling. Wonk policy gates which hosts the operator may actually dial.

## Why microsandbox

- Hardware isolation without a heavyweight VMM.
- Boot in single-digit seconds.
- OCI-image input — same toolchain as Docker but with microVM semantics.
- Rust SDK (we embed the crate; no external `msb` CLI dependency at runtime).

## Related

- [[Architecture-Overview]]
- [[Direct-Mode]]
- [[Operators]]
- [[Dispatch]]

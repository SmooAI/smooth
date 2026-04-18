<div align="center">

# smooth-bootstrap-bill

**Bootstrap Bill — The Board's host-side broker**

*Bill walks between worlds. Big Smooth lives inside the Boardroom VM where the hypervisor cannot be reached. Bill lives on the host where the hypervisor is. He takes his orders from Big Smooth, and he spawns the pods nobody else can.*

[![crates.io](https://img.shields.io/crates/v/smooai-smooth-bootstrap-bill)](https://crates.io/crates/smooai-smooth-bootstrap-bill)
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/SmooAI/smooth/blob/main/LICENSE)

</div>

---

## Why Bill exists

Big Smooth, the orchestrator, lives inside a hardware-isolated Boardroom microVM for a reason — it's the one process in the cast that's `READ-ONLY` and never touches the host. But HVF on Apple Silicon (and KVM under most configs) don't offer nested virtualization. Big Smooth cannot call `microsandbox` from inside its own VM to spawn operator pods.

Bill is the one process on the host that holds `microsandbox::Sandbox` handles. Everyone else — Big Smooth, Archivist, the test harness, the `th` CLI itself in Boardroom mode — asks Bill over TCP loopback (via `host.containers.internal` when the caller lives inside a VM) and Bill does the actual spawn, exec, destroy.

## Protocol

Line-delimited JSON over TCP. One request per connection, one response line, close. No request IDs, no multiplexing, no streaming. Keep it dumb; the smart part lives elsewhere.

```
┌──────────────────────┐              ┌──────────────────────┐
│   Big Smooth         │              │   Bootstrap Bill     │
│   (Boardroom VM)     │ ── TCP ───▶  │   (host process)     │
│                      │  loopback    │                      │
│   orchestrator       │              │   holds microsandbox │
│   READ-ONLY          │              │   Sandbox registry   │
└──────────────────────┘              └──────────────────────┘
                                              │
                                              ▼
                                      ┌──────────────────────┐
                                      │  Operator microVMs   │
                                      │  (spawned on demand) │
                                      └──────────────────────┘
```

Part of **[Smooth](https://github.com/SmooAI/smooth)**, the security-first AI-agent orchestration platform.

## What's in the box

- **`server`** (feature-gated behind `server`) — Bill's TCP listener + registry. The only place in the workspace that holds `microsandbox::Sandbox` handles.
- **`BillClient`** — thin TCP client used by Big Smooth and friends to send `BillRequest::Spawn|Exec|Destroy|List|Ping` and get a `BillResponse` back.
- **`BindMountSpec` / `SandboxSpec` / `PortMapping`** — shared wire types. Importing just these is zero-cost (no microsandbox dep pulled in).
- **`run_bind_all_proxy`** — TCP forwarder from `0.0.0.0:<port>` to `127.0.0.1:<port>`, so guest-to-guest traffic can reach a microsandbox-published port (microsandbox binds 127.0.0.1-only by default).

## Usage

Most callers only want the client:

```rust
use smooth_bootstrap_bill::BillClient;
use smooth_bootstrap_bill::protocol::SandboxSpec;

let bill = BillClient::new("http://host.containers.internal:8650");
let spec = SandboxSpec { /* image, mounts, env, ports, … */ ..Default::default() };
let (_name, _ports, _created) = bill.spawn(spec).await?;
```

Run the server binary (`scripts/bootstrap-bill-server`) or, in tests, call the in-process `server::spawn_sandbox` / `exec_sandbox` / `destroy_sandbox` functions directly (enable the `server` feature).

## License

MIT

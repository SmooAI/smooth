# ADR-001: Consolidate Boardroom and Operator VMs into one microsandbox VM

#decision

**Date**: 2026-05
**Status**: Accepted

---

## Context

The earlier architecture (documented in the security white paper, `docs/white-paper-security-architecture.md`) ran two distinct kinds of microVM:

- A **Boardroom microVM** hosting Big Smooth + the cast.
- One **Operator microVM per dispatched task**, spawned by Bootstrap Bill (a host-side broker), because Apple HVF doesn't support nested virtualization and Big Smooth couldn't call `microsandbox` from inside its own VM.

This bought us per-task kernel isolation but cost us:

- Multi-second cold starts per task (image pull + VM boot + bind-mount).
- A required host-side broker process (`smooth-bootstrap-bill`) that held the `microsandbox::Sandbox` handles and accepted requests over TCP.
- Cross-VM URL plumbing for every cast call (Archivist URL had to be the host's interface IP, not loopback).
- A separate Docker-based `th vm` subsystem the user was running in parallel for "Smooth on Linux without nested virt".

User directive (2026-05-17): "I do not want Docker. We don't even have a boardroom anymore. I want microsandbox to be where the sandbox smooth lives or you can run smooth without the sandbox if it's in its own trusted environment."

---

## Decision

- **`th up`** (default) boots one microsandbox microVM. Big Smooth, the cast, and operator runners all live inside it.
- **`th up direct`** is the escape hatch for trusted environments — runs the same stack on the host with no VM.
- The `th vm` subsystem (Docker container + named volume + credential broker) is removed entirely.
- Per-task operator VMs are no longer spawned. Operators are dispatched as runners inside the existing VM (or inside the existing host process in direct mode).

---

## Reasoning

### Cost of the per-task VM model wasn't paying for itself

The microsandbox boundary at `th up` time is the security guarantee that matters. Adding another microVM layer per task adds another kernel boundary that, in practice, only contributes to cold start. The in-VM cast (Wonk + Goalie + Narc + NarcHook + WriteGuard) already enforces the agent-vs-cast boundary; that's the boundary the threat model actually cares about.

### One mental model

Before: "is this a Boardroom service, an Operator service, or a host service? Where does its config come from? Where does it log to?" — six versions of the answer depending on the path.

After: one VM, one tokio runtime, one cast. Everything either runs in the VM (sandboxed) or on the host (direct). Done.

### Removes the Bill broker

Bootstrap Bill existed because the Boardroom VM couldn't spawn other VMs. With no per-task VM, there's nothing for Bill to do. The host-side broker is gone, which removes a TCP attack surface and a lot of plumbing.

### Docker was orthogonal

`th vm` (Docker container + named volume) was a third runtime path that existed mostly because microsandbox isn't on every platform yet. Removing it forces the right question — make microsandbox the substrate, ship a direct-mode escape hatch for the rest.

---

## Implementation

- `crates/smooth-cli/src/main.rs`: `cmd_up` becomes a 2-mode fork (`Some(UpMode::Direct)` vs. default). `start_sandboxed_vm` boots the boardroom OCI image via the embedded microsandbox SDK.
- `crates/smooth-cli/src/vm.rs`: deleted (413 lines).
- `docker/Dockerfile.smooth-vm`, `scripts/build-smooth-vm-image.sh`: deleted.
- `--sandboxed` and `--sandbox-backend` flags on `th up`: gone (sandbox is the default).
- `crates/smooth-bigsmooth/src/bin/boardroom.rs`: the in-VM Big Smooth binary. Spawns the cast as tokio tasks via `spawn_boardroom_cast`. Holds `BoardroomHandles` on `AppState`.

See the changeset at `.changeset/consolidate-vm-into-up.md` for the user-facing release note.

---

## Consequences

### Positive

- Single mental model for dispatch (one VM or one process; one cast; one runner).
- ~1.5s saved per task (no operator VM boot).
- Removes Bill TCP surface area and ~1.5k lines of host-side broker code.
- `th up` and `th down` are now the entire lifecycle.

### Negative

- Per-task kernel isolation is gone. The threat model now relies on the in-VM cast (NarcHook, WriteGuard, Goalie iptables, FUSE on `/workspace`) to keep operators from compromising other operators or Big Smooth.
- Operator dispatch from inside the Boardroom VM is mid-transition — the existing `dispatch_ws_task_sandboxed` still calls `create_sandbox` per task, which works on the host but would require nested virt inside the Boardroom. A follow-up flips this to spawn the runner as a sibling tokio task or VM-local subprocess. Until then, end-to-end loops should run in direct mode.

### Neutral

- `~/.smooth/sandboxed.vm` replaces the `th vm` state files.
- Outbound to host services (Docker / OrbStack / Kalima) still works via `allow_host_loopback`; no nested virt required.

---

## Alternatives Considered

### Keep per-task operator VMs

Rejected — cold start dominates the dev loop and the kernel-isolation-per-task guarantee wasn't material to the threat model. The in-VM cast is the real boundary.

### Drop the microVM entirely, ship only direct mode

Rejected — the microVM is the hardware-isolation pitch. Direct mode is a useful escape hatch but not the default. Untrusted prompts in an untrusted environment should never see the host directly.

### Switch microVMs to Firecracker / Cloud Hypervisor

Out of scope. microsandbox already gives us a fast Rust-native SDK; replacing the hypervisor is a separate decision.

---

## Related

- [[ADR-Index]]
- [[../Architecture/Sandboxed-Mode]]
- [[../Architecture/Direct-Mode]]
- [[../Architecture/The-Cast]]

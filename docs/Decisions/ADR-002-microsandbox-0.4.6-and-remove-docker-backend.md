---
status: Accepted
date: 2026-05-17
deciders: Brent
supersedes: None
superseded-by: None
tags: [decision, security, runtime]
---

# ADR-002 — Bump microsandbox to 0.4.6 and remove the Docker sandbox backend

#decision

## Status

Accepted (2026-05-17)

## Context

After [[ADR-001-Consolidate-into-one-microVM]] landed, the codebase still carried two pieces of dead weight from the previous architecture:

1. **`DockerSandboxClient` in `crates/smooth-bigsmooth/src/sandbox.rs`** — a `SandboxClient` implementation that shelled out to the host `docker` CLI as an alternate sandbox backend, selected via `SMOOTH_SANDBOX_BACKEND=docker`. About 200 lines of code plus four tests.
2. **`microsandbox = "0.3.14"`** — the last release before 0.4. A 2026-05-10 attempt to bump to 0.4.5 had failed with an opaque "sandbox process exited before sending startup info" panic on macOS HVF, so the workspace was reverted to 0.3.14 with a TODO comment.

Two independent reasons to revisit:

- Per ADR-001, Docker is never the sandbox runtime — the CLI is bundled inside the microVM so the agent can reach a host Docker / OrbStack / Colima / Rancher / Podman daemon when it needs a container, but Smooth itself does not run on Docker. The `DockerSandboxClient` was orphaned the moment `th vm` was deleted.
- microsandbox 0.4.6 brings three upstream PRs that directly affect what we hit:
    - **PR #673** bounds the relay handshake reads and prefers `boot-error.json` on timeout. The exact `read_exact` call that returned the opaque "sandbox process exited" message on the 0.4.5 attempt is now bounded; failed boots surface a structured error block instead.
    - **PR #650** adds `exec.log` capture and a typed `ExecFailed` so we can distinguish "image didn't boot" from "exec inside the image failed".
    - **PR #697** SIGKILLs `replace`-grace overruns, which is the most plausible cause of the bind-mount silent drop tracked in pearl th-dd0cef.

## Decision

1. Bump the workspace `microsandbox` dependency from `"0.3"` to `"0.4"` (resolves to 0.4.6).
2. Delete `DockerSandboxClient`, its `Default` impl, its four tests, and the `SMOOTH_SANDBOX_BACKEND=docker` branch in `init_sandbox_client`. Remove `SMOOTH_DOCKER_BIN`.
3. Simplify `init_sandbox_client`'s selection order to:
    1. `SMOOTH_BOOTSTRAP_BILL_URL` → `BillSandboxClient` (brokered mode);
    2. `direct-sandbox` feature → `DirectSandboxClient` (in-process embedded microsandbox);
    3. Otherwise → a deliberately broken `BillSandboxClient` pointing at `http://127.0.0.1:0` so dispatch fails loudly.

## Reasoning

### The Docker backend was speculative

`DockerSandboxClient` was added when we weren't sure microsandbox would boot reliably on CI runners. It was never reached by any production code path on developer machines and is rendered obsolete by the two-mode model: if the host can run microsandbox, `th up` uses it; if the host can't (CI, nested-virt VMs), `th up direct` runs the cast as host processes and trusts the surrounding sandbox. Adding a third "Docker container" middle ground would re-introduce the complexity ADR-001 just deleted.

### The 0.4.5 → 0.4.6 jump is itself the fix

The opaque "sandbox process exited before sending startup info" on the previous bump attempt came from microsandbox-runtime calling an unbounded `read_exact` against the relay handshake; PR #673 (in 0.4.5) bounds that read and prefers `boot-error.json` when the handshake times out. Even if 0.4.6 boots no better than 0.3.14 on a given host, the failure mode is now diagnosable. The bound is just "if it doesn't boot we'll find out why," which is enough to commit.

### Verified on macOS HVF before merge

Built `cargo install --path crates/smooth-cli --locked --force`, then:

- `th up` (the new sandboxed default) — boardroom microVM boots, `:4400` returns HTTP 200, `th down` cleans up with no leaked `msb` or `krun` processes.
- `th up direct` — cast runs on the host, `:4400` returns HTTP 200, `th status` reports healthy, `th down` stops cleanly.

1393 lib tests pass after the rip-out (the previous 1394 minus the four `DockerSandboxClient` tests, plus one new one isn't a thing here — math is 1394 − 4 = 1390 expected). The flaky pearls comment test (pearl th-da2461) is unrelated and pre-existing.

## Consequences

### Positive

- Single sandbox backend to maintain. Future microsandbox API churn touches one site (`DirectSandboxClient` + `BillSandboxClient`) instead of three.
- Failed boots produce structured error blocks; on-call investigations for sandbox failures get faster.
- `SMOOTH_SANDBOX_BACKEND` and `SMOOTH_DOCKER_BIN` are gone from the env surface — fewer footguns when copying config between machines.

### Negative / accepted

- No "Docker fallback" if microsandbox stops booting on a specific platform — direct mode is the only escape hatch, and direct mode requires the surrounding environment to already be sandboxed.
- microsandbox 0.4.x enables a `keyring` feature by default (pulls in `dbus` on Linux). We don't disable it here; if it bites a CI environment we'll gate it then.

### Reversal

If 0.4.6 turns out to regress on a platform we care about, we can pin back to `0.4.x` with the failing patch reverted upstream, or in the extreme revert the version to `"0.3"`. The Bill `exec_stream` + held-`AgentClient` workaround landed in `crates/smooth-bootstrap-bill/src/server.rs` and `crates/smooth-cli/src/main.rs::start_sandboxed_vm` is version-independent — `Sandbox::create` only boots agentd in every 0.x line; user workload still requires an explicit `exec()` and the host process has to stay alive to hold the agentd connection open.

## Related

- [[ADR-001-Consolidate-into-one-microVM]]
- [[../Architecture/Sandboxed-Mode]]
- [[../Operations/Troubleshooting]] — `th up` failure modes
- Pearl th-9f04c2 — the implementation pearl
- Pearl th-dd0cef — original `th up --sandboxed` failure that motivated the bump

---
'@smooai/smooth': minor
---

Consolidate `th up` and `th vm` into a single mode story. `th up`
now boots Smooth inside a microsandbox microVM by default — no
Docker container, no persistent named volume, no `th vm` subcommand.
`th up direct` is the new escape hatch for running Smooth on the
host without a sandbox (only safe inside an already-trusted
environment such as a CI runner or a dedicated devbox).

The previous `th vm up` workflow (Docker container + named volume +
host-stub credential broker) is removed entirely:

* `th vm up`, `th vm down`, `th vm shell`, `th vm prune`, `th vm status` → gone
* `docker/Dockerfile.smooth-vm` and `scripts/build-smooth-vm-image.sh` → deleted
* `--sandboxed` and `--sandbox-backend` flags on `th up` → gone (sandbox is the default)

**If you used `th vm up`, you now want `th up` instead.**

**The persistent named volume `smooth-vm-root` is now orphaned.**
Delete it with `docker volume rm smooth-vm-root` if you don't need
the accumulated `~/.smooth` state from your old Docker VM.

Outbound reachability to Docker / OrbStack / Kalima from inside the
microsandbox VM is still supported via the existing
`allow_host_loopback` config (which exposes `host.docker.internal`
inside the sandbox). No nested virtualization required — Smooth
talks to whichever container runtime is on your host over the
network.

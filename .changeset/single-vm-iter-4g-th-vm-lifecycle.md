---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 2 iter-4g. New `th vm` subcommand group
manages the long-lived sandbox container built by iter-4f.

Subcommands:

* `th vm up` — boot the container. Idempotent (no-op if
  already running). Bind-mounts the current working directory
  at `/workspace`, the host stub socket dir at `/run/smooth`,
  and (unless `--no-docker`) the host Docker socket at
  `/var/run/docker.sock`. Docker socket path auto-detected via
  `smooth_host_stub::docker_socket::detect` (iter-4e), so
  Colima / OrbStack / Rancher / Podman just work. Container
  policy is `--restart unless-stopped`.
* `th vm down` — stop the container; volume retained.
* `th vm status` — report container + volume state.
* `th vm prune` — stop + remove container AND volume. Gated
  by `--yes` to prevent surprise nukes.
* `th vm shell` — `docker exec -it <container> bash`. Auto-up
  if the container is missing.

Persistence model: `/workspace` is the user's repo
(bind-mount, lives on host); `/root` is a named volume
(`smooth-vm-root`) carrying mise toolchains, pearl DB, SSH
config, gh/aws/gcloud credentials. State survives `th vm down`
and image rebuilds; `th vm prune` is the only way to wipe it.

Env overrides:

* `SMOOTH_VM_NAME` — container name (default `smooth-vm`).
* `SMOOTH_VM_VOLUME` — volume name (default `smooth-vm-root`).
* `SMOOTH_VM_IMAGE` — image tag (default
  `ghcr.io/smooai/smooth-vm:latest`).
* `SMOOTH_HOST_STUB_SOCKET_DIR` — host stub socket dir
  bind-mount source (default `~/.smooth/host-stub`).

5 new tests cover the env-override precedence (explicit arg >
env > default), empty-env fallback, and the `is_running()`
predicate. Env-touching tests share a `Mutex` so parallel
test runs don't race on `SMOOTH_VM_*` vars.

Phase 2 ships. Phase 4 (cleanup) is the remaining pearl arc.

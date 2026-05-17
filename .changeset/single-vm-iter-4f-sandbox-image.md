---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 2 iter-4f. New
`docker/Dockerfile.smooth-vm` + `scripts/build-smooth-vm-image.sh`
build the long-lived sandbox image `th up` boots
(iter-4g lands the lifecycle command).

Design choices:

* **Base**: `debian:bookworm-slim`. Cloud CLIs (gcloud, az)
  install cleanly without the glibc/musl friction that
  blocks them on the alpine boardroom base. ~80MB before
  layering CLIs.
* **Cloud CLIs bundled via vendor scripts**, each pinned via
  build ARG so rebuilds are reproducible: `gh@2.62.0`,
  `awscli v2` (latest), `gcloud@494.0.0`, `az@2.66.0`,
  `kubectl@1.31.4`, `docker CLI@27.4.1` (CLI only — the
  host's daemon socket is bind-mounted at `/var/run/docker.sock`).
* **mise** (pinned to `2024.12.13`) for language toolchains.
  Seeded `~/.config/mise/config.toml` ships node 22 / python
  3.13 / go 1.23; users `mise install <other>` after the VM
  is up and state persists in `/root` (volume).
* **Smooth binaries** copied from
  `target/aarch64-unknown-linux-musl/release/`: `boardroom`,
  `smooth-operator-runner`, `smooth-dolt`.
* **Long-lived state**: `/workspace` is the bind-mounted
  user repo; `/root` is a named volume carrying mise state,
  pearl DB, SSH config, gh/aws/gcloud credentials. `th down`
  stops the container without touching the volume; `th prune`
  (iter-4g) removes the volume.
* **Env defaults**: `SMOOTH_SINGLE_PROCESS=1`,
  `SMOOTH_BOARDROOM_MODE=1`,
  `SMOOTH_HOST_STUB_SOCKET=/run/smooth/host.sock`. The
  in-process gRPC cast comes up on UDS sockets under
  `$XDG_RUNTIME_DIR/smooth/` by default.

`scripts/build-smooth-vm-image.sh` mirrors the existing
`build-boardroom-image.sh` ergonomics (`--push`, explicit
version arg, `SMOOTH_IMAGE_REPO`/`SMOOTH_IMAGE_TOOL` env
overrides) and cross-compiles the three required binaries
before invoking `docker build`. Default repo:
`ghcr.io/smooai/smooth-vm`.

iter-4g wires the `th up` / `th down` / `th prune` lifecycle
on top of this image.

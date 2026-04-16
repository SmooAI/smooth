---
"@smooai/smooth": patch
---

Fix P0 (th-1ec3ce): sandbox bind-mounts not landing in the guest VM.

The microsandbox guest agent does not `mkdir -p` mount targets before
calling `mount -t virtiofs` — mounts to paths that don't pre-exist in
the rootfs (`/opt/smooth/bin`, `/opt/smooth/policy`, `/workspace`)
silently fail. We were falling back to plain `alpine` because our
custom `smooth-operator` image was only in Docker Desktop's local
store and microsandbox couldn't pull it; alpine has an empty `/opt`,
so every mount missed.

Fix: publish `smooai/smooth-operator` and `smooai/boardroom` images
to GitHub Container Registry (public), and default to pulling from
there. The `Dockerfile.smooth-operator` pre-creates `/workspace`,
`/opt/smooth/bin`, and `/opt/smooth/cache/mise` — so every bind-mount
target now exists before the guest agent tries to mount on top of it.

- `SandboxConfig` default image: `alpine` → `ghcr.io/smooai/smooth-operator:latest`
- `th run` default: `smooai/smooth-operator:latest` → `ghcr.io/smooai/smooth-operator:latest`
- `scripts/build-smooth-operator-image.sh` + `build-boardroom-image.sh`:
  default `SMOOTH_IMAGE_REPO` to `ghcr.io/smooai/...`, add `--push`
  flag so one command builds + publishes.
- Preflight probe now confirms mounts land: `/opt/smooth/bin/smooth-operator-runner`
  is executable inside the VM and the runner boots as expected.

Users can override with `SMOOTH_WORKER_IMAGE` / `SMOOTH_OPERATOR_IMAGE`
if they publish a fork to a different registry. Public pulls from
`ghcr.io/smooai/*` require no auth.

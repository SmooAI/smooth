---
"@smooai/smooth": patch
---

Buildable OCI images for the microVM cast:

- `docker/Dockerfile.smooth-operator` + `scripts/build-smooth-operator-image.sh`
  → `smooai/smooth-operator:<version>` (alpine + mise + runner).
- `docker/Dockerfile.boardroom` + `scripts/build-boardroom-image.sh`
  → `smooai/boardroom:<version>` (alpine + boardroom bin + smooth-dolt).
- Both scripts delegate to the existing cross-compile flow
  (`build-operator-runner.sh` / `build-boardroom.sh`).
- Fixed a latent package-name bug in `build-boardroom.sh`
  (`-p smooth-bigsmooth` → `smooai-smooth-bigsmooth`).

Still pending: registry publish on release so `microsandbox` can
pull without Docker on end-user machines.

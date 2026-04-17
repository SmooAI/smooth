---
"@smooai/smooth": patch
---

Release workflow: build `aarch64-unknown-linux-gnu` on a native ARM
runner instead of cross-compiling from x86_64.

Cross-compilation was failing at `pkg_config failed: pkg-config has
not been configured to support cross-compilation` — libdbus-sys's
build script needs per-architecture pkg-config sysroot + prefix vars,
which are annoying to set correctly and fragile across dep updates.

`ubuntu-24.04-arm` is now a free GitHub-hosted runner, so we switch
the aarch64 Linux matrix entry to it. That makes the build a plain
native build: same `libdbus-1-dev` + `libcap-ng-dev` apt deps, no
multi-arch, no cross linker env, no sysroot juggling.

Also removed the now-unused `gcc-aarch64-linux-gnu` install step and
the `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER` env override.

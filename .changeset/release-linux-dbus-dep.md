---
"@smooai/smooth": patch
---

Release workflow: install `libdbus-1-dev` on Linux runners.

`libdbus-sys` (pulled in transitively via the keyring / zbus chain
that microsandbox depends on) runs `pkg-config` at build time and
fails with "pkg_config failed" if the dev headers are missing. Both
`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` jobs were
failing there.

Also separated the cross-compile toolchain install (aarch64 only)
from the common Linux build deps step.

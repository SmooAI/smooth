---
'@smooai/smooth': patch
---

SMOODEV-4114: smooth-daemon no longer links OpenSSL. Web push moves from `web-push` (isahc/curl + `ece`, both on Homebrew's libssl) to the pure-Rust `web-push-native`, sent over the daemon's rustls `reqwest`, so a local build runs on any Mac and Windows gets push too. Big Smooth now hands its daemon the newest of Homebrew's and the bundled `th` instead of always brew's. New read-only `th doctor install` lists every `th` / `smooth-daemon` with versions, Homebrew dylib linkage, which daemon holds `daemon.lock` / `daemon.addr` / `flow.addr`, stale pid and `.bak` files, and version skew.

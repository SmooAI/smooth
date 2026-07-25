---
"@smooai/smooth": patch
---

fix(release): `sync-versions.mjs` Cargo.lock pass now also skips the external operator crates. #260 fixed the Cargo.toml git-dep stamping, but the Cargo.lock block still bumped `smooai-smooth-operator-server` / `smooai-smooth-operator` (svc) lock entries to the workspace version (0.23.0), while their git source only offers 1.23.1 — so `cargo --locked` failed (`= "*" locked to 0.23.0 … candidate 1.23.1`), still red-lighting the version PR. Skip all three external operator package names (core/server/svc) by exact name in the lock pass. Pearl th-1ee32b.

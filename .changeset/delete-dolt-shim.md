---
'@smooai/smooth': minor
---

Delete the Dolt shim from `th pearls`. `th pearls migrate-from-dolt`, `dolt.rs` / `dolt_server*.rs` / `migrate_dolt.rs`, the `go/smooth-dolt` Go binary, its C launcher, the build/install scripts, and the Go + ICU CI steps are gone (pearl th-c6ba83). Pearls have lived in `~/.smooth/pearls.db` since PR #522; a straggler machine that still has a `.smooth/dolt/` store must migrate with th ≤ 0.42.x before upgrading.

---
"@smooai/smooth": patch
---

fix(release): `sync-versions.mjs` no longer stamps a `version` onto external **git** dependencies. The existing guard skipped only `smooai-smooth-operator-core` (a crates.io dep), but the operator **git** deps `smooth-operator-server` / `smooth-operator-svc` still had `version = "<workspace>"` injected each release. Their crate version at the pinned rev (1.23.1) is unrelated to the workspace version (0.23.0), so cargo failed to resolve (`failed to select a version for smooai-smooth-operator-server = ^0.23.0`), red-lighting the Changesets version PR's Rust checks and blocking every release. Both steps now skip any entry with a `git =` key (plus the existing core name-guard). Pearl th-1ee32b.

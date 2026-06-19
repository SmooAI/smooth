---
"@smooai/smooth": patch
---

build: `sync-versions.mjs` also skips the external `operator-core` in Cargo.lock

Follow-up to the Cargo.toml skip (th-1ee32b): the Cargo.lock updater matched
`name = "smooai-smooth-operator-core"` too and bumped its locked version to the
workspace version (0.14.1), so even with the dependency requirement corrected
to `^0.14.0`, cargo failed with "locked to 0.14.1 … candidate 0.14.0". The lock
updater now skips `smooai-smooth-operator-core`, leaving it pinned to its real
published release. Pearl th-1ee32b.

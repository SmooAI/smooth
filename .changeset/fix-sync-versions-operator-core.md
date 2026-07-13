---
"@smooai/smooth": patch
---

fix(release): `sync-versions.mjs` no longer stamps a `version` onto the external `smooth-operator` git dependency. Step 2 skipped `smooai-smooth-operator-core`, but the step-3 "add missing version" branch didn't — so each release injected `version = "<workspace>"` onto the git dep, which the pinned rev can't satisfy (`failed to select a version for smooai-smooth-operator-core = ^X.Y.Z`), blocking the Changesets version PR's Rust checks. Step 3 now applies the same skip. Pearl th-1ee32b.

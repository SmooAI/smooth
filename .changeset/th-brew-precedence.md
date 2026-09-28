---
'@smooai/smooth': patch
---

Homebrew's `th` is the main one when it's installed, and `cargo-nextest` goes through the check queue (th-35d0d0).

**Big Smooth and a brew `th`.** A `th` that resolves into `Cellar/th/` counts as brew's. When one exists, the desktop app never creates or repoints a `th` link, and it removes its own app-bundle link so PATH falls through to brew's. It does this even when the bundled `th` is newer; in that case it logs a `brew upgrade smooai/tools/th` hint instead of shadowing brew. The daemon it spawns gets `SMOOTH_TH_BIN` set to brew's `th`, so the daemon's own `th` calls use it too. With no brew `th`, the app links its bundled one as before, and only touches links of its own.

**Shims.** Every shim now tries brew's `th` first. `th doctor`, `th ci-queue shim install` and `shim status` warn when `command -v th` isn't brew's while brew's exists, and name what is shadowing it.

**`cargo-nextest`.** A new default shim covers `cargo-nextest nextest run …` called directly. That path used to run rustup's cargo by absolute path, completely outside the queue. The shim takes the `cargo` lock. `nextest list` is heavy, because it compiles every test binary; `--version`, `help`, `show-config` and `self` are light.

**`./gradlew` is not covered.** A repo's wrapper script never reaches a PATH shim, so it still bypasses the queue. The docs now say so, and pearl th-cb3c66 tracks the fix.

---
'@smooai/smooth': patch
---

Bundle the `th` CLI in the Big Smooth desktop DMG so OTA updates carry both the daemon and `th` (pearl th-d07cf0).

The Electron app now stages `th` next to `smooth-daemon` in the app bundle (`stage-daemon`, electron-builder `extraResources`, and the `desktop-publish` CI workflow all build+stage `smooai-smooth-cli` too). On launch the app symlinks the bundled `th` onto PATH (`/usr/local/bin/th`, falling back to `~/.local/bin/th`) via `resolveThBin()` + `linkThOnPath()`. Because the link points into the app bundle, an electron-updater OTA replacement auto-updates the `th` users run — no separate CLI channel.

Coexistence is safe: mirroring `scripts/dev-link-th.sh`, the app only ever creates or repoints a **symlink**. A regular-file `th` installed with Homebrew or the curl installer is left untouched, never clobbered.

---
'@smooai/smooth': patch
---

Big Smooth desktop: native Sparkle-style update dialog

The Electron desktop app now presents a native update dialog modeled on the Sparkle
one the native macOS companion (SmoothFlow) shows. Electron can't use Sparkle, so the
UX is reproduced with Electron's own `dialog.showMessageBox`, driven by
electron-updater's `update-available` event: title "A new version of Big Smooth is
available!", the "X is now available—you have Y" body, a **Skip This Version / Remind
Me Later / Install Update** button row, and an "Automatically download and install
updates in the future" checkbox.

`autoDownload` is now off — nothing downloads until the user picks Install (or has
opted into auto-download via the checkbox). Install starts `downloadUpdate()` and the
existing guarded restart step (stop daemon → `quitAndInstall`) finishes the job; Skip
persists the version to a skip list so it's never offered again; Remind Me Later defers
to the next launch/poll; the checkbox persists an `autoUpdate` preference that silently
downloads future updates. The decision layer stays pure and unit-tested
(`decideAvailableAction`), and everything still flows through the existing attempt-cap /
give-up / once-per-session guards so an un-installable bundle falls back to a manual
download instead of nagging forever.

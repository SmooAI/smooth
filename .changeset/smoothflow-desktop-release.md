---
'@smooai/smooth': minor
---

SmoothFlow 0.2.0: app icon (the `th` mark on three Smoo-gradient streams), menu-bar status item, Sparkle 2 OTA updates (`SUFeedURL` → downloads.smoo.ai/smoothflow/appcast.xml, "Check for Updates…"), and `smoothflow-publish.yml` — the Developer-ID-signed, notarized, appcast-generating publish pipeline that mirrors desktop-publish.yml. `build-release.sh` now bundles `th` next to `smooth-daemon`, re-signs Sparkle.framework inside-out, and writes a versioned `SmoothFlow-<version>-arm64.dmg`. th-b4e4de.

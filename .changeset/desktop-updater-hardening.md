---
'@smooai/smooth': patch
---

Desktop OTA: stop the "update keeps coming back" nag when an update won't install (th-d4feb8).

If a published update can't be installed (Squirrel rejects a bad signature / corrupt zip, as the current 0.1.12 mac zip does), the old updater re-offered it and re-prompted to restart every 30 minutes forever — and `update-downloaded` could fire twice, racing two `quitAndInstall`s. The updater now: fires at most once per version per session (no 30-minute re-nag if you defer), guards against a duplicate install, and — after an update repeatedly fails to stick across restarts — stops nagging and offers a one-time manual download of the DMG instead. The pure decision logic is extracted to `updateDecision.ts` and unit-tested. NOTE: this makes a broken update graceful; the separate fix for _why_ 0.1.12 won't install (signing/publish pipeline) is still needed to actually ship updates.

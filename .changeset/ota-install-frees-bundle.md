---
'@smooai/smooth': patch
---

Desktop OTA installs reliably now (th-79416c). The updater used to hand the bundle to Squirrel while the daemon was still shutting down — `stopDaemon()` was a fire-and-forget SIGTERM — so the copy raced a daemon still holding files inside Big Smooth.app and failed intermittently ("couldn't copy bundle … no such file"), silently rolling back the update. `stopDaemon()` now awaits the process's actual exit (SIGKILL fallback after a grace period), and the update-downloaded handler awaits it before `quitAndInstall`, so the bundle is free when the swap runs. Pairs with th-76a353 (fresh, non-stale daemon) for a self-installing OTA.

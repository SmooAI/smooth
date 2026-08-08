---
'@smooai/smooth': patch
---

Harden desktop daemon discovery: the daemon now advertises its bound `host:port` to `~/.smooth/daemon.addr`, and the Electron app reads it (env `SMOOTH_ADDR` → file → default). Fixes the app loading the wrong page when launched outside launchd on hosts where the default `:8787` is taken (smoo-hub's SmooHub dashboard) — it now follows the daemon to whatever port it actually bound.

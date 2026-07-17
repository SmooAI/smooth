---
"@smooai/smooth": patch
---

Add a launchd installer for the Big Smooth daemon on smoo-hub (`scripts/smoo-hub/install-smooth-daemon.sh` + `com.smooai.smooth-daemon.plist`). Replaces the fragile hand-started `nohup` process with a `KeepAlive` + `RunAtLoad` agent so the daemon survives reboots and auto-respawns on crash. The installer kills any lingering nohup daemon to free the port, then boots out / bootstraps / enables / kickstarts the agent under the user's GUI session. Mirrors the smooai repo's `install-docker-watchdog.sh` sibling pattern.

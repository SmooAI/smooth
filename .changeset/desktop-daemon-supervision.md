---
'@smooai/smooth': patch
---

Big Smooth desktop now supervises its bundled `smooth-daemon` child (th-4b189c). The app once ran for hours with the daemon dead — nothing on the port, no log of the exit — because the exit handler only cleared a variable. An exit (or a child that stops answering `/api/mode`, probed every 30s) is now logged with its code/signal, uptime and the last stderr lines to `~/Library/Logs/Big Smooth/daemon.log` (rotating), respawned with exponential backoff (1s…60s, giving up after 8 consecutive failures), and shown in the tray (`Daemon crashed — restarting…` / `Daemon stopped — click to retry`) plus a new **About Big Smooth…** item with pid, port, uptime and restart count. `smooth-daemon` gained `SMOOTH_LOG_FILE`: when set, tracing goes to that size-rotated file instead of stderr; the desktop app sets it to `~/Library/Logs/Big Smooth/smooth-daemon.log` so the next death is diagnosable.

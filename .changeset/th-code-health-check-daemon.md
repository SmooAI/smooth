---
'@smooai/smooth': patch
---

`th code`'s startup health check asks the daemon it actually talks to (th-8b55de follow-up). Inside a SmoothFlow pane the TUI loaded fine but showed "Big Smooth API not running. Starting..." It probed a hard-coded `localhost:4400`, while its daemon was elsewhere, and it started nothing. The check now uses the same discovery as the rest of th code (`$SMOOTH_URL`, then `~/.smooth/daemon.addr`, then :4400) and names the URL when a daemon really is down. The TUI's other daemon calls (skills, mode, search) use that discovery too. The stale "Database not found" warning about the legacy `~/.smooth/smooth.db`, which nothing reads, is gone.

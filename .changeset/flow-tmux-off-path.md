---
'@smooai/smooth': patch
---

SmoothFlow: find tmux off-PATH, and make a failed launch say why (th-9f6814). A Finder-launched daemon (Big Smooth.app, the SmoothFlow app) has `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, so it never found Homebrew's tmux, and every session it created sat in `starting` with no pid, no detail and no log line. tmux now resolves through `$SMOOTH_TMUX_BIN`, then `PATH`, then `/opt/homebrew/bin`, `/usr/local/bin`, `/opt/local/bin` and `/usr/bin`. Panes get the user's login-shell `PATH`, so `claude`, `node` and `git` are found. A launch that fails (no tmux, a tmux error, a missing harness binary) now sends the session to `dead` with a `launch_failed` reason and a detail, logs it at WARN, and broadcasts it to every client. The daemon logs the tmux it resolved at boot, and `th harness doctor` prints it.

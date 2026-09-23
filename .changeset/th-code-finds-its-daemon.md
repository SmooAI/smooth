---
'@smooai/smooth': patch
---

`th code` finds its daemon instead of assuming port 4400 (th-8b55de). At startup it probed only `localhost:4400`, so a th code session launched by SmoothFlow looped on its boot screen forever. SmoothFlow's daemon listens on a random port, which it passes in `$SMOOTH_URL`, and nothing was on 4400: th code ran `th up`, which said "already running", then waited for :4400, gave up, and was relaunched. Big Smooth, which advertises itself in `~/.smooth/daemon.addr`, was missed from a plain terminal too. Startup now resolves `$SMOOTH_URL`, then `daemon.addr`, then :4400, health-checks the daemon it finds, and hands that address to the TUI. It boots a daemon only when none is advertised. A `$SMOOTH_URL` that doesn't answer is reported as an error instead of booting a different daemon.

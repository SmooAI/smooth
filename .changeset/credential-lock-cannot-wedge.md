---
'@smooai/smooth': patch
---

A stuck process can no longer hang every `th` and daemon on the machine behind the Smoo credentials lock (th-2d2c15). The lock used to wait forever, and the daemons refreshed the session under it with an HTTP client that had no timeout. When SmoothFlow's daemon hung on a dead socket after sleep, `th auth login` sat in `flock()` indefinitely, and so did Big Smooth's heartbeat. Now:

- waits for the lock are bounded (45s), and the timeout error names the holder (program, pid, since when); the holder records itself in the lock file;
- the network call made while holding the lock is time-boxed (30s), and the session HTTP clients have connect and request timeouts, so a hung request releases the lock;
- async callers wait for the lock on the blocking pool instead of stalling a runtime worker;
- `th auth login` and the other interactive commands say who they're waiting on after 2 seconds instead of silently hanging.

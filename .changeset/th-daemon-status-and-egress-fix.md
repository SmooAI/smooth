---
'smooai-smooth-daemon': patch
'smooai-smooth-cli': patch
---

Add `th daemon status` and fix `th daemon` not starting the egress boundary.

- **`th daemon status`** queries the running daemon's `/api/status` and prints
  version, uptime, permission mode, egress state, and active-task count
  (friendly message when the daemon isn't reachable). Pure formatters are
  unit-tested.
- **Bug fix:** the egress-proxy startup lived only in the standalone
  `smooth-daemon` binary's `main`, so launching via **`th daemon`** (the
  primary entry) served without ever starting the proxy — the egress boundary
  was silently inert even with `SMOOTH_EGRESS_ALLOWLIST` set. The startup is
  consolidated into a library `smooth_daemon::serve_persistent`, now used by
  both entries. Verified live: `th daemon status` reports `egress: on (…)`.

---
'@smooai/smooth': patch
---

Single-daemon enforcement (th-c71e6f): smooth-daemon now takes a machine-wide advisory lock on ~/.smooth/daemon.lock at startup and refuses to start when another Big Smooth is already running (with a /health probe of daemon.addr to catch pre-lock builds). Two daemons used to run side by side, sharing operator-storage.db and fighting over daemon.addr — clients could discover the one without macOS TCC grants. `th up` now also probes daemon.addr first and reports "already running at <addr>" instead of burying the child's refusal in the log. Escape hatch: SMOOTH_ALLOW_SECOND_DAEMON=1.

---
'@smooai/smooth': patch
---

Fix a pipe-buffer deadlock in every bounded smooth-dolt subprocess call (`th pearls push`/`pull`/doctor's remote probe): the child was spawned with stdout/stderr piped but nothing drained them until after exit, so any child writing more than the ~64KB pipe buffer blocked on write, looked stalled, and was SIGKILLed at the deadline even mid-healthy-transfer. A shared `wait_child_draining` now drains both pipes on background threads while polling the deadline. Also: `th pearls doctor` no longer reports a timed-out probe clone as "remote unreachable" — a full clone of a large store is legitimately minutes of CPU; the timeout case now says what happened and how to raise the bound (`SMOOTH_DOLT_SYNC_TIMEOUT_SECS`).

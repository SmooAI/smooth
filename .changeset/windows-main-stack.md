---
'@smooai/smooth': patch
---

Fix `th --version` and `th --help` crashing with a stack overflow on Windows. `th` now runs on an 8 MB thread on every OS instead of the 1 MB Windows main thread (th-bd84cf).

---
'@smooai/smooth': patch
---

Fix a leak in the ci-queue stall-watchdog test: its CPU-spinner left the job's process group and survived the test, so every run left a busy `perl` behind. The test now kills it on every exit path and the spinner times itself out.

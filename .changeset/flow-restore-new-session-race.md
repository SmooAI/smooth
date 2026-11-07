---
'@smooai/smooth': patch
---

SmoothFlow no longer kills an agent session opened just after the daemon starts. Restore could mistake it for one lost in a crash and mark it dead with "the daemon stopped before this session launched".

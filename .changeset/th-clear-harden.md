---
'@smooai/smooth': patch
---

`/th-clear` no longer loses the handoff when `/clear` comes late. The arming terminal can claim its token for 24 hours; the old 15-minute TTL reaped it silently. A token the new session can't take now leaves a note naming the pearl. `th harness handoff arm` also copies the `/th-handoff resume <pearl>` line to the clipboard.

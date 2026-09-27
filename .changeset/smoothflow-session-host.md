---
'@smooai/smooth': patch
---

The SmoothFlow engine no longer calls tmux directly (th-64d4ab). Sessions now go through a `SessionHost` trait: launch, liveness, exit status, capture, send, kill and the attach stream. `TmuxHost`, the default, runs the same tmux code as before, so nothing changes at runtime. The seam makes room for a native host on Windows later, and it lets the engine's supervision be tested against an in-memory host, covering cases a real pane only hits by timing.

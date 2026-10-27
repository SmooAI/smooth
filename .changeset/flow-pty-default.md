---
'@smooai/smooth': minor
---

SMOODEV-4238: New SmoothFlow sessions run on the `pty` session host by default (`SMOOTH_FLOW_HOST=tmux` still selects tmux; existing tmux sessions finish on tmux). Quitting SmoothFlow no longer ends `pty` sessions: the next launch re-adopts them. The app saves its tabs and splits and restores them on launch. The engine drops terminal-query answers a client sends to a `pty` session, which stops stray characters appearing in shells, and the Mac app no longer sends them. See ADR-014.

---
'@smooai/smooth': minor
---

SmoothFlow session host (th-e4aef9, ADR-011 phase 3): `smooth-daemon flow-host`, a per-session process that owns the agent's PTY, is its parent (exact exit codes from `waitpid`, no wrapper) and feeds every byte through a headless libghostty-vt. It serves the daemon over a 0600 Unix socket with a versioned, token-authenticated protocol, keeps a per-session `seq`, answers bounded snapshots, survives daemon crashes and is re-adopted from its record. Unix only for now (Windows: th-2b32a6); behind the off-by-default `pty-host` feature until `PtyHost` (th-dc9822) wires it into the engine.

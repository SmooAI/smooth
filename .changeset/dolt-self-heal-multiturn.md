---
"@smooai/smooth": patch
---

Self-healing dolt mid-session — fixes multi-turn-after-sleep wedge

Big Smooth would lock up after macOS overnight sleep: the long-running
`smooth-dolt serve` socket goes silent (child still alive at 0% CPU,
just unresponsive), and any subsequent dolt-touching request blocks
forever. Multi-turn chats died on the second turn.

Fix:
- `SmoothDoltServer` is now respawn-capable. Internal state moved
  behind a `Mutex<ServerHandle>`; `client()` self-heals on connect
  failure (kills + spawns a fresh child, returns the new socket).
- New `is_healthy()` (3 s ping) + `ensure_healthy()` (probe →
  respawn-if-sick → re-ping). Background tokio task in BS startup
  pings every server (project + global) every 30 s and respawns any
  that have wedged.
- `SmoothDoltClient::connect` applies a 15 s read/write timeout so
  a wedged peer surfaces as an `io::Error` instead of blocking.
- `SmoothDolt::{sql,exec,commit}` retries once on transport-looking
  errors (broken pipe, timeout, closed connection, ENOENT on the
  socket) via `ensure_healthy` between attempts. SQL-engine errors
  (locks, syntax) propagate unchanged.
- Hard 5-minute ceiling on `chat_handler` and the session-bound chat
  path so a wedge that slips through still returns an actionable
  error instead of leaving the user watching the spinner forever.
- New `PearlStore::dolt_server()` accessor so the host process can
  register the global store in the healthcheck loop alongside the
  per-project servers.

Tests: `is_transport_err` round-trip (broken-pipe / timeout get
flagged, SQL errors don't).

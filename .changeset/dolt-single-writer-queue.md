---
"@smooai/smooth": patch
---

Single-writer queue in front of smooth-dolt serve

Concurrent dolt callers (chat agent + orchestrator + healthcheck +
session save) could race each other into the Dolt manifest lock,
producing intermittent "database is read only" errors. With this
change every op for a given data dir is serialized through the
server's `serial_lock` mutex — at most one in-flight write at a
time, with the underlying socket timeout (15 s) bounding any
single op.

Combined with the 30 s healthcheck respawn loop, the connect-time
self-heal in `client()`, and the 5-minute chat-turn ceiling, this
closes the last common Dolt-as-daemon failure mode.

- New `SmoothDoltServer::with_client(|c| ...)` is the public entry
  point for serialized ops. `client()` is still exposed for the
  health-check path which deliberately bypasses the lock so it can
  race with in-flight work and detect a wedge.
- `SmoothDolt::{sql, exec, commit, log, push, pull, gc, status}`
  in server mode now route through `with_client`.
- New unit test `with_client_serializes_concurrent_callers` —
  spawns 8 racing threads, asserts the high-water "inside the
  closure" count never exceeds 1.

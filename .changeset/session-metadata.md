---
'@smooai/smooth': patch
---

Maintain session metadata in the daemon's durable store (th-503c80). Appending a
message now increments the owning session's `messageCount` (and refreshes
`lastActivityAt`/`updatedAt`) and writes it through to sqlite, so sessions no
longer report `messageCount: 0` despite having real messages. Empty-session
churn is out of scope here — the engine's `handle_create_session` mints a fresh
session per WS connection when no known `conversationId` is passed, which the
storage adapter can't prevent.

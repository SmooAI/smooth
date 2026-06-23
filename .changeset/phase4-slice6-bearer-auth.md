---
'smooai-smooth-daemon': patch
---

Phase 4 Slice 6 (th-bd0def): bearer-token auth + bind hardening for the
always-on daemon — the gap its own module doc flagged. Auth is opt-in:
with no `SMOOTH_DAEMON_TOKEN` set the daemon serves open (loopback trusts
the local user), so existing frontends are unaffected. When a token is
set, every API + WS route requires `Authorization: Bearer <token>` (or a
`?token=` query param, for browser WebSockets that can't set headers),
checked in constant time; `/health` and the embedded SPA stay open. A
non-loopback bind without a token logs a startup warning.

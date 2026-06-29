---
'smooai-smooth-daemon': patch
---

EPIC th-c89c2a: the daemon converges fully on the operator — no second agent loop.
`th daemon` (default and `operator`) now runs only smooth-operator's local flavor,
made durable by a new sqlite `StorageAdapter` (survives restart, no Postgres) wired
through the operator's `.storage()` seam. Deleted the bespoke `serve_persistent`
path and its 13 modules (server/wire/runner/coordinator/scheduler/permission/
sqlite/approval/event/hook/session/messages) plus the dead `:4400` bind. The daemon
is now just config (egress + LLM creds) + operator + durable storage.

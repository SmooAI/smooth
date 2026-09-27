---
'@smooai/smooth': minor
---

smooth-agent plugin: every Claude Code session now listens for agent mail by default (SMOODEV-3356). SessionStart tells the session to arm the `th msg watch --once` background watcher, which wakes it even while idle; a new `mail-guard.sh` hook surfaces unread mail on each prompt and, before the session goes idle, blocks once to handle unread mail or re-arm a lapsed watcher (loop-guarded, skipped for `claude -p`/SDK sessions, opt out with `SMOOTH_MAIL_WATCH=0`). Also fixes the MCP `agent_identity` rename leaving the session's recorded handle stale, which made `th agent whoami`, bare `th msg`, and the watcher resolve to the old, unregistered handle.

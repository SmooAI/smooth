---
'@smooai/smooth': patch
---

th-mail: read the mailbox you actually registered, and fail loudly when the store is broken.

The session-handle tier of identity resolution never fired: it looked up `$CLAUDE_SESSION_ID`, but Claude Code exports `$CLAUDE_CODE_SESSION_ID`, so every `th msg` / `th agent` command with no `--agent` silently answered for `user@host` while the session's real mail piled up under the handle the SessionStart hook registered. Both vars are now read, and the MCP mail tools share the CLI's resolver instead of a second copy of it (they still refuse rather than falling back to `user@host`).

Registering a second identity for a session that already has one is now refused without `--force` — that split is invisible and reads exactly like "no mail"; `th agent claim` remains the sanctioned path, and now says when it resumed an existing handle rather than carrying your mail across. The SessionStart hook reuses a session's recorded handle on resume instead of re-announcing the placeholder.

Mail-store failures no longer wear the no-mail costume: `th msg watch --once` propagates a failed poll instead of retrying forever, and the `/th-mail` watcher script exits non-zero on a `th` failure instead of printing `[]` and exiting 0 — which is how a 100%-full disk read as an empty inbox.

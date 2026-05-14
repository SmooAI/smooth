---
'@smooai/smooth': patch
---

TUI: inline Claude-Code-style approval cards for Wonk Ask verdicts.
The TUI subscribes to `/api/access/stream` and renders pending
requests as compact cards under the chat scroll. Keystrokes
`o`/`s`/`p`/`u`/`d`/`D` resolve the most recently filed open
prompt at the chosen scope (once/session/project/user/deny-once/
deny-forever) and POST to `/api/access/{approve,deny}`. Reconnects
the SSE stream automatically with exponential backoff so a Big
Smooth restart doesn't strand prompts. Pearl th-670fb2.

Wire types moved to `smooth-narc::access_wire` so the TUI consumes
them without taking a direct dep on smooth-bigsmooth; the orchestrator
crate re-exports the same types so existing call sites compile
unchanged. `AccessStore::subscriber_count()` lets integration tests
wait for the broadcast subscription to register before firing events.

---
'@smooai/smooth': patch
---

wonk/narc: close the loop on auto-mode Phase A. Boardroom Narc now
holds tool calls open when its verdict is `Ask` — files into the
shared `AccessStore`, awaits a human resolution with a 60s timeout,
returns Approve / Deny / EscalateToHuman accordingly. New HTTP routes
make the queue addressable from the TUI / CLI:

- `GET /api/access/pending` — list of pending requests
- `POST /api/access/approve` — resolve at a scope (once / session /
  project / user) with an optional glob override
- `POST /api/access/deny` — same shape as approve
- `GET /api/access/stream` — SSE feed of pending / resolved / expired
  events for inline UIs

Low-confidence LLM approvals now coerce to `Ask` instead of silent
`EscalateToHuman`, so the human gets agency over uncertain calls
instead of just denials. `th access approve/deny <id> [--scope=...]
[--glob=...]` adopts the new id-based shape. Pearl th-49b4aa is now
complete.

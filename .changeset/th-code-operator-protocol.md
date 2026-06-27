---
'smooai-smooth-code': minor
---

EPIC th-c89c2a: `th code` now speaks the **operator's canonical WS protocol**
(`th daemon operator`, :8787) instead of the legacy bespoke `/ws` (:4400). New
`OperatorClient` connects with the local token, opens a conversation session
(reused across turns so multi-turn history stays server-side), sends
`send_message`, and maps the operator's `stream_token` / `stream_chunk` (tool
calls) / `eventual_response` / `error` back to the same `ServerEvent`s the TUI +
headless already render — so the rendering loops are unchanged. `app.rs` +
`headless.rs` swapped over; `ensure_server` now starts `th daemon operator`.
Validated live: `th code --headless` ran a real LLM turn that called the
kernel-sandboxed `bash` tool and round-tripped its output. The bespoke
`BigSmoothClient` + the SSE fallback remain only for the bench-test capture path
(follow-up to remove).

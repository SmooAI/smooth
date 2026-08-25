---
'@smooai/smooth': patch
---

CLI: send `agentId` on the operator's `create_conversation_session` — `th api smooth-operator chat` and MCP `ask_business` were failing on EVERY org

Both operator entry points died with `VALIDATION_ERROR: missing 'agentId'` before the turn ever started. Not an org-config gap and not the master org: reproduced on four unrelated orgs, all identical.

`agentId` is required by the SEP Request schema, but `smooth-operator-server` used to fabricate a UUID for an absent one. th-68897a moved that check to the boundary where it belongs (`handle_create_session` now rejects absent-or-blank instead of silently minting an id or storing NULL) — correct server-side, but this hand-rolled WS client was one of the callers relying on the old fabrication, so it started failing the moment the new server rolled out. The dashboard kept working because it always sent one.

The frame now carries a fresh uuid, exactly what the working dashboard client sends (`agentId: agentSlug ?? crypto.randomUUID()`). It is a correlation id, not an `agents.id`: copilot-ws — the pod behind `smooth-operator.smoo.ai` — runs its storage adapter with `with_builtin_session_agent()`, which ignores the caller's agent id and binds the session to the org's built-in "Smooth Operator" row. So this picks no agent and changes no behaviour; it makes the CLI byte-identical to the UI path that already works.

Frame construction moved into a pure `create_frame` so the regression is testable: `agentId` present and non-blank, parses as a uuid, fresh per session, and still sent on the resume path alongside `conversationId`.

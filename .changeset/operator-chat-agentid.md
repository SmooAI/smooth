---
'@smooai/smooth': patch
---

Operator chat works again: `smoo api smooth-operator chat` and the MCP `ask_business` tool send `agentId` on `create_conversation_session`. Both failed on every org with `VALIDATION_ERROR: missing 'agentId'` before the turn started.

`agentId` is required by the SEP Request schema. `smooth-operator-server` used to make one up when it was missing, and th-68897a moved that check to the boundary, so `handle_create_session` now rejects an absent or blank id. This hand-rolled WS client relied on the old behaviour; the dashboard never did, which is why the UI kept working.

The frame now carries a fresh uuid, the same thing the dashboard sends (`agentId: agentSlug ?? crypto.randomUUID()`). It is a correlation id, not an `agents.id`: copilot-ws, the pod behind `smooth-operator.smoo.ai`, builds its storage with `with_builtin_session_agent()`, which binds every session to the org's built-in "Smooth Operator" row whatever uuid arrives. The token endpoint returns no agent id, and none is needed. Frame construction moved into a pure `create_frame` so the regression is covered by unit tests.

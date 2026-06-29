---
'smooai-smooth-web': patch
---

EPIC th-c89c2a (th-f1a1f0): reimagine smooth-web as the operator's **Presence**
control surface. A thin client on the canonical WS protocol (`operator.ts`
`useOperator` hook — same stream_token/stream_chunk/write_confirmation_required
events as `th code`): one session, streaming conversation with inline tool calls,
and the HITL approval inbox as the hero. The Three.js Big Smooth face is now
reactive across the agent's live presence (awake → thinking → speaking →
amber-alert "needs you"). Warm-ink theme. Replaces the orphaned `/api/*`-bound SPA
(deleted control/daemon/api/layout/pages). Build green (1856 modules).

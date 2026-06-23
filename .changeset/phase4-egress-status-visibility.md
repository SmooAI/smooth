---
'smooai-smooth-daemon': patch
'smooai-smooth-web': patch
---

Phase 4 (EPIC th-c89c2a): surface the egress boundary's status to operators.
`GET /api/status` now includes `egress_proxy` — the proxy address when the
egress boundary is on, else `null` — and the control surface shows an
`egress on/off` chip in the header (tooltip names the proxy). Also hardens
`resolve_egress` into a pure, env-free `resolve_egress_inner` so its
parse/expand tests don't race on `SMOOTH_EGRESS_ALLOWLIST` (matching the
existing `resolve_llm_inner` pattern).

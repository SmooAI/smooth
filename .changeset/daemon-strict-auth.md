---
'smooai-smooth-daemon': minor
---

EPIC th-c89c2a: the local flavor now enables the operator's **strict-auth** mode
(`LocalServerBuilder::strict_auth(true)` in `serve_local_flavor`), so a `/ws`
connection with a missing/invalid token is **rejected (HTTP 401)** instead of
degrading to an anonymous connection. This closes the gap the live e2e surfaced
(th-6d1863): `LocalTokenVerifier` now genuinely gates connections — a stray local
process or tailnet peer can't drive the agent. The widget + SDK clients carry the
token, so they're unaffected. The e2e test now asserts tokenless `/ws` is rejected
and a valid token is accepted (and the live LLM turn still runs).

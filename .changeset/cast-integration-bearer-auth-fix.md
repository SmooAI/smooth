---
"@smooai/smooth": patch
---

Fix 5 cast_integration tests that had been failing in CI since the
Wonk bearer-token auth was added in `f7676d8`. The release workflow
has been red for ~8 days, stranding 12 changesets and blocking every
version bump.

Root cause: `ALLOW_EXAMPLE_POLICY` has `[auth] token = "test-token"`,
so Wonk's `require_operator_token` middleware rejects any request
without `Authorization: Bearer test-token` with a 401 (empty body).
The tests built `reqwest::Client::new()` directly and called
`.post(...).json(...).send().await.unwrap().json().await.unwrap()`,
which panicked at the final `.json()` with
`reqwest::Error { kind: Decode, source: Error("EOF while parsing a value") }`.

Fix: introduce `TEST_AUTH_TOKEN = "test-token"` next to the policy
fixture, attach `.bearer_auth(TEST_AUTH_TOKEN)` to every direct Wonk
request, and switch `spawn_goalie` to `WonkClient::with_auth` so its
`/check/*` calls carry the header too. The `goalie_forwards_..._for_allowed_request`
test had surfaced as a `403 != 200` assertion for the same reason —
Goalie was failing its auth to Wonk and correctly denying the request.

Narc / Scribe / Archivist tests were never affected (those services
do not require auth).

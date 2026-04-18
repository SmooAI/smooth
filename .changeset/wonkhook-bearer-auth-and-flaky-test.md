---
"@smooai/smooth": patch
---

Fix **th-dfd0d3**: every sandboxed tool call was being rejected with
"error decoding response body" because `WonkHook` inside the operator
runner never carried the per-VM bearer token. The same security
hardening commit (`f7676d8`) that added `Authorization: Bearer` auth
to Wonk's `/check/*` endpoints updated `WonkClient` (used by Goalie)
but left `WonkHook` (used by the agent's tool registry) untouched.
Every `pre_call` → `/check/tool` now gets a 401 with an empty body,
and `resp.json::<CheckResponse>()` surfaces as the opaque
"error decoding response body" at the hook layer.

Changes:

- `WonkHook::with_auth(url, token)` constructor; `new` remains as
  a zero-token shim for legacy tests.
- Per-request `Authorization: Bearer <token>` when the token is
  non-empty.
- `check()` now inspects HTTP status before attempting to decode as
  JSON — on a non-success response we surface
  `"Wonk /check/... returned 401: <body>"` instead of the misleading
  decode error. Future misconfigurations will be obvious.
- `smooth-operator-runner` stores the operator token on `Cast` and
  wires `WonkHook::with_auth(&cast.wonk_url, &cast.operator_token)`
  into the tool registry.
- Regression tests on `WonkHook` pre-call:
  `pre_call_without_token_surfaces_401_not_decode_error` (negative)
  and `pre_call_with_auth_passes_through` (positive).

Also fixed a CI-flaky test on the side: the two
`smooai_gateway_*` provider tests both mutate the global
`SMOOAI_GATEWAY_URL` env var and ran in parallel, racing each other.
Added a module-local `Mutex` so they serialize.

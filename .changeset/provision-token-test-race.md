---
'@smooai/smooth': patch
---

Fix the flaky `provision_generates_and_persists_when_unset` test that reddened every Release CI run since 2026-07-22 (th-d9dbd7). It and `provision_prefers_env_token` both mutate the process-global `SMOOTH_LOCAL_TOKEN`, and cargo runs tests in parallel threads of one process — so the env-token test's `set_var` could leak into the generate-and-persist test, which then took the env branch and never wrote `~/.smooth/operator-token`, failing its `.exists()` assertion. The release job's scheduling exposed the race that PR Checks got lucky on. Both tests now serialize on a `TOKEN_ENV_LOCK`, mirroring the existing `GATEWAY_ENV_LOCK` pattern.

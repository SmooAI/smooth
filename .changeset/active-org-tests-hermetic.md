---
"@smooai/smooth": patch
---

test: make `auth::active_org` tests hermetic so they stop flaking the Release run

`auth::active_org`'s tests pointed `default_user()` at a tempfile by mutating
the process-global `SMOOAI_USER_AUTH_FILE` env var under a module-local
`ENV_LOCK`. The cross-store `active_org` module's tests mutate the *same* env
vars under a *separate* lock, so the two modules raced when cargo ran them in
parallel in the `th` test binary: one clobbered the other's env mid-test, the
read hit the wrong file, the assert failed, and the failure poisoned the mutex
— cascading. It passed in PR Checks but lost the race in the Release
(Changesets) workflow, keeping Release red (and blocking changeset versioning).

Fix: drop the env entirely. `set`/`resolve` now delegate to private
`set_in(&store, …)` / `resolve_in(&store)`, and the tests construct
`CredentialsStore::at(<tempfile>)` directly — no global env, no lock, no
cross-module race. Verified passing under `--test-threads=16`. Pearl th-2944e5.

---
"smooai-smooth": patch
---

th: unify active-org resolution across `th api`, `th config`, `th auth`

`th api orgs switch <id>` wrote the active org only to the legacy
`smooth-api-client` store at `~/.smooth/auth/smooai.json`, but
`th config list` (and any other subcommand that uses
`smooai-client-shared`'s `default_user()` store) read from a different
file (`~/.smooth/auth/smooai-user.json`). Net effect: switch reported
success, then `th config list` immediately failed with
"no active org set — pass `--org-id <id>`, set SMOOAI_ORG_ID, or run
`th api orgs switch <id>`" — the same command the user just ran.

Adds a shared `crate::active_org` module with two functions:

- `resolve(override_org)` — consults `--org` flag → `$SMOOAI_ORG_ID` →
  every credential store on disk (legacy api-client + client-shared
  M2M + client-shared User), returning the first non-empty
  `active_org_id`.
- `set(org_id)` — fans the write out to every credential store whose
  file already exists. Won't fabricate a stub User session for an
  M2M-only user.

Wires `th api orgs switch`, `th api orgs show`, the `th api`
`require_active_org` helper, and `th config`'s `resolve_org` through
the shared module. Covered by ten new cross-subcommand contract
tests in `crates/smooth-cli/src/active_org.rs`.

---
'@smooai/smooth': patch
---

Org DX bundle: add a top-level `th org` alias for `th api orgs` (list/switch/show) for discoverability; `th auth whoami` now prints a switch hint; `--org` and `--org-id` are interchangeable on both `th config` and `th admin config` (each accepts the other as an alias); and `docs/Engineering/Using-th-CLI.md` documents the key gotcha — the **user JWT** acts cross-org via `--org-id` (master admin over child orgs) while **M2M** tokens are org-locked server-side, so `th org switch` is cosmetic for the `--m2m`/`th admin config` surface and child-org config-env bootstrap must use the deploy path (`prepareSmooConfig`), not an admin env-create. (pearl th-c153ec; closes the active-org switch-contract work tracked in th-3217db, which round-trips cleanly across all 3 credential stores.)

---
"@smooai/smooth": patch
---

Fix release automation: `sync-versions.mjs` no longer injects the workspace
version onto the external `smooai-smooth-operator-core` git dependency. Step 2
already skipped it (pearl th-1ee32b), but step 3 — which adds a `version =` to
any `smooth-X` dep missing one — did not, re-introducing the exact broken pin
(`operator-core = "^0.19.0"` against a rev that is only 0.15.0) and failing
`cargo` resolution in the version PR. This had blocked every `th` release past
0.18.0.

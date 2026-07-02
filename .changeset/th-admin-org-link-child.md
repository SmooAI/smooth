---
'@smooai/smooth': patch
---

Add `th admin org link-child <child-org-id> [--parent <org>] [--type manages]`, `unlink-child`, and `children` — manage the client-portal parent/child org relationships from the CLI. `link-child` POSTs `/organizations/{parent}/relationships` (parent defaults to the active org, type defaults to the platform's `manages` convention); `unlink-child` resolves the matching relationship id and deletes it; `children` lists a parent's child orgs. These are the user-JWT relationship endpoints (a parent-org admin's session), not `/admin/*`. Previously this required a hand-rolled curl with a bearer token.

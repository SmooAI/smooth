---
'@smooai/smooth': patch
---

Add `th config environments` (alias `th config env`) — manage an org's config environments (list / create / update / delete / values) over the **user-JWT** `th config` surface, not the internal `th admin`. Creating an environment is how a new org's config is activated. Because it authenticates with your user session, the SMOODEV-695 path-org guard authorizes it — so a **parent-org admin can create/manage a child org's environments** with `--org-id <child>` (or after `th org switch <child>`), with no `th admin`. Pairs with the smooai backend change that lets master-org admins act on active child orgs.

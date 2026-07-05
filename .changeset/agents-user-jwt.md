---
"@smooai/smooth": patch
---

`th api agents` now authenticates as the logged-in **user** (Supabase JWT), the same way `th api crm` does, instead of the org-locked M2M `client_credentials` token. M2M tokens are strictly bound to one org, so `th api agents show`/`update` 403'd on **child** orgs; a parent-org admin's user session carries cross-org access via membership, so every agent verb (list/show/update/delete/summary/regenerate/knowledge/mint/create/generate-config) now works on the orgs the user belongs to. Paired with the monorepo PR that org-locks the native agents LIST handler against M2M (SMOODEV-1863).

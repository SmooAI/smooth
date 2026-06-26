---
'@smooai/smooth': patch
---

Add `th llm` — a top-level surface for an org's `llm.smoo.ai` gateway keys, wrapping the shipped `api.smoo.ai/organizations/{org_id}/llm-gateway/*` API: `overview`, `usage`, `create-key`, `rotate-key`, and `keys` (list/create/rotate/delete). Mints the org's persistent LiteLLM virtual key (scoped to the org's team/budget) and prints the value once. Authenticates as the user (Supabase JWT) and is org-admin-gated, so a master admin can mint for a child org with `--org-id <child>`. Adds a `delete` method to the user-JWT `UserClient`. This is the static-key model the backend actually ships — it re-scopes pearl th-f7b20f (whose ephemeral JWT→session design has no backend endpoint). (pearl th-f5781f)

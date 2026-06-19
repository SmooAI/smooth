---
"@smooai/smooth": minor
---

`th admin` + `th auth` (pearl th-abc4e2). New `th auth` for user identity (Supabase OAuth browser flow, M2M client_credentials, `whoami`, `logout`) stored at `~/.smooth/auth/smooai.json` — separate from the existing provider-credential management which is renamed to `th model` (Anthropic, Smoo AI Gateway, OpenRouter, OpenAI). New `th admin` for superadmin operations against `api.smoo.ai/admin/*` — currently 14 verbs across `user` (list / search / roles / magic-link) and `org` (list / show / create / members / products). All admin commands require a `th auth login` session whose account has `requireSuperAdmin` (403 otherwise). Pretty table rendering via `tabled` (heavy styling, opt-out via `--json`). Foundation for pearl th-feebd2 (`th admin onboard-customer`).

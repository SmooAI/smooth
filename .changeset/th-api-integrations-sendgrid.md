---
'smooai-smooth-cli': minor
---

Add `th api integrations sendgrid` — get / create / delete / test for the per-org
SendGrid email integration (backed by the monorepo's
`/organizations/{org}/integrations/sendgrid` routes). This is the CLI surface for
provisioning outbound-email delivery, e.g. the OTP identity-verification email
path for `end_user`-gated agent tools.

The API key is never a flag: `create` reads it from `SENDGRID_API_KEY` or an
interactive no-echo prompt, so it can flow env→env (`SENDGRID_API_KEY=$(…) th …`)
without ever landing in shell history or the transcript. Reads return `hasApiKey`
(the server stores the key write-only).

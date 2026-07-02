---
'smooai-smooth-cli': patch
---

Add `th api integrations sendgrid get|create|delete|test`.

Wraps the org SendGrid integration CRUD at
`/organizations/{org_id}/integrations/sendgrid` (+`/test`). `create` takes
`--from-email`/`--inbound-email`/`--from-name` and reads the API key from
`SENDGRID_API_KEY` or a masked prompt — never from argv. Unblocks provisioning
a fresh test org's OTP email delivery without the dashboard or raw SQL.

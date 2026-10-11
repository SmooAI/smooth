---
'@smooai/smooth': minor
---

`th smoo org-notifications` manages the org's notification email recipients: shared inboxes and members that get instant emails and daily or weekly digests of bookings, deals, leads, escalations, missed calls and conversations. The verbs are `list`, `add`, `update`, `remove`, `settings`, `test` and `resend`. Every change confirms first and needs `org.notifications.manage`. It is the CLI twin of the copilot's `org_notification_emails.*` tools and the hosted MCP `org_notification_emails_*` tools (SMOODEV-3828).

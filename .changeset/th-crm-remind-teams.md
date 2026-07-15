---
"@smooai/smooth": minor
---

`th api crm remind` / `th api crm reminders` + `th api teams` — CLI follow-up of
the CRM overhaul epic (SMOODEV-2646 reminders, SMOODEV-2645 teams).

- **Reminders**: `th api crm remind <TYPE:REF> --at <when> [--note] [--assignee]`
  sets a reminder on any CRM entity (`contact:jane@acme.com`, `deal:"Acme
  renewal"`, or `<type>:<uuid>` for task/proposal/funnel/custom_object). `--at`
  parses `tomorrow` / `"next week"` / `"in 3 days"` / `2h` / `2026-08-01` /
  full RFC3339 (bare dates land at 09:00 UTC). `th api crm reminders list
  --mine | --entity <TYPE:REF>` lists them; `th api crm remind cancel <id>`
  (also `reminders cancel <id>`) soft-cancels. Assignee defaults to you.
- **Teams**: `th api teams list | create <name> | rename <team> <new> | delete
  <team> | set-members <team> <email|id>… | set-roles <team> <role|id>…`.
  Teams resolve by name or id, members by email, roles by name; `set-members` /
  `set-roles` are replace-all.

Both authenticate as the logged-in user (`th auth login`), reusing the CRM
entity resolvers and the `UserClient` (now with a `put` method).

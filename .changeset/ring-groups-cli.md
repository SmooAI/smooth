---
'@smooai/smooth': minor
---

`th smoo calling ring-groups list|show|set|members` (alias `departments`) lets an admin read and change ring group settings: strategy, ring-first seconds, message-only, notify email, enabled, and the ordered member list with ring-first and off flags. It calls the same api-prime routes as the Calling settings page, so a non-admin gets the API's 403, reported as "admin only".

---
'@smooai/smooth': minor
---

SMOODEV-4052: `th smoo calling` can now stand up a phone line end to end. `ring-groups create|delete`; `hours show|create|delete` and `hours holidays get|set` (with `--us-holidays`, the dashboard's 14-entry US preset, which skips holidays already on the schedule); `seats list|assign|unassign`. Every write confirms on a TTY or needs `--yes`, and a 403 reads as "admin only".

---
'@smooai/smooth': minor
---

Big Smooth can now organize Reminders (SMOODEV-3734). The `reminders` tool gained `lists`, `create_list` (on the default or a named sibling list's account, refusing duplicates), `move` (re-parents a reminder with every field intact) and `update` (title, notes, due date, priority). A new `reminders_delete` tool deletes one reminder, but only after the user confirms. An unknown list name now errors with the real list names on every verb. The persona now tells Big Smooth to check it can deliver an option before offering it, to try `bash`/`osascript`/`th` before saying it can't do something, and to offer to file a pearl when one of its own tools falls short. Pearls about Big Smooth itself go to the smooth repo (`"about_smooth": true`, new `smooth.repo` setting), and every pearls result names the project it landed in.

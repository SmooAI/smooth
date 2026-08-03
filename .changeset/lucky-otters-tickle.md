---
"@smooai/smooth": patch
---

Big Smooth can read and adjust your macOS Reminders (pearl th-94cc4a)

Adds a `reminders` tool alongside `calendar`: `list` (open or all, optionally
filtered to one list), `add` (title, absolute due date, target list) and
`complete` (by id). Unlike `calendar` there is no CLI to install — reminders go
through **EventKit in-process**, via the `smooth-menubar` objc2 quarantine crate,
so no subprocess exists and there is no shell or argv to inject into. There is
deliberately no delete verb.

`th doctor --setup-reminders` drives the one-time macOS Reminders grant (a
separate TCC grant from Calendar). Until it's granted the tool still registers
and answers with that command, rather than claiming you have no todos.

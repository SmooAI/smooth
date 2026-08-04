---
'@smooai/smooth': patch
---

`th doctor` now raises Big Smooth's macOS access grants, and gains `--onboard` (pearl th-ba764e).

A bare `th doctor` could report "all checks passed" on a Mac where the calendar, reminders and messages tools were all dead — those grants were only visible behind the `--setup-*` flags. Doctor now prints a `macOS access` section (app bundle, `ical` CLI, Calendar/Reminders EventKit status, and a real `chat.db` readability probe for Full Disk Access), each with the command that fixes it, plus a Smoo AI sign-in check. Because TCC grants are per-binary and belong to `Big Smooth.app`, every line is worded as a proxy for the daemon's grant rather than proof of it.

`th doctor --onboard` runs the health check and then walks every not-ready step in dependency order — providers, Smoo AI sign-in, Full Disk Access, Calendar, Reminders, Messages — driving each one's existing `--setup-*` path. Ready steps are skipped, and a failing step is reported without stranding the rest.

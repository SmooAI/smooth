---
'@smooai/smooth': patch
---

Add a **Set Up** submenu to the Big Smooth menu-bar app that drives the macOS access grants from inside the app (pearl th-ba764e).

`th doctor` can open the same panes, but TCC attributes a grant to the process that asks — so asking from `th` grants `th`, not the daemon that actually reads `chat.db` and calls EventKit. These items run in-process inside `Big Smooth.app`, which is the identity the grants have to land on.

Four items: **Configure Full Disk Access…** (opens the Privacy & Security → Full Disk Access pane), **Grant Calendar Access…** and **Grant Reminders Access…** (fire the native EventKit prompts, off the main thread so the AppKit run loop doesn't deadlock the prompt it's waiting on, then report the result), and **Set Up Messages…** (opens the FDA pane for the `chat.db` read and fires the harmless `get name` Apple Event that makes the Messages Automation prompt appear).

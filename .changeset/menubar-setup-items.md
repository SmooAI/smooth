---
'@smooai/smooth': patch
---

Add a **Set Up** submenu to the Big Smooth menu-bar app that drives the macOS access grants from inside the app (pearl th-ba764e).

`th doctor` can open the same panes, but TCC attributes a grant to the process that asks — so asking from `th` grants `th`, not the daemon that actually reads `chat.db` and calls EventKit. These items run in-process inside `Big Smooth.app`, which is the identity the grants have to land on.

The same grants are now also **auto-initiated** the moment a tool needs one it doesn't have: `reminders` and `calendar` fire the EventKit prompt, an FDA-denied `chat.db` read opens the Full Disk Access pane, and an Automation-denied Messages send re-fires the prompt-triggering Apple Event. The tool answers "I just opened the … — click Allow, then ask me again" instead of "go run `th doctor`". At most once per grant per daemon session, and only when running as the app — a headless daemon can't show a prompt, so it keeps the old actionable text.

Four menu items: **Configure Full Disk Access…** (opens the Privacy & Security → Full Disk Access pane), **Grant Calendar Access…** and **Grant Reminders Access…** (fire the native EventKit prompts, off the main thread so the AppKit run loop doesn't deadlock the prompt it's waiting on, then report the result), and **Set Up Messages…** (opens the FDA pane for the `chat.db` read and fires the harmless `get name` Apple Event that makes the Messages Automation prompt appear).

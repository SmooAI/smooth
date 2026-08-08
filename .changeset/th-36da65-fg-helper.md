---
'@smooai/smooth': patch
---

Desktop 0.1.3: the calendar/reminders TCC helper is now a foreground app (removed `LSUIElement`) so macOS actually presents the EventKit permission prompt. A background/agent helper is silently refused (returns not-determined, no prompt) — verified on macOS 26.4 (th-36da65). Set Up → Calendar…/Reminders… now works.

---
'@smooai/smooth': patch
---

Desktop 0.1.5: add `com.apple.security.personal-information.calendars` + `.reminders` entitlements (inherited by the BigSmoothTCC helper). Without them a hardened-runtime process's `requestFullAccessToEvents` silently returns not-determined with no prompt — this is the final piece that makes Set Up → Calendar…/Reminders… actually show the macOS permission prompt (verified on macOS 26.4, th-36da65).

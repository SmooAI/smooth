---
"@smooai/smooth": patch
---

Give Big Smooth a clock: new `current_datetime` tool (th-4c6271)

The daemon had no way to ask what time it was, so the model invented "today" from its training data (observed "Fri May 22" and "December 19, 2024"). `current_datetime` returns the local time, weekday, IANA timezone, ISO-8601 timestamp, UTC, and unix epoch. It takes no arguments, is registered in the default tool set on every platform, and uses `chrono` + `iana-time-zone` so it works identically on macOS, Linux, and Windows.

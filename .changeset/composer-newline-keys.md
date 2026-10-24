---
'@smooai/smooth': patch
---

Shift+Enter and Alt/Option+Enter start a new line in both the Big Smooth chat and `th code` (SMOODEV-4147). The web composer used to send on Alt+Enter. `th code` had no newline key at all: it now turns on the kitty keyboard protocol where the terminal supports it, so Shift+Enter is distinguishable from Enter, and it also accepts Alt+Enter and Ctrl+J everywhere.

---
'@smooai/smooth': patch
---

SmoothFlow crash cards name the signal and use this OS's numbering (th-7be58a). tmux 3.5 reports a pane's killing signal by name (`bus`, `usr1`), and the engine turned names into numbers with a Linux table, so on macOS a SIGBUS read "killed by signal 7" (macOS's SIGBUS is 10). The table is now per-platform, and the card says "killed by SIGBUS (signal 10)".

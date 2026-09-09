---
'@smooai/smooth': patch
---

`th down` no longer orphans the daemon. The `th up --foreground` wrapper now execs `smooth-daemon` (the recorded pid *is* the daemon), and `th down` signals the pid file's whole process tree, waits, and fails loudly if anything survives — instead of printing "stopped" while the child kept the port bound and `daemon.lock` held (th-eed3de).

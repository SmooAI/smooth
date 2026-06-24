---
'smooai-smooth-cli': patch
---

`th daemon` subcommands (including `th daemon operator`) now log to **stderr**
instead of the TUI's `~/.smooth/log/th.log`. Daemon subcommands are
long-running services, so their tracing output should be visible in the
foreground and captured by the service supervisor (launchd/systemd via
`th service`) — not buried in the file the TUI shares. (EPIC th-c89c2a; closes
th-9eb87d — the operator's info logs were never silent, just file-routed.)

---
'smooai-smooth-daemon': patch
---

EPIC th-c89c2a (th-515a13): `smooth-daemon permissions check "<cmd>"` / `--write
<path>` / `permissions path` — inspect what verdict (DENY/ASK/ALLOW) the Gate-1
rules give a command or write, so users can author + verify `~/.smooth/
permissions.toml` before relying on it. Reachable as `th daemon permissions …`.

---
'@smooai/smooth': patch
---

wonk: persistent permission grants via `wonk-allow.toml`. Approvals
at scope `user` (and for now, `project`) survive a Big Smooth
restart — the resolution is appended to `~/.smooth/wonk-allow.toml`
and Boardroom Narc consults the file at startup so subsequent
requests for the same resource short-circuit to Approve without
re-asking the human.

Schema (v1): `[network] allow_hosts`, `[tools] allow`, `[bash]
allow_patterns`. Host patterns support `*.example.com` and
`.example.com` glob shapes; bare suffixes require exact match (so
`evil-example.com` can't slip past an `example.com` allow entry).
Atomic writes via tempfile + rename. Pearl th-38b72c.

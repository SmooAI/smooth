---
'@smooai/smooth': patch
---

Big Smooth: stop auto-mode from prompting for the clock. The engine's permission classifier grades a tool by name (`get_*`/`read_*`/`list_*` = read-only-safe → auto-allowed), and the bare name `current_datetime` fell into the `Unknown` bucket, so `AcceptEdits` asked the user before every clock read. Renamed the tool `current_datetime` → `get_current_datetime` so it is auto-allowed, with a regression test asserting the engine actually allows it. (The broader class fix — engine metadata-based tool classification and a wired "always allow" grant — is tracked in th-4c71a6 / th-cc0894.)

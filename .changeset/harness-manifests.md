---
'@smooai/smooth': minor
---

Harness manifests (th-0f6126, epic th-faa590): any coding agent CLI can be a
SmoothFlow session kind. One TOML per harness — binary names + real-install
paths + shim skip list, launch/resume argv templates with `{prompt}`
`{session_id}` `{cwd}` `{model}` `{daemon_url}` placeholders, state source
(`hooks` with an event map | `scrape` regexes | `native`), steer/kill/install
paths — loaded built-in < `~/.smooth/harnesses/` < project `.smooth/harnesses/`
< `th pkg` packages. The engine's launch table, resolver and scraper now read
manifests; `claude`, `opencode`, `codex` are the built-ins byte-for-byte, and
`th code` joins as a fourth with NATIVE state (the engine exports
`SMOOTH_FLOW_SESSION`/`SMOOTH_URL` into the pane and `th code` reports its own
turn_start/turn_end — no plugin, no scraping). Sort/hide prefs persist in
flow.db (`GET /api/flow/harnesses`, `PUT /api/flow/harnesses/prefs`);
`flow.hello` carries the visible `harnesses` list and `flow.harnesses` follows
changes; `th harness list|show|add|hide|unhide|order` manage them; the macOS
New-session and fan-out pickers render the engine's list (uninstalled shown
disabled with the reason) and Settings ▸ Harnesses reorders/hides it.

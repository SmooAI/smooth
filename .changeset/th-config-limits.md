---
"@smooai/smooth": minor
---

`th config limits` — CLI surface for the new `@smooai/config` **limits** kind
(SMOODEV-2306), numeric segment-resolved config that never bakes and clamps
client-side. `limits evaluate <key>` POSTs to
`/organizations/{org}/config/limits/{key}/evaluate` (same segment machinery as
`th config feature-flag`, but the resolved value is a number; prints the raw
pre-clamp value, `--json` for the full envelope), `limits list` / `limits get`
surface the clamp metadata (`default`/`min`/`max`/`step`) declared in the org's
schema, and `limits set <key> <value>` writes a raw numeric value (thin wrapper
over `th config set … --tier limit`, rejecting non-finite values at parse
time). Adds a `limit` value tier (`--tier limit`). Aliased `th config limit`.
Runtime resolution depends on the config-server route being wired separately.

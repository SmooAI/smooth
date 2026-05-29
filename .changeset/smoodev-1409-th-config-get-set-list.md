---
'@smooai/smooth': minor
---

SMOODEV-1409: Add top-level `th config` command with `get`, `set`,
and `list` subcommands for day-to-day `@smooai/config` value
management. Auths via the user JWT at
`~/.smooth/auth/smooai-user.json` by default (with auto-refresh via
the stored Supabase refresh_token); pass `--m2m` to use the
service-account session at `~/.smooth/auth/smooai.json` instead.

```
th config get apiUrl --environment=production
th config set apiUrl https://api.smoo.ai --environment=production
th config list --environment=production --json
```

Org id resolves from `--org-id` flag → `SMOOAI_ORG_ID` env →
`active_org_id` in the credentials file. The full schemas +
environments surface still lives under `th api config` — this
top-level command is just the muscle-memory "read or write a single
value" wrapper that mirrors the `smooai-config` CLI's `get` / `set`
/ `list` ergonomics.

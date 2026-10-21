---
'@smooai/smooth': minor
---

`smoo admin org overrides set` now changes only the flags you pass: it reads the stored override to preview before/after, then PATCHes just those fields (PUT only creates), so `set <org> telephony --seats 10` grants calling seats without wiping the expiry, reason or calling allowance. New flags: `--seats`, `--clear-seats`, `--clear-voice-allowance` (telephony only) and `--permanent`; `--enabled`/`--disabled` and `--reason` are only required when creating an override, and a set that changes nothing sends no write. New `smoo admin config flag-org add|remove <flagKey> <orgId> --environment <env>` adds or removes one org in a feature flag's org allowlist rule in place (creating the rule if missing, leaving other rules untouched), prints before/after, confirms, and re-reads before writing (a best-effort check against a concurrent edit). The key is matched canonically and written under its stored or schema-declared name.

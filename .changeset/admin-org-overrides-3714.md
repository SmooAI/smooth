---
'@smooai/smooth': minor
---

`smoo admin org overrides list | set | remove` (SMOODEV-3714): super-admin per-org product access overrides over the SMOODEV-3711 API. Grant or deny one feature (`--enabled`/`--disabled`, required `--reason`, optional `--expires-at`, telephony-only `--included-voice-minutes`/`--overage-cents`) with client-side validation, a before/after banner, TTY confirmation and `--yes`/`--dry-run` for scripts. Internal `admin` build only.

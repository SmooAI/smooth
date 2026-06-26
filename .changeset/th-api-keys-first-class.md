---
'@smooai/smooth': patch
---

Make `th api keys` first-class for both auth-client types. `create` now takes structured `--type m2m|b2m` and repeatable `--allowed-origin` flags (B2M requires ≥1 origin, validated client-side) instead of a hand-written raw JSON body; `update <id> --allowed-origin …` replaces a B2M client's origin allowlist (PATCH, B2M-only); a new `rotate <id>` mints a replacement of the same type/origins then revokes the old one (the API has no in-place rotation, so the replacement is created first and the new client id + key are shown once). Adds accurate help (M2M secret vs B2M publishable, both shown once), `--json` on reads, and `--org-id [aliases: --org]`. The raw `--body` escape hatch stays. Fixes a latent bug: these routes require a dashboard user session (`auth.provider === 'supabase'`, 403 under M2M), so the surface now uses the user-JWT `UserClient` rather than the M2M-capable client. (pearl th-8d2a41)

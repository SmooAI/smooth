---
'@smooai/smooth': patch
---

Daemon refreshes its Smoo session so smoo-hub stays on the relay.

The relay supervisor re-read the stored access token on every reconnect but
never refreshed it, so on an always-on box (smoo-hub) once the ~hourly access
token expired the relay client got a 4401 and could no longer reconnect — the
box dropped off relay presence and phones couldn't reach it. `fresh_access_token`
now mints a fresh token from the stored refresh_token when the access token is
missing/expired/near-expiry (before connecting) and on a relay 4401/auth-close,
persisting the rotated tokens back to the credentials store (Supabase rotates
refresh tokens, so the new one must be saved). Reuses the same Supabase refresh
path as the credential heartbeat; best-effort and non-fatal, matching relay.rs's
existing retry/backoff style.

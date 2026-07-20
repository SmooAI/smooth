---
"@smooai/smooth": patch
---

Keep the Big Smooth daemon's Smoo AI session alive, and stop `/api/auth/status` from lying about it (pearl th-cbf613).

`GET /api/auth/status` reported `loggedIn: true` whenever the credentials file merely existed — never checking expiry — so the daemon claimed to be authenticated while holding a dead ~1h access token, and every `api.smoo.ai` call 401'd until a human re-ran sign-in. There was no refresh anywhere in the daemon.

- **Honest status.** `loggedIn` now means "session exists *and* is still usable". Adds `expiresAt`, `stale` (inside the 5-minute pre-expiry window), and `expired` (on disk but past expiry — renewal failed, sign in again). Identity (`user`/`orgId`) is kept even when expired so the UI can say *who* needs to re-auth; the sign-in pill now reads "Session expired — sign in".
- **Credential heartbeat.** A background task ticks every 60s and, when the session is inside the refresh window, exchanges its refresh token and persists the result. `smoo.ai/api/token` implements only `authorization_code` + the device grant, but the session it mints is a real Supabase session, so renewal goes direct to Supabase via `smooai_client_shared::auth::refresh` — persisting the rotated refresh token, which Supabase requires. Failures are logged at `error` and surface to the UI as `expired: true`; a heartbeat that fails quietly is worse than none.
- Supabase endpoint + anon key are env-overridable (`SMOOAI_SUPABASE_URL` / `SMOOAI_SUPABASE_ANON_KEY`), matching the four sign-in endpoints already in the same module.
- The M2M path (`smooai.json`, `client_credentials`) is untouched — it has no refresh token and must be re-minted from client_id/secret.

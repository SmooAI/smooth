---
"@smooai/smooth": patch
---

Fix `th`'s user-session auto-refresh, and funnel every credential load through
one choke point (pearl th-2273b8).

A `th auth login` Supabase session never expires — the project runs with
`sessions_timebox = 0` and `sessions_inactivity_timeout = 0`, so its 1-hour
access token should be invisible. It wasn't: refresh was copy-pasted per module,
and two of those copies didn't know user sessions existed.

- **`th api …` / `th admin …` no longer die an hour after login.**
  `smooai::try_user_session` loaded the user JWT into `SmoothApiClient`, whose
  `ensure_fresh_token` only ever knew how to re-mint M2M tokens from a stored
  `client_id`/`client_secret`. With a user session it silently returned `Ok(())`
  and let the request 401. It now refreshes through the shared choke point.
- **One entry point**: `auth::refresh::fresh_credentials_from` loads a store and
  applies whichever grant the new pure `decide()` picks (M2M re-mint vs Supabase
  `refresh_token` exchange), persisting the rotated token. `config.rs`'s
  hand-rolled copy of that branch is deleted, and `refresh_user_session` now
  delegates to client-shared's `refresh_session` instead of re-implementing the
  HTTP call.
- **`SmoothApiClient::ensure_fresh_token` fails legibly** ("run `th auth login`
  again") instead of silently deferring to a 401. It stays M2M-only on purpose:
  Supabase rotates refresh tokens with a 10-second reuse grace, so exactly one
  component may write that file, and this crate's `Credentials` has no `kind`
  discriminator — round-tripping a user session through it would downgrade the
  stored session to M2M.

Refresh stays lazy / on-demand in the CLI (no background thread), so it can't
race the daemon's credential heartbeat. M2M behaviour is unchanged.

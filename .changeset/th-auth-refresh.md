---
'@smooai/smooth': patch
---

Add `th auth refresh` — a headless, on-demand session refresh (th-1d3362). Defaults to the user session; `--m2m` targets the service-account session. It's a thin command over the existing `fresh_credentials_from` choke point, so it adds no refresh logic of its own: user sessions exchange the Supabase refresh token, M2M sessions re-mint via `client_credentials` (no browser, no rotation, no human). No-op — and says so — when the token still has runway. Fills the "`th auth` has login and profile but no refresh" gap; the auto-refresh in the `th api` request path was already there, this just exposes it as a standalone command.

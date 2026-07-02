---
'@smooai/smooth': minor
---

`th auth login` user sessions now effectively last as long as their Supabase refresh token — login is once-per-machine. The silent-refresh machinery (`refresh_user_session`) already existed but was only wired into `th auth whoami` and the config client; `UserClient` (agents mint, keys, CRM, LLM gateway, orgs) and `AdminClient` (`th admin *`) hard-bailed with "session expired — run `th auth login` again" after the ~1h JWT expiry. Both clients now load credentials through a shared `fresh_user_credentials()` helper that silently refreshes an expired session (persisting the rotated refresh token) and only errors — with a `th auth login` hint — when no session or refresh material exists. The access-token TTL is deliberately unchanged: short JWT + silent refresh over long-lived tokens.

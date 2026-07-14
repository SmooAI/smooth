---
"@smooai/smooth": minor
---

Big Smooth UI sign-in: add `GET /auth/login`, `GET /auth/callback`, and
`GET /api/auth/status` so a user viewing the Big Smooth web UI remotely
(e.g. over tailscale) can log `th` into Smoo AI by clicking a button
instead of SSHing in to run `th auth login`. Runs the browser OAuth2 +
PKCE flow through the daemon itself — the callback URL is derived from
the request's `Host` + `X-Forwarded-Proto`, the PKCE verifier is held in
a single-use, 10-minute-TTL in-memory store, and the resulting user
session is persisted to `~/.smooth/auth/smooai-user.json` (the same file
`th`'s user-authed API calls read). The web sidebar shows "Signed in as
…" or a "Sign in to Smoo AI" button accordingly.

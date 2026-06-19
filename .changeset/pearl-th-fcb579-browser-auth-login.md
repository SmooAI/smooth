---
"@smooai/smooth": patch
---

Pearl th-fcb579 — browser-based `th auth login` (smooth side). Lays the
OAuth2 + PKCE plumbing for `th auth login` to open the user's default
browser, capture the authorization code on a localhost listener, and
exchange it for tokens — matching the `gh auth login` / `gcloud auth
login` UX. Behind the `SMOOTH_AUTH_BROWSER=1` env gate while the
smooai-side `/cli-login` endpoint (pearl th-62e710) is in flight; new
`--browser` / `--no-browser` flags let callers override the gate
explicitly. Pairs with a single-store `active_org::set` writer that
will swap to the cross-store writer from pearl th-3217db once that
lands. New modules: `crates/smooth-cli/src/auth/pkce.rs` (RFC 7636
code verifier + S256 challenge generator), `auth/browser_login.rs`
(tiny_http listener, PKCE flow, token exchange), `auth/active_org.rs`
(active-org-id persistence helper). Headless / SSH / CI paths
unchanged — no TTY = no browser, ever. M2M (`--m2m`) unchanged.

# Browser-based `th auth login` — smooth side

Pearl: **th-fcb579**. Pairs with **th-62e710** (smooai side: `/cli-login` route + org picker).

## Goal

`th auth login` opens the browser, captures the OAuth code on a localhost listener, exchanges for tokens via PKCE, and persists active org via the shared `active_org::set()` writer from th-3217db. Matches `gh auth login` / `gcloud auth login` / `pi auth login` UX.

## Flow

```
$ th auth login
  → Bind localhost listener on random high port (e.g. 47812)
  → Generate PKCE code_verifier + code_challenge (SHA-256)
  → Generate random state token (CSRF guard)
  → Print: "Opening browser to https://auth.smoo.ai/cli-login ..."
  → `open` https://auth.smoo.ai/cli-login?redirect_uri=http://localhost:47812/callback
    &state=<state>&code_challenge=<challenge>&code_challenge_method=S256
  → Block on local listener (timeout 5min)
  → Receive GET /callback?code=<code>&state=<state>&org_id=<org_id>
  → Validate state matches
  → POST https://auth.smoo.ai/token with code + code_verifier (PKCE exchange)
  → Receive {access_token, refresh_token, expires_in}
  → active_org::set(org_id)  -- writes ALL three credential stores in lockstep (pearl th-3217db)
  → Store tokens in user profile store
  → Print: "✓ Signed in as <email> · active org: <org_name>"
```

`N == 1` org: server auto-selects, callback comes immediately with `org_id`.
`N > 1` orgs: server renders picker page; callback fires only after user picks.

## Files to touch

```
crates/smooth-cli/src/auth/
├── login.rs                    # existing — extend cmd_login_user
├── browser_login.rs            # NEW — listener + PKCE + browser opener
└── pkce.rs                     # NEW — code_verifier/challenge generator
```

Plus wiring in the `Commands::AuthLogin` clap definition: new `--browser` / `--no-browser` flags. Default to browser for user login (M2M keeps prompt; bots can't pop a browser).

## Dependencies

- `tiny_http = "0.12"` — minimal blocking HTTP server (one endpoint, no async needed for callback capture). Avoids dragging in axum/hyper for this one use.
- `open = "5"` — cross-platform `open` for the browser
- `sha2 = "0.10"` — already a transitive dep; use directly for PKCE challenge
- `base64 = "0.22"` — URL-safe base64 for PKCE encoding (probably already pulled in)
- `rand = "0.8"` — random state token + code_verifier

## Tests (colocated under `#[cfg(test)]`)

- **PKCE round-trip**: code_verifier → code_challenge (S256) matches RFC 7636 vector
- **Callback parser**: valid `?code=…&state=…&org_id=…` extracted; missing params error cleanly; wrong state rejected
- **Port fallback**: if 47812 is bound, listener finds the next free port and threads it into the redirect_uri
- **Timeout**: 5-minute deadline → clean error, no zombie listener
- **Browser-denied**: server redirects to `/callback?error=access_denied&state=…` → CLI surfaces a useful message
- **`--no-browser` flag**: falls back to existing prompt flow without binding any port
- **Active-org write contract**: after successful exchange, `active_org::resolve(None)` returns the chosen org_id (uses Agent 2's helper; the test exercises the integration)

## Edge cases

- **Port conflict**: bind to `127.0.0.1:0` (let the OS pick), then read back the assigned port. Avoids both collision and the hardcoded :47812 leaking into firewall logs.
- **Headless / SSH / CI**: `--no-browser` flag falls back to today's prompt flow. Auto-detect via `IsTerminal::is_terminal(stdout)` and prompt the user whether to open or paste a one-time code.
- **Multiple `th auth login` racing**: listener bind is atomic; second invocation gets EADDRINUSE on its own (different) random port — no shared state to corrupt.
- **Browser hangs**: the 5-minute timeout drops the listener. User can re-run.
- **State token replay**: state is single-use; the listener exits after first match.

## Out of scope

- M2M (`th auth login --m2m`) keeps the existing client_credentials flow. Bots don't pop browsers.
- Refresh-token rotation logic stays unchanged; this just changes how the INITIAL tokens are obtained.

## Ship order

1. PKCE module + tests (no external deps yet — just RFC 7636 round-trip)
2. Listener module + tests (port-binding, callback parsing)
3. Wire into `cmd_login_user` behind `--browser` flag, default off
4. Once smooai-side `/cli-login` is live (pearl th-62e710), flip default to on
5. Behind a feature flag (`SMOOTH_AUTH_BROWSER=1` initially) so we can dark-launch

## Verification

After this lands AND the smooai side ships:

```bash
th auth logout
th auth login                    # browser pops, you sign in, pick org
th auth whoami                   # shows email + active org
th config list --env production  # works without further org switch (th-3217db invariant)
```

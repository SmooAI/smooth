//! Browser-based OAuth2 + PKCE flow for `th auth login`.
//!
//! Flow:
//! 1. Generate PKCE pair + CSRF state token.
//! 2. Bind a localhost HTTP listener on `127.0.0.1:0` (OS-assigned
//!    high port).
//! 3. Open `https://auth.smoo.ai/cli-login?redirect_uri=…&state=…
//!    &code_challenge=…&code_challenge_method=S256` in the user's
//!    default browser.
//! 4. Block on the listener (5-minute timeout) for the callback at
//!    `/callback?code=…&state=…&org_id=…` (success) or `?error=…&state=…`
//!    (denied).
//! 5. POST `https://auth.smoo.ai/token` with `grant_type=authorization_code`,
//!    the `code`, the original `code_verifier`, and the same
//!    `redirect_uri` we registered for the auth request.
//! 6. Hand the resulting `{access_token, refresh_token, expires_in,
//!    org_id}` back to the caller, which persists it via the shared
//!    active-org writer (pearl th-3217db).
//!
//! Pearl th-fcb579.

use std::io::Cursor;
use std::net::TcpListener;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tiny_http::{Header, Response, Server};

use super::pkce::PkcePair;

/// Default deadline before we give up on the browser callback. The
/// design doc fixes 5 minutes — more than enough for a user to sign
/// in + pick an org, short enough that a forgotten terminal session
/// doesn't leak a listener forever.
pub const DEFAULT_CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

/// One callback successfully parsed off the local listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackResult {
    /// Authorization code to redeem at the token endpoint.
    pub code: String,
    /// Organization id the user picked in the browser. The server
    /// resolves this for us; on `N == 1` orgs it skips the picker and
    /// returns the user's only org immediately.
    pub org_id: Option<String>,
}

/// One callback parsed off the local listener — success xor error.
/// Public for the unit tests; production callers only see the
/// `Result<CallbackResult>` returned from [`wait_for_callback`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedCallback {
    Ok(CallbackResult),
    /// Upstream signalled a user-cancellable error (e.g.
    /// `access_denied`). String is the upstream `error` value.
    Denied(String),
    /// Server redirected with no usable parameters (or a state we
    /// didn't issue). Treated as a hard failure — never silently
    /// retried.
    Invalid(String),
}

/// Parse a tiny_http URL (path + query, no scheme/host) for the
/// `/callback` endpoint, validating the `state` token matches the one
/// we generated.
///
/// Returns one of:
///   - `Ok(ParsedCallback::Ok(...))` on `?code=…&state=…[&org_id=…]`
///   - `Ok(ParsedCallback::Denied(reason))` on `?error=…&state=…`
///   - `Ok(ParsedCallback::Invalid(msg))` on any other shape or a
///     state mismatch
///   - `Err(...)` only if the URL itself is unparseable
pub fn parse_callback(url: &str, expected_state: &str) -> Result<ParsedCallback> {
    // tiny_http hands us a path-and-query like "/callback?code=…&state=…".
    // We don't want to drag the `url` crate's full URL parser in for
    // this — just split on the first '?'.
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    };
    if path != "/callback" {
        return Ok(ParsedCallback::Invalid(format!("unexpected path {path}")));
    }
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut org_id: Option<String> = None;
    let mut error: Option<String> = None;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(v);
        match k {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "org_id" => org_id = Some(v),
            "error" => error = Some(v),
            // Unknown query params are forwarded by some IdPs (e.g.
            // `error_description`). Drop them silently — the
            // canonical signal is `error`.
            _ => {}
        }
    }
    let Some(state) = state else {
        return Ok(ParsedCallback::Invalid("missing state".to_string()));
    };
    if state != expected_state {
        // CSRF: someone redirected the browser to our callback with a
        // state we never issued. Refuse without inspecting `code`.
        return Ok(ParsedCallback::Invalid("state mismatch".to_string()));
    }
    if let Some(err) = error {
        return Ok(ParsedCallback::Denied(err));
    }
    let Some(code) = code else {
        return Ok(ParsedCallback::Invalid("missing code".to_string()));
    };
    Ok(ParsedCallback::Ok(CallbackResult { code, org_id }))
}

/// Minimal percent-decoder for the handful of bytes a sane IdP escapes
/// in query values (`+`, `%XX`). Avoids pulling in `urlencoding` /
/// `percent-encoding` for this one call site.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    #[allow(clippy::cast_possible_truncation)]
                    out.push(((hi << 4) | lo) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build the authorization URL we'll point the browser at.
#[must_use]
pub fn build_authorize_url(base: &str, redirect_uri: &str, state: &str, challenge: &str) -> String {
    // Manual query construction — no `url` crate needed for this
    // narrow surface. `state` and `challenge` are already
    // URL-safe-no-pad base64 (no '%', '&', '=' to escape); the
    // redirect_uri does have ':' and '/', so we percent-encode it.
    format!(
        "{base}?redirect_uri={ru}&state={state}&code_challenge={challenge}&code_challenge_method=S256",
        ru = url_encode(redirect_uri),
    )
}

/// Minimal percent-encoder for the URL building above. RFC 3986
/// unreserved set is left as-is.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~') {
            out.push(c);
        } else {
            // %XX, upper-case hex per RFC 3986.
            use std::fmt::Write as _;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// HTML the browser tab sees after the callback fires. Closed by JS
/// on a short timer; falls back to a "you can close this tab" line
/// if scripts are blocked.
const SUCCESS_HTML: &str = r#"<!doctype html>
<html><head><title>th auth login — done</title></head>
<body style="font-family: system-ui, sans-serif; padding: 2rem; max-width: 32rem;">
<h2>You're signed in.</h2>
<p>You can close this tab and return to your terminal.</p>
<script>setTimeout(() => window.close(), 1500);</script>
</body></html>"#;

const ERROR_HTML: &str = r#"<!doctype html>
<html><head><title>th auth login — error</title></head>
<body style="font-family: system-ui, sans-serif; padding: 2rem; max-width: 32rem;">
<h2>Sign-in failed.</h2>
<p>Return to your terminal — th will show the details.</p>
</body></html>"#;

/// Listener wrapper that knows its OS-assigned port.
pub struct Listener {
    server: Server,
    port: u16,
}

impl Listener {
    /// Bind on `127.0.0.1:0` so the OS picks a free port. We pre-bind
    /// with stdlib `TcpListener` so we can read the assigned port
    /// before handing the socket to `tiny_http` — this avoids both
    /// hardcoded port collisions and noisy firewall log entries from
    /// the same well-known port every session.
    pub fn bind() -> Result<Self> {
        let std_listener = TcpListener::bind(("127.0.0.1", 0)).context("bind 127.0.0.1:0 for OAuth callback")?;
        let port = std_listener.local_addr().context("read OS-assigned port back")?.port();
        let server = Server::from_listener(std_listener, None).map_err(|e| anyhow!("hand listener to tiny_http: {e}"))?;
        Ok(Self { server, port })
    }

    /// The OS-assigned port we ended up bound to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The `redirect_uri` we should advertise to the authorization
    /// server.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }
}

/// Block on the listener until one callback arrives (success or
/// error), then send the browser an HTML closer page and return the
/// parsed result. Times out after `timeout` with a clean error and
/// drops the listener so no zombie port stays open.
pub fn wait_for_callback(listener: &Listener, expected_state: &str, timeout: Duration) -> Result<CallbackResult> {
    let req = listener.server.recv_timeout(timeout).context("listening for OAuth callback")?.ok_or_else(|| {
        anyhow!(
            "timed out after {}s waiting for the browser callback — re-run `th auth login`",
            timeout.as_secs()
        )
    })?;
    let url = req.url().to_string();
    let parsed = parse_callback(&url, expected_state)?;
    let (status, body) = match &parsed {
        ParsedCallback::Ok(_) => (200u32, SUCCESS_HTML),
        _ => (400u32, ERROR_HTML),
    };
    let response = Response::new(
        status.into(),
        vec![Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).map_err(|()| anyhow!("construct Content-Type header"))?],
        Cursor::new(body.as_bytes()),
        Some(body.len()),
        None,
    );
    // Best-effort — if the browser already disconnected, that's fine.
    let _ = req.respond(response);
    match parsed {
        ParsedCallback::Ok(cb) => Ok(cb),
        ParsedCallback::Denied(reason) => bail!("Sign-in was cancelled (server returned error={reason}). Re-run `th auth login` to try again."),
        ParsedCallback::Invalid(msg) => bail!("OAuth callback rejected: {msg}. Re-run `th auth login`."),
    }
}

/// The token-exchange response from `POST /token` with
/// `grant_type=authorization_code`. Mirrors the contract documented
/// in `DESIGN.md` — extra fields are ignored.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TokenExchangeResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// The org the server resolved for this session (single-org
    /// shortcut or browser picker).
    #[serde(default)]
    pub org_id: Option<String>,
    /// Authenticated user identifier (display only). The smooai
    /// side may surface `email` or `user_id`; we accept either.
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

/// Exchange the authorization code for tokens, sending the original
/// PKCE verifier the server will re-hash and compare.
///
/// # Errors
/// - Network failure
/// - Non-2xx from `/token` (surfaces upstream body verbatim)
/// - Malformed JSON body
pub async fn exchange_code(http: &reqwest::Client, token_url: &str, code: &str, verifier: &str, redirect_uri: &str) -> Result<TokenExchangeResponse> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ];
    let resp = http.post(token_url).form(&form).send().await.with_context(|| format!("POST {token_url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("token exchange returned HTTP {status}: {text}");
    }
    serde_json::from_str::<TokenExchangeResponse>(&text).with_context(|| format!("parse token-exchange response: {text}"))
}

/// One-shot end-to-end browser login.
///
/// Caller owns the policy decision (TTY check, env-var gate). This
/// helper just runs the flow once those preconditions are met.
pub async fn run_browser_login(http: &reqwest::Client, authorize_base: &str, token_url: &str) -> Result<BrowserLoginOutcome> {
    let pair = PkcePair::generate();
    let state = super::pkce::random_state();
    let listener = Listener::bind()?;
    let redirect_uri = listener.redirect_uri();
    let authorize_url = build_authorize_url(authorize_base, &redirect_uri, &state, &pair.challenge);
    // Best-effort browser open; if it fails (no display, exotic
    // platform), we still print the URL so the user can paste it.
    println!("Opening browser to sign in...");
    println!("  {authorize_url}");
    if let Err(e) = open::that(&authorize_url) {
        eprintln!("  (couldn't auto-open the browser: {e}. Copy the URL above into a browser to continue.)");
    }
    let cb = wait_for_callback(&listener, &state, DEFAULT_CALLBACK_TIMEOUT)?;
    let tokens = exchange_code(http, token_url, &cb.code, &pair.verifier, &redirect_uri).await?;
    // Server is the source of truth for `org_id`. Prefer the value
    // off the callback (it's what the user picked in the browser),
    // fall back to the token-response field if the server only puts
    // it there.
    let org_id = cb.org_id.or(tokens.org_id.clone());
    Ok(BrowserLoginOutcome { tokens, org_id })
}

/// Bundle of "what we learnt from a successful browser login" — the
/// caller persists this via the shared credential stores.
#[derive(Debug, Clone)]
pub struct BrowserLoginOutcome {
    pub tokens: TokenExchangeResponse,
    pub org_id: Option<String>,
}

// ────────────────────────────────────────────────────────────────────
//  Tests
// ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;

    use super::*;

    fn ok_state() -> String {
        "the-expected-state".to_string()
    }

    #[test]
    fn parse_callback_happy_path_with_org() {
        let r = parse_callback("/callback?code=abc123&state=the-expected-state&org_id=org_42", &ok_state()).expect("parse");
        match r {
            ParsedCallback::Ok(cb) => {
                assert_eq!(cb.code, "abc123");
                assert_eq!(cb.org_id.as_deref(), Some("org_42"));
            }
            other => panic!("expected Ok variant, got {other:?}"),
        }
    }

    #[test]
    fn parse_callback_happy_path_without_org() {
        let r = parse_callback("/callback?code=abc123&state=the-expected-state", &ok_state()).expect("parse");
        match r {
            ParsedCallback::Ok(cb) => {
                assert_eq!(cb.code, "abc123");
                assert!(cb.org_id.is_none());
            }
            other => panic!("expected Ok variant, got {other:?}"),
        }
    }

    #[test]
    fn parse_callback_state_mismatch_is_invalid() {
        let r = parse_callback("/callback?code=abc&state=wrong", &ok_state()).expect("parse");
        assert_eq!(r, ParsedCallback::Invalid("state mismatch".to_string()));
    }

    #[test]
    fn parse_callback_missing_state_is_invalid() {
        let r = parse_callback("/callback?code=abc", &ok_state()).expect("parse");
        assert_eq!(r, ParsedCallback::Invalid("missing state".to_string()));
    }

    #[test]
    fn parse_callback_missing_code_is_invalid() {
        let r = parse_callback("/callback?state=the-expected-state", &ok_state()).expect("parse");
        assert_eq!(r, ParsedCallback::Invalid("missing code".to_string()));
    }

    #[test]
    fn parse_callback_access_denied() {
        let r = parse_callback("/callback?error=access_denied&state=the-expected-state", &ok_state()).expect("parse");
        assert_eq!(r, ParsedCallback::Denied("access_denied".to_string()));
    }

    #[test]
    fn parse_callback_denied_with_wrong_state_is_invalid() {
        // Even on a denial, we refuse anything carrying the wrong CSRF
        // state — otherwise an attacker could surface fake "denied"
        // messages.
        let r = parse_callback("/callback?error=access_denied&state=other", &ok_state()).expect("parse");
        assert_eq!(r, ParsedCallback::Invalid("state mismatch".to_string()));
    }

    #[test]
    fn parse_callback_wrong_path_is_invalid() {
        let r = parse_callback("/something-else?code=abc&state=the-expected-state", &ok_state()).expect("parse");
        match r {
            ParsedCallback::Invalid(msg) => assert!(msg.contains("unexpected path"), "got {msg}"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn parse_callback_handles_extra_query_params() {
        // IdPs often forward `error_description`, `iss`, etc. We
        // ignore them and key only on `code`/`state`/`org_id`/`error`.
        let r = parse_callback(
            "/callback?code=abc&state=the-expected-state&org_id=org_1&error_description=hi&iss=auth.smoo.ai",
            &ok_state(),
        )
        .expect("parse");
        match r {
            ParsedCallback::Ok(cb) => {
                assert_eq!(cb.code, "abc");
                assert_eq!(cb.org_id.as_deref(), Some("org_1"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn parse_callback_percent_decodes_org_id() {
        let r = parse_callback("/callback?code=abc&state=the-expected-state&org_id=org%2Fwith%20spaces", &ok_state()).expect("parse");
        match r {
            ParsedCallback::Ok(cb) => assert_eq!(cb.org_id.as_deref(), Some("org/with spaces")),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn build_authorize_url_includes_all_pkce_params() {
        let url = build_authorize_url("https://auth.smoo.ai/cli-login", "http://127.0.0.1:47812/callback", "STATE123", "CHALLENGE456");
        assert!(url.starts_with("https://auth.smoo.ai/cli-login?"), "got {url}");
        assert!(url.contains("state=STATE123"));
        assert!(url.contains("code_challenge=CHALLENGE456"));
        assert!(url.contains("code_challenge_method=S256"));
        // redirect_uri must be percent-encoded — `:` and `/` should
        // become %3A and %2F.
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A47812%2Fcallback"), "got {url}");
    }

    #[test]
    fn listener_binds_to_random_high_port() {
        let l1 = Listener::bind().expect("bind 1");
        let l2 = Listener::bind().expect("bind 2");
        // OS-assigned ports are non-zero and (overwhelmingly) distinct.
        assert!(l1.port() > 0);
        assert!(l2.port() > 0);
        assert_ne!(l1.port(), l2.port(), "OS should hand out distinct ports");
    }

    #[test]
    fn listener_redirect_uri_includes_port() {
        let l = Listener::bind().expect("bind");
        let port = l.port();
        assert_eq!(l.redirect_uri(), format!("http://127.0.0.1:{port}/callback"));
    }

    /// End-to-end: spawn the listener on a background thread, have
    /// the main thread play the role of the browser by GET-ing the
    /// callback URL. Exercises `wait_for_callback` happy path.
    #[test]
    fn wait_for_callback_returns_parsed_result() {
        let listener = Listener::bind().expect("bind");
        let port = listener.port();
        let state = Arc::new(ok_state());
        let state_for_thread = state.clone();

        let handle = thread::spawn(move || wait_for_callback(&listener, &state_for_thread, Duration::from_secs(5)));

        // Give the listener a beat to be ready.
        thread::sleep(Duration::from_millis(50));

        // Pretend to be the browser.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let req = format!(
            "GET /callback?code=abc&state={state}&org_id=org_42 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n",
            state = *state,
        );
        stream.write_all(req.as_bytes()).expect("write");
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);

        let cb = handle.join().expect("thread join").expect("callback");
        assert_eq!(cb.code, "abc");
        assert_eq!(cb.org_id.as_deref(), Some("org_42"));
    }

    /// Browser hits the callback with `?error=access_denied` — surface
    /// it as a clean Err to the caller.
    #[test]
    fn wait_for_callback_surfaces_access_denied() {
        let listener = Listener::bind().expect("bind");
        let port = listener.port();
        let state = ok_state();
        let state_for_thread = state.clone();

        let handle = thread::spawn(move || wait_for_callback(&listener, &state_for_thread, Duration::from_secs(5)));
        thread::sleep(Duration::from_millis(50));

        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let req = format!("GET /callback?error=access_denied&state={state} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).expect("write");
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);

        let err = handle.join().expect("thread join").expect_err("should be Err");
        let msg = format!("{err:#}");
        assert!(msg.contains("access_denied"), "expected access_denied in {msg}");
    }

    /// No callback within the deadline → clean timeout error, listener
    /// drops cleanly.
    #[test]
    fn wait_for_callback_times_out_cleanly() {
        let listener = Listener::bind().expect("bind");
        let start = Instant::now();
        let result = wait_for_callback(&listener, &ok_state(), Duration::from_millis(200));
        let elapsed = start.elapsed();
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("timed out"), "expected timeout msg in {msg}");
        // Sanity: we honored the deadline within a generous margin
        // (CI runners stall sometimes).
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    }

    /// Wrong-state callbacks are rejected — defends against an
    /// attacker who tricked the user's browser into hitting the
    /// loopback port with a code they minted elsewhere.
    #[test]
    fn wait_for_callback_rejects_state_mismatch() {
        let listener = Listener::bind().expect("bind");
        let port = listener.port();
        let handle = thread::spawn(move || wait_for_callback(&listener, "the-expected-state", Duration::from_secs(5)));
        thread::sleep(Duration::from_millis(50));

        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let req = format!("GET /callback?code=abc&state=wrong HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).expect("write");
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);

        let err = handle.join().expect("thread join").expect_err("should reject");
        let msg = format!("{err:#}");
        assert!(msg.contains("state mismatch"), "expected state mismatch in {msg}");
    }

    /// End-to-end token exchange against a fixture HTTP server. Proves
    /// the form-encoded body shape matches what auth.smoo.ai will
    /// expect and that we parse the response correctly.
    #[tokio::test]
    async fn exchange_code_round_trips_against_fixture_server() {
        // Stand up a tiny_http fixture on a random port to play the
        // token endpoint.
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fixture");
        let port = listener.local_addr().expect("port").port();
        let server = Server::from_listener(listener, None).expect("server");

        let server_handle = thread::spawn(move || {
            let mut req = server.recv().expect("recv");
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).expect("read body");
            // Sanity: we sent the form fields the contract requires.
            assert!(body.contains("grant_type=authorization_code"), "body={body}");
            assert!(body.contains("code=THECODE"), "body={body}");
            assert!(body.contains("code_verifier=THEVERIFIER"), "body={body}");
            let response =
                Response::from_string(r#"{"access_token":"acc","refresh_token":"ref","expires_in":3600,"org_id":"org_99","email":"hi@example.com"}"#);
            req.respond(response).expect("respond");
        });

        let http = reqwest::Client::new();
        let result = exchange_code(
            &http,
            &format!("http://127.0.0.1:{port}/token"),
            "THECODE",
            "THEVERIFIER",
            "http://127.0.0.1:9999/callback",
        )
        .await
        .expect("exchange");
        assert_eq!(result.access_token, "acc");
        assert_eq!(result.refresh_token.as_deref(), Some("ref"));
        assert_eq!(result.expires_in, Some(3600));
        assert_eq!(result.org_id.as_deref(), Some("org_99"));
        assert_eq!(result.email.as_deref(), Some("hi@example.com"));

        server_handle.join().expect("server join");
    }

    /// Non-2xx from /token surfaces the upstream body verbatim — no
    /// silent "internal error" masking.
    #[tokio::test]
    async fn exchange_code_surfaces_upstream_4xx() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fixture");
        let port = listener.local_addr().expect("port").port();
        let server = Server::from_listener(listener, None).expect("server");

        let server_handle = thread::spawn(move || {
            let req = server.recv().expect("recv");
            let response = Response::from_string(r#"{"error":"invalid_grant","error_description":"code reused"}"#).with_status_code(400);
            req.respond(response).expect("respond");
        });

        let http = reqwest::Client::new();
        let err = exchange_code(
            &http,
            &format!("http://127.0.0.1:{port}/token"),
            "STALE",
            "VERIFIER",
            "http://127.0.0.1:9999/callback",
        )
        .await
        .expect_err("expected non-2xx to error");
        let msg = format!("{err:#}");
        assert!(msg.contains("400"), "expected status in {msg}");
        assert!(msg.contains("invalid_grant"), "expected upstream body in {msg}");

        server_handle.join().expect("server join");
    }

    #[test]
    fn url_encode_unreserved_set_unchanged() {
        let s = "abcXYZ-._~0123";
        assert_eq!(url_encode(s), s);
    }

    #[test]
    fn url_encode_escapes_reserved_chars() {
        assert_eq!(url_encode("http://x:1/"), "http%3A%2F%2Fx%3A1%2F");
    }
}

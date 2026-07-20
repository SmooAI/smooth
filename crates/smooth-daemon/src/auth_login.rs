//! Big Smooth UI sign-in — browser OAuth2 + PKCE routed through the
//! daemon itself.
//!
//! Lets a user viewing the web UI remotely (e.g. over a tailscale-served
//! origin) log `th` into Smoo AI by clicking a button instead of SSHing
//! in and running `th auth login`.
//!
//! Flow:
//! 1. `GET /auth/login` — the UI navigates the browser here. We derive
//!    the daemon's own public callback URL from the request headers
//!    (`X-Forwarded-Proto` + `Host`), mint a PKCE pair + CSRF `state`,
//!    stash the verifier under `state` in [`PendingLogins`], and 302 to
//!    `smoo.ai/cli-login?redirect_uri=…&state=…&code_challenge=…`.
//! 2. The user signs in on smoo.ai and picks an org; smoo.ai redirects
//!    the browser back to `{daemon}/auth/callback?code=&state=&org_id=`.
//! 3. `GET /auth/callback` — look up + single-use-remove the `state`,
//!    exchange the code for tokens at `smoo.ai/api/token`, and persist a
//!    user `Credentials` to `~/.smooth/auth/smooai-user.json` (the same
//!    file `th`'s user-authed API calls read via
//!    `CredentialsStore::default_user()`).
//! 4. `GET /api/auth/status` — the UI polls this to render "Signed in as
//!    …" vs a sign-in button. Expiry-aware: a present-but-dead session
//!    reports `loggedIn: false` + `expired: true`, not "signed in".
//! 5. [`spawn_credential_heartbeat`] — a background task that renews the
//!    session against Supabase before its ~1h access token expires, so
//!    the daemon doesn't silently rot into 401s (th-cbf613).
//!
//! ponytail: duplicated from smooth-cli auth::{browser_login,pkce} (and
//! from smooth-bigsmooth's auth_login); extract to a shared crate if a
//! fourth consumer appears. Unlike bigsmooth's version this router owns
//! its own state (`AuthState`) via `.with_state(...)` so it needs no
//! daemon `AppState` — it merges cleanly alongside search/push.
//!
//! ponytail: `smoo.ai/cli-login` may only allowlist `localhost`
//! redirect_uris, so the tailnet host (`:8443`) could be rejected on the
//! first real click. Deriving redirect_uri from Host/X-Forwarded-Proto
//! (as main does) is the correct generic derivation; the tailnet
//! redirect_uri may need a smoo.ai allowlist entry or a device-code
//! fallback before it works over the tailnet.
//!
//! Pearl th-bc624a.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use smooai_client_shared::auth::refresh;
use smooai_client_shared::auth::storage::{CredentialKind, Credentials, CredentialsStore};
use tokio::sync::Semaphore;

/// How long a mint-a-login stays valid before we forget its verifier.
/// 10 minutes is generous for a human sign-in + org pick, short enough
/// that an abandoned tab doesn't leave a live PKCE secret around.
// `from_mins` is still unstable, so `from_secs(600)` is the readable
// stable spelling — silence the pedantic "use a larger unit" nudge.
#[allow(clippy::duration_suboptimal_units)]
const PENDING_TTL: Duration = Duration::from_secs(600);

/// Max concurrent in-flight device logins. Each `/auth/device/start` holds
/// a slot for the full lifetime of its background poll loop (up to
/// `expires_in`, ~900s). A personal daemon has one human clicking a button;
/// 3 covers a couple of stale/abandoned tabs. Over-cap → 429, so an
/// unauthenticated caller on the tailnet can't spawn unbounded 900s loops
/// that hammer the daemon + smoo.ai (resource-amplification DoS). th-ea7b54.
const MAX_PENDING_DEVICE_LOGINS: usize = 3;

// ── env-overridable smoo.ai endpoints (copied from smooth-cli auth::login) ──

const DEFAULT_CLI_LOGIN_URL: &str = "https://smoo.ai/cli-login";
const DEFAULT_CLI_TOKEN_URL: &str = "https://smoo.ai/api/token";
/// Device Authorization Grant (RFC 8628) endpoint + public client id.
const DEFAULT_CLI_DEVICE_URL: &str = "https://smoo.ai/api/device/code";
const DEFAULT_DEVICE_CLIENT_ID: &str = "bigsmooth-daemon";

fn cli_login_url() -> String {
    std::env::var("SMOOAI_CLI_LOGIN_URL").unwrap_or_else(|_| DEFAULT_CLI_LOGIN_URL.to_string())
}

fn cli_token_url() -> String {
    std::env::var("SMOOAI_CLI_TOKEN_URL").unwrap_or_else(|_| DEFAULT_CLI_TOKEN_URL.to_string())
}

fn cli_device_url() -> String {
    std::env::var("SMOOAI_CLI_DEVICE_URL").unwrap_or_else(|_| DEFAULT_CLI_DEVICE_URL.to_string())
}

fn device_client_id() -> String {
    std::env::var("SMOOAI_DEVICE_CLIENT_ID").unwrap_or_else(|_| DEFAULT_DEVICE_CLIENT_ID.to_string())
}

// ── Supabase endpoints (session refresh) ────────────────────────────
//
// `smoo.ai/api/token` implements only `authorization_code` + the device
// grant — it 400s `unsupported_grant_type` on a refresh. But the session
// it mints IS a real Supabase session (admin `generateLink` → `verifyOtp`),
// so the stored `refresh_token` is a genuine Supabase refresh token and the
// renewal goes **direct to Supabase**. Same constants + env-var names
// `th auth login` uses (smooth-cli `auth::{supabase_url, supabase_anon_key}`),
// duplicated rather than shared because smooth-daemon doesn't depend on
// smooth-cli.
//
// ponytail: env vars, not `@smooai/config`. The house rule prefers config
// keys, but the four endpoints above are already
// `std::env::var(..).unwrap_or_else(default)` and matching them beats being
// the one odd knob — worth revisiting for all six together.

const DEFAULT_SUPABASE_URL: &str = "https://db.smoo.ai";
/// The **anon** (publishable) key — the same one every customer-website JS
/// bundle ships. Not a secret; the service-role key is never touched here.
const DEFAULT_SUPABASE_ANON_KEY: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6InhycWJxZ290Z2hpdGNmdW91a2RrIiwicm9sZSI6ImFub24iLCJpYXQiOjE3NDEwNDEyODksImV4cCI6MjA1NjYxNzI4OX0.KHwbyjdrBhCiP6Na8aY8b3fA6RNkCqJ4m-dmY4AOdmw";

fn supabase_url() -> String {
    std::env::var("SMOOAI_SUPABASE_URL").unwrap_or_else(|_| DEFAULT_SUPABASE_URL.to_string())
}

fn supabase_anon_key() -> String {
    std::env::var("SMOOAI_SUPABASE_ANON_KEY").unwrap_or_else(|_| DEFAULT_SUPABASE_ANON_KEY.to_string())
}

// ── router state ────────────────────────────────────────────────────

/// Self-contained state for the sign-in router: the in-flight PKCE
/// pending-logins map + a shared HTTP client for the token exchange.
/// Owned by the router via `.with_state(...)`, so this module needs no
/// daemon `AppState`.
#[derive(Clone)]
pub struct AuthState {
    pending_logins: PendingLogins,
    http: reqwest::Client,
    /// Bounds concurrent in-flight device logins (see
    /// [`MAX_PENDING_DEVICE_LOGINS`]). Each spawned poll loop holds one
    /// permit for its whole lifetime; dropping the permit (task ends on
    /// success/expiry/error) releases the slot.
    device_slots: Arc<Semaphore>,
}

impl Default for AuthState {
    fn default() -> Self {
        Self {
            pending_logins: PendingLogins::default(),
            http: reqwest::Client::default(),
            device_slots: Arc::new(Semaphore::new(MAX_PENDING_DEVICE_LOGINS)),
        }
    }
}

/// Build the sign-in router: `GET /auth/login`, `GET /auth/callback`,
/// `GET /api/auth/status`. Merged alongside search/push in the daemon's
/// `serve_routes(...)` call.
pub fn auth_router() -> Router {
    Router::new()
        .route("/auth/login", get(login_handler))
        .route("/auth/callback", get(callback_handler))
        .route("/auth/device/start", post(device_start_handler))
        .route("/api/auth/status", get(status_handler))
        .with_state(AuthState::default())
}

// ── PKCE (RFC 7636), copied from smooth-cli auth::pkce ──────────────

/// One in-flight login: the PKCE verifier we must present on the token
/// exchange, the exact redirect_uri we advertised (must match on
/// exchange), and when it was created (for TTL pruning).
struct Pending {
    verifier: String,
    redirect_uri: String,
    created: Instant,
}

/// State store for the two-request (login → callback) PKCE flow, keyed
/// by CSRF `state`. NEVER log the verifier or any token.
#[derive(Clone, Default)]
pub struct PendingLogins(Arc<Mutex<HashMap<String, Pending>>>);

impl PendingLogins {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a new pending login under `state`, pruning any expired
    /// entries first so the map can't grow unbounded from abandoned tabs.
    fn insert(&self, state: String, verifier: String, redirect_uri: String) {
        let mut map = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        map.retain(|_, p| p.created.elapsed() < PENDING_TTL);
        map.insert(
            state,
            Pending {
                verifier,
                redirect_uri,
                created: Instant::now(),
            },
        );
    }

    /// Single-use lookup: remove and return the pending entry for
    /// `state`, but only if it hasn't expired. Removal is unconditional
    /// (a stale entry is dropped, not left to linger).
    fn take_valid(&self, state: &str) -> Option<Pending> {
        let mut map = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending = map.remove(state)?;
        (pending.created.elapsed() < PENDING_TTL).then_some(pending)
    }
}

/// One PKCE pair: secret verifier + derived S256 challenge.
struct PkcePair {
    verifier: String,
    challenge: String,
}

impl PkcePair {
    fn generate() -> Self {
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
        let challenge = derive_challenge(&verifier);
        Self { verifier, challenge }
    }
}

/// `BASE64URL-NO-PAD(SHA-256(verifier))` per RFC 7636 §4.2.
fn derive_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// Fresh URL-safe-no-pad CSRF state token (16 random bytes → 22 chars).
fn random_state() -> String {
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

// ── URL building + callback parsing (copied from smooth-cli auth::browser_login) ──

/// Build the authorization URL we redirect the browser to.
fn build_authorize_url(base: &str, redirect_uri: &str, state: &str, challenge: &str) -> String {
    // `state` and `challenge` are URL-safe-no-pad base64 (nothing to
    // escape); the redirect_uri has ':' and '/', so percent-encode it.
    format!(
        "{base}?redirect_uri={ru}&state={state}&code_challenge={challenge}&code_challenge_method=S256",
        ru = url_encode(redirect_uri),
    )
}

/// Minimal RFC 3986 percent-encoder — unreserved set left as-is.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~') {
            out.push(c);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Minimal percent-decoder for callback query values (`+`, `%XX`).
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

/// A parsed + state-validated callback query — success xor error xor
/// invalid (CSRF/malformed). Public for the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedCallback {
    Ok {
        code: String,
        org_id: Option<String>,
    },
    /// Upstream signalled a user-cancellable error (the `error` value).
    Denied(String),
    /// Missing/unusable params or a `state` we didn't issue.
    Invalid(String),
}

/// Parse a callback query string (`code=…&state=…[&org_id=…]` or
/// `error=…&state=…`), validating `state` matches what we issued.
/// The path is handled by axum routing, so we only take the query.
pub fn parse_callback(query: &str, expected_state: &str) -> ParsedCallback {
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
            _ => {}
        }
    }
    let Some(state) = state else {
        return ParsedCallback::Invalid("missing state".to_string());
    };
    if state != expected_state {
        // CSRF: refuse anything carrying a state we never issued,
        // without inspecting `code` — even on a claimed error.
        return ParsedCallback::Invalid("state mismatch".to_string());
    }
    if let Some(err) = error {
        return ParsedCallback::Denied(err);
    }
    let Some(code) = code else {
        return ParsedCallback::Invalid("missing code".to_string());
    };
    ParsedCallback::Ok { code, org_id }
}

// ── token exchange (copied from smooth-cli auth::browser_login) ─────

/// Token-exchange response from `POST /api/token`. Extra fields ignored.
#[derive(Debug, Clone, serde::Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    org_id: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

/// Exchange the authorization code for tokens, presenting the original
/// PKCE verifier + the same redirect_uri we advertised.
async fn exchange_code(http: &reqwest::Client, token_url: &str, code: &str, verifier: &str, redirect_uri: &str) -> anyhow::Result<TokenExchangeResponse> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ];
    let resp = http.post(token_url).form(&form).send().await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("token exchange returned HTTP {status}: {text}");
    }
    Ok(serde_json::from_str::<TokenExchangeResponse>(&text)?)
}

// ── device authorization grant (RFC 8628) ──────────────────────────
//
// Additive to the redirect flow above: lets a browser on the tailnet
// sign the daemon into a Smoo org with no redirect_uri. The UI POSTs
// `/auth/device/start`; we ask smoo.ai for a device+user code, hand the
// user code back to the browser, and poll the token endpoint in the
// background until the user approves. `/api/auth/status` (already polled
// by the UI every 5s) flips to logged-in once the loop persists creds —
// so no separate status route is needed. Pearl th-ea7b54.

/// smoo.ai's device-authorization response. Extra fields ignored.
#[derive(Debug, Clone, serde::Deserialize)]
struct DeviceAuthResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default = "default_poll_interval")]
    interval: u64,
}

/// RFC 8628 §3.2 default poll interval when the server omits one.
fn default_poll_interval() -> u64 {
    5
}

/// What the UI gets back from `/auth/device/start`. NEVER carries the
/// `device_code` — that's the daemon's secret for polling.
#[derive(Debug, Serialize)]
struct DeviceStartResponse {
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
}

impl DeviceStartResponse {
    /// `verification_uri_complete` falls back to the bare
    /// `verification_uri` when smoo.ai doesn't embed the code.
    fn from_device_auth(d: &DeviceAuthResponse) -> Self {
        Self {
            user_code: d.user_code.clone(),
            verification_uri: d.verification_uri.clone(),
            verification_uri_complete: d.verification_uri_complete.clone().unwrap_or_else(|| d.verification_uri.clone()),
        }
    }
}

/// Outcome of one device-grant poll. `Success` boxes the token payload
/// so the enum stays small (clippy::large_enum_variant).
#[derive(Debug)]
enum DevicePollOutcome {
    Success(Box<TokenExchangeResponse>),
    /// User hasn't approved yet — keep polling at the current interval.
    Pending,
    /// Server asked us to back off — widen the interval, keep polling.
    SlowDown,
    /// The device code expired before approval — terminal.
    Expired,
    /// User denied the request — terminal.
    Denied,
    /// Anything we can't safely continue on — terminal, fail closed.
    Other(String),
}

/// Map an HTTP result body to a poll outcome. Pure + total so the poll
/// state machine is unit-testable without a network. On a 2xx we parse
/// tokens; on a non-2xx we read the RFC 8628 `error` code; a parse
/// failure fails closed as `Other` rather than looping forever.
fn map_device_poll(is_success: bool, body: &str) -> DevicePollOutcome {
    #[derive(serde::Deserialize)]
    struct ErrBody {
        error: String,
    }
    if is_success {
        return match serde_json::from_str::<TokenExchangeResponse>(body) {
            Ok(tokens) => DevicePollOutcome::Success(Box::new(tokens)),
            Err(e) => DevicePollOutcome::Other(format!("could not parse token response: {e}")),
        };
    }
    match serde_json::from_str::<ErrBody>(body) {
        Ok(e) => match e.error.as_str() {
            "authorization_pending" => DevicePollOutcome::Pending,
            "slow_down" => DevicePollOutcome::SlowDown,
            "expired_token" => DevicePollOutcome::Expired,
            "access_denied" => DevicePollOutcome::Denied,
            other => DevicePollOutcome::Other(format!("device authorization error: {other}")),
        },
        Err(e) => DevicePollOutcome::Other(format!("could not parse error response: {e}")),
    }
}

/// One device-grant poll against the token endpoint. A network/transport
/// error is surfaced as `Other` (terminal) — we can't distinguish it from
/// a real failure, so fail closed rather than hammer the endpoint.
async fn device_poll_exchange(http: &reqwest::Client, token_url: &str, device_code: &str, client_id: &str) -> DevicePollOutcome {
    let form = [
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("device_code", device_code),
        ("client_id", client_id),
    ];
    let resp = match http.post(token_url).form(&form).send().await {
        Ok(r) => r,
        Err(e) => return DevicePollOutcome::Other(format!("device poll request failed: {e}")),
    };
    let is_success = resp.status().is_success();
    let body = resp.text().await.unwrap_or_default();
    map_device_poll(is_success, &body)
}

/// Background poll loop: sleep `interval`, poll, react, repeat until the
/// user approves (persist creds), a terminal outcome, or `expires_in`.
/// Never returns anything — logs and stops. NEVER logs the device_code.
async fn run_device_poll(http: reqwest::Client, device_code: String, interval: u64, expires_in: u64) {
    let client_id = device_client_id();
    let token_url = cli_token_url();
    let deadline = Instant::now() + Duration::from_secs(expires_in);
    let mut interval = interval.max(1);
    loop {
        if Instant::now() >= deadline {
            tracing::warn!("device login: code expired before approval");
            return;
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
        match device_poll_exchange(&http, &token_url, &device_code, &client_id).await {
            DevicePollOutcome::Success(tokens) => {
                match persist_credentials(&tokens, tokens.org_id.clone()) {
                    Ok(who) => tracing::info!(user = %who, "device login: signed in"),
                    Err(e) => tracing::error!(error = %e, "device login: signed in but saving the session failed"),
                }
                return;
            }
            DevicePollOutcome::Pending => {}
            DevicePollOutcome::SlowDown => interval += 5,
            DevicePollOutcome::Expired => {
                tracing::warn!("device login: token expired");
                return;
            }
            DevicePollOutcome::Denied => {
                tracing::info!("device login: user denied the request");
                return;
            }
            DevicePollOutcome::Other(msg) => {
                tracing::warn!(reason = %msg, "device login: stopping poll");
                return;
            }
        }
    }
}

/// `POST /auth/device/start` — ask smoo.ai for a device+user code, spawn
/// the background poll loop, and hand the browser the user-facing bits.
/// The `device_code` never leaves the daemon.
///
/// Guarded two ways (th-ea7b54): a same-origin check (the routes are
/// otherwise ungated, so a CSRF page in the user's browser or a random
/// tailnet peer must not be able to trigger a login), and a bounded
/// [`Semaphore`] cap so an accepted caller can't spawn unbounded 900s poll
/// loops.
async fn device_start_handler(State(state): State<AuthState>, headers: HeaderMap) -> Response {
    // CSRF/SSRF guard: reject anything that isn't the daemon's own SPA.
    if !is_same_origin(&headers) {
        tracing::warn!("device start: rejected cross-origin / unattributable request");
        return device_start_json_error(StatusCode::FORBIDDEN, "forbidden");
    }
    // Cap concurrent in-flight logins. Acquire the slot BEFORE the smoo.ai
    // round-trip so it also bounds the request fan-out; the permit is held
    // as a local (released on every early-return error path) and only moved
    // into the poll task on success.
    let permit = match Arc::clone(&state.device_slots).try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            tracing::warn!("device start: too many pending logins in flight");
            return device_start_json_error(StatusCode::TOO_MANY_REQUESTS, "too_many_pending_logins");
        }
    };

    let client_id = device_client_id();
    let form = [("client_id", client_id.as_str())];
    let resp = match state.http.post(cli_device_url()).form(&form).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "device start: request to smoo.ai failed");
            return device_start_error("Could not reach Smoo AI to start sign-in. Please try again.");
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        tracing::warn!(%status, "device start: smoo.ai returned non-success");
        return device_start_error("Smoo AI could not start the sign-in. Please try again.");
    }
    let body = resp.text().await.unwrap_or_default();
    let dev = match serde_json::from_str::<DeviceAuthResponse>(&body) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "device start: could not parse device-auth response");
            return device_start_error("Smoo AI sent an unexpected response. Please try again.");
        }
    };

    let ui = DeviceStartResponse::from_device_auth(&dev);

    // Poll in the background; the UI learns success via /api/auth/status.
    // The permit rides along and is dropped when the task ends (success,
    // expiry, or error), releasing the slot on ALL exit paths.
    let http = state.http.clone();
    tokio::spawn(async move {
        let _permit = permit;
        run_device_poll(http, dev.device_code, dev.interval, dev.expires_in).await;
    });

    (StatusCode::OK, [(axum::http::header::CACHE_CONTROL, "no-store")], Json(ui)).into_response()
}

/// Same-origin guard for the browser-triggered device-login POST. Not full
/// auth: the SPA's own `fetch` carries an `Origin` (or, failing that,
/// `Referer`) whose authority must equal the daemon's own `Host`. A
/// malicious page in the user's browser gets a cross-origin `Origin` →
/// rejected; a request that carries neither header can't be shown
/// same-origin → rejected (fail closed).
///
// ponytail: origin/host-match only — a raw tailnet peer with curl can still
// forge these headers. The concurrency cap bounds the blast radius; if the
// auth routes ever need real per-request auth, gate them behind the
// LocalTokenVerifier like `/ws`.
fn is_same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(axum::http::header::HOST).and_then(|v| v.to_str().ok()).map(str::trim) else {
        return false;
    };
    if host.is_empty() {
        return false;
    }
    let source = headers
        .get(axum::http::header::ORIGIN)
        .or_else(|| headers.get(axum::http::header::REFERER))
        .and_then(|v| v.to_str().ok());
    let Some(source) = source else {
        return false;
    };
    authority_of(source).is_some_and(|a| a == host)
}

/// Extract the `host[:port]` authority from a URL like
/// `https://host:8443/path`. Minimal — just enough to compare against the
/// `Host` header (the authority ends at the first `/`, `?`, or `#`).
fn authority_of(url: &str) -> Option<&str> {
    let after_scheme = url.split_once("://")?.1;
    let end = after_scheme.find(['/', '?', '#']).unwrap_or(after_scheme.len());
    let authority = &after_scheme[..end];
    (!authority.is_empty()).then_some(authority)
}

/// A JSON error body + 502 for the start route (the UI shows a retry).
fn device_start_error(message: &str) -> Response {
    device_start_json_error(StatusCode::BAD_GATEWAY, message)
}

/// A `{ "error": message }` JSON body at `status`.
fn device_start_json_error(status: StatusCode, message: &str) -> Response {
    #[derive(Serialize)]
    struct Err<'a> {
        error: &'a str,
    }
    (status, Json(Err { error: message })).into_response()
}

// ── redirect_uri derivation ─────────────────────────────────────────

/// Derive the daemon's own public callback URL from the incoming
/// request headers. Scheme comes from `X-Forwarded-Proto` (tailscale
/// serve / any reverse proxy sets it) defaulting to `http`; host from
/// the `Host` header. Returns `None` if there's no `Host` to build on.
fn derive_redirect_uri(headers: &HeaderMap) -> Option<String> {
    let host = headers.get("host")?.to_str().ok()?.trim();
    if host.is_empty() {
        return None;
    }
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        // A proxy may send "https, http"; take the first hop.
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("http");
    Some(format!("{scheme}://{host}/auth/callback"))
}

// ── handlers ────────────────────────────────────────────────────────

/// `GET /auth/login` — mint PKCE + state, stash the verifier, 302 to
/// smoo.ai's `/cli-login`.
async fn login_handler(State(state): State<AuthState>, headers: HeaderMap) -> Response {
    let Some(redirect_uri) = derive_redirect_uri(&headers) else {
        return (StatusCode::BAD_REQUEST, "Missing Host header — cannot derive the sign-in callback URL.").into_response();
    };
    let pair = PkcePair::generate();
    let csrf = random_state();
    state.pending_logins.insert(csrf.clone(), pair.verifier, redirect_uri.clone());
    let authorize_url = build_authorize_url(&cli_login_url(), &redirect_uri, &csrf, &pair.challenge);
    // Explicit 302 (Found). A GET→GET redirect; the browser follows it to
    // smoo.ai's sign-in page. `no-store` so no proxy/browser caches the
    // redirect — each carries a fresh single-use state.
    (
        StatusCode::FOUND,
        [
            (axum::http::header::LOCATION, authorize_url),
            (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
        ],
    )
        .into_response()
}

/// `GET /auth/callback` — validate state, exchange the code, persist the
/// user session, render a success/failure HTML page.
async fn callback_handler(State(state): State<AuthState>, RawQuery(query): RawQuery) -> Response {
    let query = query.unwrap_or_default();

    // We don't know expected_state yet — extract state, take the pending
    // entry (single-use), then validate the full query against it.
    let Some(callback_state) = extract_param(&query, "state") else {
        return html_error("Sign-in link was malformed (no state). Please try again.");
    };
    let Some(pending) = state.pending_logins.take_valid(&callback_state) else {
        // Unknown or expired state → CSRF / stale tab. Fail closed.
        return html_error("This sign-in link has expired or is invalid. Start again from the sign-in button.");
    };

    match parse_callback(&query, &callback_state) {
        ParsedCallback::Denied(reason) => html_page(
            StatusCode::BAD_REQUEST,
            "Sign-in cancelled",
            &format!("Sign-in was cancelled ({}). You can close this tab and try again.", html_escape(&reason)),
        ),
        ParsedCallback::Invalid(msg) => html_error(&format!("Sign-in could not be completed ({}).", html_escape(&msg))),
        ParsedCallback::Ok { code, org_id } => {
            match exchange_code(&state.http, &cli_token_url(), &code, &pending.verifier, &pending.redirect_uri).await {
                Ok(tokens) => match persist_credentials(&tokens, org_id) {
                    Ok(who) => html_page(
                        StatusCode::OK,
                        "Signed in",
                        &format!("Signed in as {}. You can close this tab and return to the terminal.", html_escape(&who)),
                    ),
                    Err(e) => {
                        tracing::error!(error = %e, "auth callback: failed to persist credentials");
                        html_error("Signed in with Smoo AI, but saving the session failed. Check the daemon logs.")
                    }
                },
                Err(e) => {
                    // Log without the code/verifier — the message itself
                    // is the upstream body, which won't contain secrets.
                    tracing::warn!(error = %e, "auth callback: token exchange failed");
                    html_error("Could not complete sign-in with Smoo AI. Please try again.")
                }
            }
        }
    }
}

/// Persist a user `Credentials` from a token-exchange response, exactly
/// as `th auth login`'s browser flow does. Returns the display name.
fn persist_credentials(tokens: &TokenExchangeResponse, callback_org_id: Option<String>) -> anyhow::Result<String> {
    use chrono::{Duration as ChronoDuration, Utc};
    let expires_at = tokens.expires_in.map(|secs| {
        let secs_i64 = i64::try_from(secs).unwrap_or(i64::MAX);
        Utc::now() + ChronoDuration::seconds(secs_i64)
    });
    // Prefer the org the user picked in the browser (callback), else the
    // token response's org.
    let org_id = callback_org_id.or_else(|| tokens.org_id.clone());
    let creds = Credentials {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        expires_at,
        user: tokens.email.clone().or_else(|| tokens.user.clone()),
        active_org_id: org_id,
        client_id: None,
        client_secret: None,
        kind: CredentialKind::User,
        created_at: Utc::now(),
    };
    let who = creds.user.clone().unwrap_or_else(|| "(unknown user)".to_string());
    let store = CredentialsStore::default_user()?;
    store.save(&creds)?;
    Ok(who)
}

#[derive(Serialize)]
pub struct AuthStatus {
    /// `true` only when a session exists **and** its access token is still
    /// usable. A present-but-expired file is NOT logged in — reporting it
    /// as such is the bug this replaces (th-cbf613).
    #[serde(rename = "loggedIn")]
    logged_in: bool,
    user: Option<String>,
    #[serde(rename = "orgId")]
    org_id: Option<String>,
    /// Access-token expiry, so the UI can show how much runway is left.
    #[serde(rename = "expiresAt", skip_serializing_if = "Option::is_none")]
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Inside the 5-minute pre-expiry window but not dead yet — the
    /// heartbeat should be renewing it about now.
    stale: bool,
    /// Session on disk but past expiry: the heartbeat couldn't renew it
    /// (revoked/expired refresh token) and the operator must sign in again.
    expired: bool,
}

/// Derive the reported status from whatever is on disk. Pure so the
/// expiry logic is testable without a filesystem or a clock injection.
fn auth_status(creds: Option<Credentials>) -> AuthStatus {
    let Some(c) = creds else {
        return AuthStatus {
            logged_in: false,
            user: None,
            org_id: None,
            expires_at: None,
            stale: false,
            expired: false,
        };
    };
    let expired = c.is_expired();
    let stale = !expired && refresh::should_refresh(&c);
    AuthStatus {
        logged_in: !expired,
        // Keep identity even when expired so the UI can say *who* needs
        // to sign in again.
        user: c.user,
        org_id: c.active_org_id,
        expires_at: c.expires_at,
        stale,
        expired,
    }
}

/// `GET /api/auth/status` — report whether `th` currently has a *usable*
/// user session. Never errors on a missing file (that's logged-out).
async fn status_handler() -> Json<AuthStatus> {
    Json(auth_status(CredentialsStore::default_user().ok().and_then(|s| s.load().ok().flatten())))
}

// ── credential heartbeat (th-cbf613) ────────────────────────────────

/// How often the heartbeat re-reads the credentials file. `should_refresh`
/// opens a 5-minute pre-expiry window, so a 60s tick gets ~5 attempts
/// inside it — a transient Supabase blip retries a minute later and still
/// lands before the token dies. A tick that has nothing to do is one small
/// file read, so the idle cost doesn't argue for a longer interval.
#[allow(clippy::duration_suboptimal_units)]
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// What a single heartbeat tick should do, given the credentials on disk.
/// Split out from the I/O so every branch is unit-testable without
/// touching Supabase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeartbeatAction {
    /// No session on disk — nothing to keep alive.
    NotLoggedIn,
    /// Not a user session (M2M `client_credentials` has no refresh token;
    /// it must be re-minted from client_id/secret — out of scope here).
    NotUserSession,
    /// Token has plenty of runway; do nothing.
    Fresh,
    /// Due for renewal but carries no refresh token — a human must sign
    /// in again. Visible, not silent.
    NoRefreshToken,
    /// Inside the refresh window: exchange the refresh token.
    Refresh,
}

fn heartbeat_action(creds: Option<&Credentials>) -> HeartbeatAction {
    let Some(c) = creds else { return HeartbeatAction::NotLoggedIn };
    if c.kind != CredentialKind::User {
        return HeartbeatAction::NotUserSession;
    }
    if !refresh::should_refresh(c) {
        return HeartbeatAction::Fresh;
    }
    if c.refresh_token.is_none() {
        return HeartbeatAction::NoRefreshToken;
    }
    HeartbeatAction::Refresh
}

/// One heartbeat tick against an explicit store + Supabase endpoint.
/// Returns the action it took so the caller can log only on transitions.
/// Errors are returned, never swallowed. Endpoints are parameters (not
/// read from env in here) so tests can point at a dead port without
/// racing other tests over a process-global env var.
async fn heartbeat_tick(http: &reqwest::Client, store: &CredentialsStore, supabase_url: &str, anon_key: &str) -> anyhow::Result<HeartbeatAction> {
    let creds = store.load()?;
    let action = heartbeat_action(creds.as_ref());
    if action == HeartbeatAction::Refresh {
        let previous = creds.expect("Refresh implies credentials were loaded");
        let renewed = refresh::refresh_session(http, supabase_url, anon_key, &previous).await?;
        // Supabase ROTATES the refresh token — persisting is mandatory,
        // not an optimization: skip it and the next tick presents a
        // revoked token and the session dies.
        store.save(&renewed)?;
        tracing::info!(expires_at = ?renewed.expires_at, "credential heartbeat: session renewed");
    }
    Ok(action)
}

/// Spawn the background credential heartbeat: every [`HEARTBEAT_INTERVAL`],
/// renew the user session if it's inside the pre-expiry window.
///
/// Without this the daemon holds a ~1h token forever and every
/// `api.smoo.ai` call 401s until a human re-runs sign-in. Failures are
/// logged at warn/error and surface to the UI via `/api/auth/status`
/// (`expired: true`) — a heartbeat that fails quietly is worse than none.
pub fn spawn_credential_heartbeat() {
    tokio::spawn(async move {
        let http = reqwest::Client::default();
        // Only log on a change of state, so a long-idle daemon doesn't
        // emit the same line every minute for hours.
        let mut last: Option<HeartbeatAction> = None;
        let mut last_error: Option<String> = None;
        loop {
            let store = match CredentialsStore::default_user() {
                Ok(store) => store,
                Err(e) => {
                    tracing::error!(error = %e, "credential heartbeat: cannot locate the credentials store — session will not be renewed");
                    return;
                }
            };
            match heartbeat_tick(&http, &store, &supabase_url(), &supabase_anon_key()).await {
                Ok(action) => {
                    last_error = None;
                    if last != Some(action) {
                        match action {
                            HeartbeatAction::NoRefreshToken => {
                                tracing::warn!("credential heartbeat: session is expiring and has no refresh token — sign in again")
                            }
                            HeartbeatAction::NotUserSession => tracing::warn!("credential heartbeat: stored session is not a user session — not renewing"),
                            HeartbeatAction::NotLoggedIn | HeartbeatAction::Fresh | HeartbeatAction::Refresh => {
                                tracing::debug!(?action, "credential heartbeat");
                            }
                        }
                        last = Some(action);
                    }
                }
                Err(e) => {
                    // Never log token material — this is the upstream
                    // status/message plus our own context, no secrets.
                    let msg = format!("{e:#}");
                    if last_error.as_ref() != Some(&msg) {
                        tracing::error!(error = %msg, "credential heartbeat: session renewal FAILED — sign in again (`/api/auth/status` now reports expired)");
                        last_error = Some(msg);
                    }
                    last = None;
                }
            }
            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
        }
    });
}

// ── small HTML helpers ──────────────────────────────────────────────

/// Pull a single (percent-decoded) query param out of a raw query
/// string. Used only to read `state` before we know what to validate.
fn extract_param(query: &str, key: &str) -> Option<String> {
    query.split('&').filter(|s| !s.is_empty()).find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then(|| percent_decode(v))
    })
}

/// Minimal HTML-escape for the handful of values we interpolate into
/// the response pages (upstream error strings). Defense in depth — these
/// come from our own state machine, but treat them as untrusted.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn html_page(status: StatusCode, title: &str, body: &str) -> Response {
    let page = format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>{title} — Smooth</title></head>
<body style="font-family: system-ui, sans-serif; padding: 2rem; max-width: 32rem; margin: 0 auto;">
<h2>{title}</h2>
<p>{body}</p>
</body></html>"#,
        title = html_escape(title),
        body = body,
    );
    (status, [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")], page).into_response()
}

fn html_error(body: &str) -> Response {
    html_page(StatusCode::BAD_REQUEST, "Sign-in failed", body)
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn hdrs(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn redirect_uri_defaults_scheme_to_http() {
        let h = hdrs(&[("host", "smoo-hub:8443")]);
        assert_eq!(derive_redirect_uri(&h).as_deref(), Some("http://smoo-hub:8443/auth/callback"));
    }

    #[test]
    fn redirect_uri_honors_forwarded_proto() {
        let h = hdrs(&[("host", "smooth.example.ts.net"), ("x-forwarded-proto", "https")]);
        assert_eq!(derive_redirect_uri(&h).as_deref(), Some("https://smooth.example.ts.net/auth/callback"));
    }

    #[test]
    fn redirect_uri_takes_first_forwarded_proto_hop() {
        let h = hdrs(&[("host", "h"), ("x-forwarded-proto", "https, http")]);
        assert_eq!(derive_redirect_uri(&h).as_deref(), Some("https://h/auth/callback"));
    }

    #[test]
    fn redirect_uri_none_without_host() {
        assert!(derive_redirect_uri(&HeaderMap::new()).is_none());
    }

    #[test]
    fn pending_insert_and_single_use_take() {
        let store = PendingLogins::new();
        store.insert("st8".into(), "verf".into(), "http://h/auth/callback".into());
        let p = store.take_valid("st8").expect("present");
        assert_eq!(p.verifier, "verf");
        assert_eq!(p.redirect_uri, "http://h/auth/callback");
        // Single-use: a second take finds nothing.
        assert!(store.take_valid("st8").is_none());
    }

    #[test]
    fn pending_take_unknown_state_is_none() {
        let store = PendingLogins::new();
        store.insert("known".into(), "v".into(), "r".into());
        assert!(store.take_valid("never-issued").is_none());
    }

    #[test]
    fn pending_expired_entry_is_rejected_and_removed() {
        let store = PendingLogins::new();
        {
            let mut map = store.0.lock().unwrap();
            map.insert(
                "old".into(),
                Pending {
                    verifier: "v".into(),
                    redirect_uri: "r".into(),
                    created: Instant::now() - (PENDING_TTL + Duration::from_secs(1)),
                },
            );
        }
        // Expired → take_valid returns None (and has removed it).
        assert!(store.take_valid("old").is_none());
        assert!(store.0.lock().unwrap().is_empty(), "expired entry should be removed on take");
    }

    #[test]
    fn insert_prunes_expired_entries() {
        let store = PendingLogins::new();
        {
            let mut map = store.0.lock().unwrap();
            map.insert(
                "stale".into(),
                Pending {
                    verifier: "v".into(),
                    redirect_uri: "r".into(),
                    created: Instant::now() - (PENDING_TTL + Duration::from_secs(1)),
                },
            );
        }
        store.insert("fresh".into(), "v2".into(), "r2".into());
        let map = store.0.lock().unwrap();
        assert!(!map.contains_key("stale"), "insert should prune expired");
        assert!(map.contains_key("fresh"));
    }

    #[test]
    fn parse_callback_happy_path_with_org() {
        let r = parse_callback("code=abc&state=S&org_id=org_9", "S");
        assert_eq!(
            r,
            ParsedCallback::Ok {
                code: "abc".into(),
                org_id: Some("org_9".into())
            }
        );
    }

    #[test]
    fn parse_callback_happy_path_without_org() {
        let r = parse_callback("code=abc&state=S", "S");
        assert_eq!(
            r,
            ParsedCallback::Ok {
                code: "abc".into(),
                org_id: None
            }
        );
    }

    #[test]
    fn parse_callback_state_mismatch_is_invalid() {
        let r = parse_callback("code=abc&state=wrong", "S");
        assert_eq!(r, ParsedCallback::Invalid("state mismatch".into()));
    }

    #[test]
    fn parse_callback_missing_state_is_invalid() {
        let r = parse_callback("code=abc", "S");
        assert_eq!(r, ParsedCallback::Invalid("missing state".into()));
    }

    #[test]
    fn parse_callback_missing_code_is_invalid() {
        let r = parse_callback("state=S", "S");
        assert_eq!(r, ParsedCallback::Invalid("missing code".into()));
    }

    #[test]
    fn parse_callback_access_denied() {
        let r = parse_callback("error=access_denied&state=S", "S");
        assert_eq!(r, ParsedCallback::Denied("access_denied".into()));
    }

    #[test]
    fn parse_callback_denied_with_wrong_state_is_invalid() {
        // A forged "denied" carrying a state we never issued is refused,
        // not surfaced as a cancellation.
        let r = parse_callback("error=access_denied&state=other", "S");
        assert_eq!(r, ParsedCallback::Invalid("state mismatch".into()));
    }

    #[test]
    fn parse_callback_percent_decodes_org_id() {
        let r = parse_callback("code=abc&state=S&org_id=org%2Fslash", "S");
        assert_eq!(
            r,
            ParsedCallback::Ok {
                code: "abc".into(),
                org_id: Some("org/slash".into())
            }
        );
    }

    #[test]
    fn extract_param_reads_state() {
        assert_eq!(extract_param("code=a&state=xyz", "state").as_deref(), Some("xyz"));
        assert_eq!(extract_param("code=a", "state"), None);
    }

    #[test]
    fn build_authorize_url_percent_encodes_redirect() {
        let url = build_authorize_url("https://smoo.ai/cli-login", "https://h:8443/auth/callback", "ST", "CH");
        assert!(url.starts_with("https://smoo.ai/cli-login?"), "got {url}");
        assert!(url.contains("state=ST"));
        assert!(url.contains("code_challenge=CH"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fh%3A8443%2Fauth%2Fcallback"), "got {url}");
    }

    #[test]
    fn derive_challenge_matches_rfc7636_vector() {
        // RFC 7636 §A.1 reference vector.
        assert_eq!(
            derive_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_pkce_pair_roundtrips() {
        let p = PkcePair::generate();
        assert_eq!(p.verifier.len(), 43);
        assert_eq!(derive_challenge(&p.verifier), p.challenge);
    }

    #[test]
    fn html_escape_neutralizes_markup() {
        assert_eq!(html_escape("<script>&\"</script>"), "&lt;script&gt;&amp;&quot;&lt;/script&gt;");
    }

    // ── device authorization grant (th-ea7b54) ──────────────────────

    #[test]
    fn device_auth_response_parses_full() {
        let json = r#"{
            "device_code": "dc-secret",
            "user_code": "WXYZ-1234",
            "verification_uri": "https://smoo.ai/device",
            "verification_uri_complete": "https://smoo.ai/device?code=WXYZ-1234",
            "expires_in": 900,
            "interval": 7
        }"#;
        let d: DeviceAuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(d.device_code, "dc-secret");
        assert_eq!(d.user_code, "WXYZ-1234");
        assert_eq!(d.verification_uri, "https://smoo.ai/device");
        assert_eq!(d.verification_uri_complete.as_deref(), Some("https://smoo.ai/device?code=WXYZ-1234"));
        assert_eq!(d.expires_in, 900);
        assert_eq!(d.interval, 7);
    }

    #[test]
    fn device_auth_response_defaults_interval() {
        // No `interval`, no `verification_uri_complete` → RFC defaults.
        let json = r#"{"device_code":"dc","user_code":"UC","verification_uri":"https://smoo.ai/device","expires_in":600}"#;
        let d: DeviceAuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(d.interval, 5);
        assert!(d.verification_uri_complete.is_none());
    }

    #[test]
    fn device_start_response_hides_device_code_and_falls_back_uri() {
        let d = DeviceAuthResponse {
            device_code: "SUPER-SECRET-dc".into(),
            user_code: "UC-42".into(),
            verification_uri: "https://smoo.ai/device".into(),
            verification_uri_complete: None,
            expires_in: 600,
            interval: 5,
        };
        let ui = DeviceStartResponse::from_device_auth(&d);
        // Missing complete URI falls back to the bare one.
        assert_eq!(ui.verification_uri_complete, "https://smoo.ai/device");
        let body = serde_json::to_string(&ui).unwrap();
        assert!(body.contains("UC-42"), "user_code should be present: {body}");
        assert!(!body.contains("SUPER-SECRET-dc"), "device_code must NOT leak to the browser: {body}");
        assert!(!body.to_lowercase().contains("device_code"), "no device_code field: {body}");
    }

    #[test]
    fn map_device_poll_pending() {
        assert!(matches!(
            map_device_poll(false, r#"{"error":"authorization_pending"}"#),
            DevicePollOutcome::Pending
        ));
    }

    #[test]
    fn map_device_poll_slow_down() {
        assert!(matches!(map_device_poll(false, r#"{"error":"slow_down"}"#), DevicePollOutcome::SlowDown));
    }

    #[test]
    fn map_device_poll_expired() {
        assert!(matches!(map_device_poll(false, r#"{"error":"expired_token"}"#), DevicePollOutcome::Expired));
    }

    #[test]
    fn map_device_poll_denied() {
        assert!(matches!(map_device_poll(false, r#"{"error":"access_denied"}"#), DevicePollOutcome::Denied));
    }

    #[test]
    fn map_device_poll_unknown_error_is_other() {
        assert!(matches!(map_device_poll(false, r#"{"error":"teapot"}"#), DevicePollOutcome::Other(_)));
    }

    #[test]
    fn map_device_poll_unparseable_error_fails_closed() {
        // A non-2xx body we can't parse must be terminal, not a loop.
        assert!(matches!(map_device_poll(false, "not json"), DevicePollOutcome::Other(_)));
    }

    #[test]
    fn map_device_poll_success_parses_tokens() {
        let body = r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600,"org_id":"org_1","email":"a@b.co"}"#;
        match map_device_poll(true, body) {
            DevicePollOutcome::Success(t) => {
                assert_eq!(t.access_token, "AT");
                assert_eq!(t.org_id.as_deref(), Some("org_1"));
                assert_eq!(t.email.as_deref(), Some("a@b.co"));
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn map_device_poll_success_with_bad_body_fails_closed() {
        assert!(matches!(map_device_poll(true, "{}"), DevicePollOutcome::Other(_)));
    }

    // ── security hardening: origin guard + concurrency cap (th-ea7b54) ──

    #[test]
    fn same_origin_accepts_matching_origin() {
        let h = hdrs(&[("host", "smoo-hub:8443"), ("origin", "https://smoo-hub:8443")]);
        assert!(is_same_origin(&h));
    }

    #[test]
    fn same_origin_accepts_matching_referer_when_no_origin() {
        let h = hdrs(&[("host", "smoo-hub:8443"), ("referer", "https://smoo-hub:8443/auth")]);
        assert!(is_same_origin(&h));
    }

    #[test]
    fn same_origin_rejects_cross_origin() {
        // A malicious page in the user's browser: its Origin is its own host.
        let h = hdrs(&[("host", "smoo-hub:8443"), ("origin", "https://evil.example")]);
        assert!(!is_same_origin(&h));
    }

    #[test]
    fn same_origin_rejects_cross_origin_referer() {
        let h = hdrs(&[("host", "smoo-hub:8443"), ("referer", "https://evil.example/x")]);
        assert!(!is_same_origin(&h));
    }

    #[test]
    fn same_origin_rejects_when_no_origin_or_referer() {
        // Fail closed: nothing proves same-origin (e.g. a raw curl).
        let h = hdrs(&[("host", "smoo-hub:8443")]);
        assert!(!is_same_origin(&h));
    }

    #[test]
    fn same_origin_rejects_without_host() {
        let h = hdrs(&[("origin", "https://smoo-hub:8443")]);
        assert!(!is_same_origin(&h));
    }

    #[test]
    fn same_origin_origin_wins_over_referer() {
        // Present-but-mismatched Origin is used even if Referer would match —
        // we can't downgrade to the weaker signal.
        let h = hdrs(&[
            ("host", "smoo-hub:8443"),
            ("origin", "https://evil.example"),
            ("referer", "https://smoo-hub:8443/x"),
        ]);
        assert!(!is_same_origin(&h));
    }

    #[test]
    fn authority_of_parses_host_port() {
        assert_eq!(authority_of("https://h:8443/auth/callback"), Some("h:8443"));
        assert_eq!(authority_of("http://host"), Some("host"));
        assert_eq!(authority_of("https://host?q=1"), Some("host"));
        assert_eq!(authority_of("https://host#frag"), Some("host"));
        assert_eq!(authority_of("not-a-url"), None);
        assert_eq!(authority_of("https://"), None);
    }

    #[test]
    fn device_slots_cap_blocks_when_full_and_releases_on_drop() {
        // Mirrors the handler's guard: hold MAX permits, next acquire fails,
        // dropping one frees a slot again.
        let sem = Arc::new(Semaphore::new(MAX_PENDING_DEVICE_LOGINS));
        let mut held = Vec::new();
        for _ in 0..MAX_PENDING_DEVICE_LOGINS {
            held.push(Arc::clone(&sem).try_acquire_owned().expect("under cap"));
        }
        // Over cap → no slot (the handler returns 429 here).
        assert!(Arc::clone(&sem).try_acquire_owned().is_err(), "should be at cap");
        // A finished poll loop drops its permit → slot freed.
        held.pop();
        assert!(Arc::clone(&sem).try_acquire_owned().is_ok(), "slot released on drop");
    }

    #[test]
    fn device_client_id_and_url_defaults_and_overrides() {
        // Defaults when unset.
        std::env::remove_var("SMOOAI_DEVICE_CLIENT_ID");
        std::env::remove_var("SMOOAI_CLI_DEVICE_URL");
        assert_eq!(device_client_id(), "bigsmooth-daemon");
        assert_eq!(cli_device_url(), "https://smoo.ai/api/device/code");
        // Env override wins.
        std::env::set_var("SMOOAI_DEVICE_CLIENT_ID", "custom-client");
        std::env::set_var("SMOOAI_CLI_DEVICE_URL", "https://example.test/device");
        assert_eq!(device_client_id(), "custom-client");
        assert_eq!(cli_device_url(), "https://example.test/device");
        std::env::remove_var("SMOOAI_DEVICE_CLIENT_ID");
        std::env::remove_var("SMOOAI_CLI_DEVICE_URL");
    }

    // ── status expiry + heartbeat (th-cbf613) ───────────────────────

    /// A user session expiring `mins` from now (negative = already past).
    fn user_creds(mins: i64, refresh_token: Option<&str>) -> Credentials {
        Credentials {
            access_token: "tok".into(),
            refresh_token: refresh_token.map(str::to_string),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::minutes(mins)),
            user: Some("brent@smoo.ai".into()),
            active_org_id: Some("org_abc".into()),
            client_id: None,
            client_secret: None,
            kind: CredentialKind::User,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn status_logged_out_when_no_credentials() {
        let s = auth_status(None);
        assert!(!s.logged_in);
        assert!(!s.stale && !s.expired);
        assert!(s.user.is_none() && s.expires_at.is_none());
    }

    #[test]
    fn status_fresh_session_is_logged_in_and_not_stale() {
        let s = auth_status(Some(user_creds(45, Some("rtok"))));
        assert!(s.logged_in);
        assert!(!s.stale, "45 min of runway is outside the 5-min window");
        assert!(!s.expired);
        assert_eq!(s.user.as_deref(), Some("brent@smoo.ai"));
        assert_eq!(s.org_id.as_deref(), Some("org_abc"));
        assert!(s.expires_at.is_some());
    }

    #[test]
    fn status_near_expiry_is_stale_but_still_logged_in() {
        // Inside `should_refresh`'s 5-min window, outside `is_expired`'s 60s.
        let s = auth_status(Some(user_creds(3, Some("rtok"))));
        assert!(s.logged_in, "3 min of runway is still usable");
        assert!(s.stale);
        assert!(!s.expired);
    }

    #[test]
    fn status_expired_session_is_not_logged_in() {
        // The bug this fixes: the file exists, so the old handler said
        // `loggedIn: true` while holding a dead token.
        let s = auth_status(Some(user_creds(-30, Some("rtok"))));
        assert!(!s.logged_in);
        assert!(s.expired);
        assert!(!s.stale, "expired is reported as expired, not stale");
        assert_eq!(s.user.as_deref(), Some("brent@smoo.ai"), "identity kept so the UI can say who must re-auth");
    }

    #[test]
    fn status_without_expiry_is_logged_in() {
        // No `expires_at` → we can't prove it's dead; report usable (this
        // matches `is_expired`/`should_refresh`, which both say "no").
        let mut c = user_creds(0, Some("rtok"));
        c.expires_at = None;
        let s = auth_status(Some(c));
        assert!(s.logged_in);
        assert!(!s.stale && !s.expired);
        assert!(s.expires_at.is_none());
    }

    #[test]
    fn heartbeat_skips_when_logged_out() {
        assert_eq!(heartbeat_action(None), HeartbeatAction::NotLoggedIn);
    }

    #[test]
    fn heartbeat_skips_fresh_session() {
        assert_eq!(heartbeat_action(Some(&user_creds(45, Some("rtok")))), HeartbeatAction::Fresh);
    }

    #[test]
    fn heartbeat_refreshes_inside_the_window() {
        assert_eq!(heartbeat_action(Some(&user_creds(3, Some("rtok")))), HeartbeatAction::Refresh);
    }

    #[test]
    fn heartbeat_refreshes_already_expired_session() {
        // Still worth attempting — the refresh token outlives the access
        // token (30-day per-session lifetime), so this usually recovers.
        assert_eq!(heartbeat_action(Some(&user_creds(-30, Some("rtok")))), HeartbeatAction::Refresh);
    }

    #[test]
    fn heartbeat_flags_missing_refresh_token() {
        assert_eq!(heartbeat_action(Some(&user_creds(3, None))), HeartbeatAction::NoRefreshToken);
    }

    #[test]
    fn heartbeat_never_touches_m2m_credentials() {
        // client_credentials has no refresh token; re-minting it from
        // client_id/secret is a different flow and out of scope here.
        let mut c = user_creds(3, Some("rtok"));
        c.kind = CredentialKind::M2m;
        assert_eq!(heartbeat_action(Some(&c)), HeartbeatAction::NotUserSession);
    }

    #[test]
    fn supabase_endpoints_default_and_honor_env_overrides() {
        std::env::remove_var("SMOOAI_SUPABASE_URL");
        std::env::remove_var("SMOOAI_SUPABASE_ANON_KEY");
        assert_eq!(supabase_url(), "https://db.smoo.ai");
        assert!(supabase_anon_key().starts_with("eyJ"), "baked-in anon key should be a JWT");
        std::env::set_var("SMOOAI_SUPABASE_URL", "http://127.0.0.1:54331");
        std::env::set_var("SMOOAI_SUPABASE_ANON_KEY", "local-anon");
        assert_eq!(supabase_url(), "http://127.0.0.1:54331");
        assert_eq!(supabase_anon_key(), "local-anon");
        std::env::remove_var("SMOOAI_SUPABASE_URL");
        std::env::remove_var("SMOOAI_SUPABASE_ANON_KEY");
    }

    /// Discard port — nothing listens, so no test ever leaves the box.
    const UNREACHABLE_SUPABASE: &str = "http://127.0.0.1:9";

    fn store_with(dir: &std::path::Path, creds: Option<&Credentials>) -> CredentialsStore {
        let store = CredentialsStore::at(dir.join("smooai-user.json"));
        if let Some(c) = creds {
            store.save(c).unwrap();
        }
        store
    }

    #[tokio::test]
    async fn tick_on_empty_store_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(dir.path(), None);
        let action = heartbeat_tick(&reqwest::Client::default(), &store, UNREACHABLE_SUPABASE, "anon").await.unwrap();
        assert_eq!(action, HeartbeatAction::NotLoggedIn);
    }

    #[tokio::test]
    async fn tick_leaves_a_fresh_session_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(dir.path(), Some(&user_creds(45, Some("rtok"))));
        let action = heartbeat_tick(&reqwest::Client::default(), &store, UNREACHABLE_SUPABASE, "anon").await.unwrap();
        assert_eq!(action, HeartbeatAction::Fresh);
        // Untouched means untouched — same token still on disk.
        assert_eq!(store.load().unwrap().unwrap().access_token, "tok");
    }

    #[tokio::test]
    async fn tick_surfaces_a_failed_refresh_as_an_error() {
        // A failing refresh must return Err (→ logged at error, and the
        // still-stale file keeps `/api/auth/status` honest), not swallow.
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(dir.path(), Some(&user_creds(-30, Some("rtok"))));
        let result = heartbeat_tick(&reqwest::Client::default(), &store, UNREACHABLE_SUPABASE, "anon").await;
        assert!(result.is_err(), "a failed renewal must not be swallowed");
        // And the stale credentials are left alone rather than clobbered.
        assert_eq!(store.load().unwrap().unwrap().access_token, "tok");
    }
}

//! Web Push — Big Smooth pushes a notification to the user's phone (an installed
//! PWA) even when the tab is closed. The browser subscribes (POST /push/subscribe
//! with its endpoint + keys); the daemon VAPID-signs + encrypts a payload and
//! POSTs it to that endpoint (`web-push-native` builds the request, pure
//! RustCrypto, no OpenSSL — SMOODEV-4114; our rustls reqwest sends it); the service worker
//! (`public/push-sw.js`) wakes and shows the notification.
//!
//! VAPID keys come from `SMOOTH_VAPID_PUBLIC` / `SMOOTH_VAPID_PRIVATE` (raw
//! base64url, the `web-push` generate-vapid-keys format). Unset ⇒ push is
//! disabled and the routes 503, so the daemon runs fine without it.
//!
//! ponytail: subscriptions live in a JSON file (`~/.smooth/push-subs.json`), not
//! sqlite — a single-tenant daemon has a handful of devices. Move to the operator
//! DB if it ever grows past "my phones".

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use web_push_native::jwt_simple::algorithms::ES256KeyPair;
use web_push_native::{Auth, WebPushBuilder};

/// A browser push subscription (`PushSubscription.toJSON()`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Subscription {
    pub endpoint: String,
    pub keys: SubscriptionKeys,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

/// VAPID keys + the subscription store. `None` keys ⇒ push disabled.
#[derive(Clone)]
pub struct PushState {
    public_key: Option<String>,
    private_key: Option<String>,
    subs_path: PathBuf,
    // Serialize file writes; reads are cheap and rare.
    lock: Arc<Mutex<()>>,
}

impl PushState {
    pub fn from_env() -> Self {
        let (public_key, private_key) = resolve_vapid_keys();
        Self {
            public_key,
            private_key,
            subs_path: subs_path(),
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn enabled(&self) -> bool {
        self.public_key.is_some() && self.private_key.is_some()
    }

    fn load_subs(&self) -> Vec<Subscription> {
        std::fs::read(&self.subs_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    async fn save_subs(&self, subs: &[Subscription]) {
        let _g = self.lock.lock().await;
        if let Ok(json) = serde_json::to_vec_pretty(subs) {
            // Subscriptions carry each device's p256dh/auth keys — same
            // trust level as the VAPID private key they pair with.
            if let Err(e) = crate::secret_file::write_secret(&self.subs_path, json) {
                tracing::warn!(error = %e, path = %self.subs_path.display(), "could not persist push subscriptions");
            }
        }
    }

    /// Send `{title, body}` to every subscribed device. Expired subscriptions
    /// (410/404) are pruned. Best-effort: a failing endpoint never blocks the rest.
    pub async fn send_to_all(&self, title: &str, body: &str) -> usize {
        let Some(private) = self.private_key.as_deref().filter(|_| self.enabled()) else {
            return 0;
        };
        let key_pair = match vapid_key_pair(private) {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!(error = %e, "invalid VAPID private key; push disabled");
                return 0;
            }
        };
        let payload = serde_json::json!({ "title": title, "body": body }).to_string();
        let client = reqwest::Client::new();
        let subs = self.load_subs();
        let mut kept = Vec::with_capacity(subs.len());
        let mut sent = 0;
        for sub in subs {
            let req = match build_push_request(&sub, &key_pair, payload.as_bytes()) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "web-push build failed");
                    kept.push(sub);
                    continue;
                }
            };
            match send_request(&client, req).await {
                Ok(status) if status.is_success() => {
                    sent += 1;
                    kept.push(sub);
                }
                Ok(status) if is_expired_status(status) => {
                    tracing::info!(endpoint = %sub.endpoint, %status, "pruning expired push subscription");
                }
                Ok(status) => {
                    tracing::warn!(endpoint = %sub.endpoint, %status, "web-push send rejected");
                    kept.push(sub);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "web-push send failed");
                    kept.push(sub);
                }
            }
        }
        self.save_subs(&kept).await;
        sent
    }
}

/// The push service says the subscription is gone (RFC 8030 §7.3: 410; FCM
/// and Mozilla also answer 404) — drop it rather than retry forever.
fn is_expired_status(status: StatusCode) -> bool {
    status == StatusCode::GONE || status == StatusCode::NOT_FOUND
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Parse the raw base64url VAPID private scalar (the `generate-vapid-keys` format).
fn vapid_key_pair(private_b64: &str) -> Result<ES256KeyPair, BoxError> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(private_b64.trim_end_matches('='))?;
    Ok(ES256KeyPair::from_bytes(&bytes).map_err(|e| e.to_string())?)
}

/// Encrypt (RFC 8291 aes128gcm) + VAPID-sign (RFC 8292) one notification for
/// one subscription. Pure — no I/O, so it is unit-testable end to end.
fn build_push_request(sub: &Subscription, key_pair: &ES256KeyPair, payload: &[u8]) -> Result<axum::http::Request<Vec<u8>>, BoxError> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let p256dh = b64.decode(sub.keys.p256dh.trim_end_matches('='))?;
    let auth = b64.decode(sub.keys.auth.trim_end_matches('='))?;
    if auth.len() != 16 {
        return Err(format!("push auth secret must be 16 bytes, got {}", auth.len()).into());
    }
    let builder = WebPushBuilder::new(
        sub.endpoint.parse()?,
        web_push_native::p256::PublicKey::from_sec1_bytes(&p256dh)?,
        Auth::clone_from_slice(&auth),
    )
    .with_vapid(key_pair, "mailto:dev@smoo.ai");
    Ok(builder.build(payload.to_vec())?)
}

async fn send_request(client: &reqwest::Client, req: axum::http::Request<Vec<u8>>) -> Result<StatusCode, reqwest::Error> {
    let (mut parts, body) = req.into_parts();
    // reqwest derives Content-Length from the body; don't send it twice.
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    let resp = client
        .post(parts.uri.to_string())
        .headers(parts.headers)
        .body(body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;
    Ok(resp.status())
}

fn subs_path() -> PathBuf {
    // `dirs_next`, not `$HOME`: Windows sets `%USERPROFILE%`, so an env read
    // fell through to `.` and scattered a push-subs.json into whatever the cwd
    // happened to be.
    let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".smooth").join("push-subs.json")
}

/// `~/.smooth/vapid.json` — the auto-provisioned VAPID keypair (`SMOOTH_VAPID_FILE` overrides).
fn vapid_path() -> PathBuf {
    if let Ok(p) = std::env::var("SMOOTH_VAPID_FILE") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".smooth").join("vapid.json")
}

/// Persisted VAPID keypair (raw base64url, the `web-push` generate-vapid-keys format).
#[derive(Serialize, Deserialize)]
struct VapidFile {
    #[serde(rename = "publicKey")]
    public_key: String,
    #[serde(rename = "privateKey")]
    private_key: String,
}

/// Resolve the VAPID keypair for [`PushState`]. Precedence: explicit
/// `SMOOTH_VAPID_PUBLIC`/`_PRIVATE` env → the persisted `~/.smooth/vapid.json` →
/// a freshly generated pair (persisted for next time).
fn resolve_vapid_keys() -> (Option<String>, Option<String>) {
    let env_pub = std::env::var("SMOOTH_VAPID_PUBLIC").ok().filter(|s| !s.is_empty());
    let env_priv = std::env::var("SMOOTH_VAPID_PRIVATE").ok().filter(|s| !s.is_empty());
    if let (Some(pub_k), Some(priv_k)) = (env_pub, env_priv) {
        return (Some(pub_k), Some(priv_k));
    }
    let (pub_k, priv_k) = load_or_generate_vapid_at(&vapid_path());
    (Some(pub_k), Some(priv_k))
}

/// Load the persisted VAPID keypair, generating + persisting one on first run.
/// Path-injectable so tests don't touch `~/.smooth`.
fn load_or_generate_vapid_at(path: &std::path::Path) -> (String, String) {
    if let Ok(bytes) = std::fs::read(path) {
        if let Ok(v) = serde_json::from_slice::<VapidFile>(&bytes) {
            if !v.public_key.is_empty() && !v.private_key.is_empty() {
                return (v.public_key, v.private_key);
            }
        }
    }
    let (pub_k, priv_k) = generate_vapid_keys();
    let file = VapidFile {
        public_key: pub_k.clone(),
        private_key: priv_k.clone(),
    };
    if let Ok(json) = serde_json::to_vec_pretty(&file) {
        // The private key is a signing secret — owner-only from creation,
        // never briefly 0644 like an `fs::write` + `chmod` (th-5c0189).
        match crate::secret_file::write_secret(path, json) {
            Ok(()) => tracing::info!(path = %path.display(), "auto-provisioned a VAPID keypair for Web Push"),
            Err(e) => tracing::warn!(error = %e, path = %path.display(), "could not persist the VAPID keypair — it will be regenerated on restart"),
        }
    }
    (pub_k, priv_k)
}

/// Generate a P-256 VAPID keypair as (public, private) raw base64url — the format
/// [`vapid_key_pair`] reads and the browser's
/// `applicationServerKey` expects (65-byte uncompressed point / 32-byte scalar).
fn generate_vapid_keys() -> (String, String) {
    use base64::Engine as _;
    use p256::elliptic_curve::sec1::ToEncodedPoint as _;

    let secret = p256::SecretKey::random(&mut rand::rngs::OsRng);
    let public_point = secret.public_key().to_encoded_point(false); // uncompressed: 0x04 || x || y
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    (b64.encode(public_point.as_bytes()), b64.encode(secret.to_bytes().as_slice()))
}

/// `/push/*` — the browser subscribes here and the SPA reads the VAPID public key.
/// Takes the state so the caller can ALSO hand it to the turn notifier
/// (th-b9a636) — before this, `PushState` never escaped the router and nothing
/// could ever trigger a push.
pub fn push_router(state: PushState) -> Router {
    Router::new()
        .route("/push/key", get(get_key))
        .route("/push/subscribe", post(subscribe))
        .route("/push/test", post(test_push))
        .with_state(state)
}

async fn get_key(State(state): State<PushState>) -> Response {
    match &state.public_key {
        Some(key) => Json(serde_json::json!({ "publicKey": key })).into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "push not configured").into_response(),
    }
}

async fn subscribe(State(state): State<PushState>, Json(sub): Json<Subscription>) -> Response {
    if !state.enabled() {
        return (StatusCode::SERVICE_UNAVAILABLE, "push not configured").into_response();
    }
    let mut subs = state.load_subs();
    if !subs.iter().any(|s| s.endpoint == sub.endpoint) {
        subs.push(sub);
        state.save_subs(&subs).await;
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn test_push(State(state): State<PushState>) -> Response {
    let n = state.send_to_all("Big Smooth", "Push is working — I can reach your phone.").await;
    Json(serde_json::json!({ "sent": n })).into_response()
}

use axum::response::{IntoResponse, Response};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_are_valid_vapid() {
        use base64::Engine as _;
        let (pub_k, priv_k) = generate_vapid_keys();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        // Public: 65-byte uncompressed EC point (0x04 || x || y); private: 32-byte scalar.
        let pub_bytes = b64.decode(&pub_k).expect("public is base64url");
        let priv_bytes = b64.decode(&priv_k).expect("private is base64url");
        assert_eq!(pub_bytes.len(), 65);
        assert_eq!(pub_bytes[0], 0x04, "uncompressed point marker");
        assert_eq!(priv_bytes.len(), 32);
        // The signer must accept our private key, and derive the same public point.
        let kp = vapid_key_pair(&priv_k).expect("generated private key parses");
        let derived = web_push_native::p256::PublicKey::from_sec1_bytes(&kp.public_key().to_bytes()).unwrap();
        let advertised = web_push_native::p256::PublicKey::from_sec1_bytes(&pub_bytes).unwrap();
        assert_eq!(derived, advertised, "keypair matches advertised public key");
    }

    #[test]
    fn vapid_key_pair_rejects_garbage() {
        assert!(vapid_key_pair("not base64!").is_err());
        assert!(vapid_key_pair("AAAA").is_err(), "wrong length");
    }

    /// A browser-side keypair + auth secret, as `pushManager.subscribe` makes.
    fn ua_subscription(endpoint: &str) -> (Subscription, web_push_native::p256::SecretKey, [u8; 16]) {
        use base64::Engine as _;
        use web_push_native::p256::elliptic_curve::sec1::ToEncodedPoint as _;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let ua_secret = web_push_native::p256::SecretKey::random(&mut rand::rngs::OsRng);
        let auth: [u8; 16] = rand::random();
        let sub = Subscription {
            endpoint: endpoint.into(),
            keys: SubscriptionKeys {
                p256dh: b64.encode(ua_secret.public_key().to_encoded_point(false).as_bytes()),
                auth: b64.encode(auth),
            },
        };
        (sub, ua_secret, auth)
    }

    #[test]
    fn push_request_round_trips_through_browser_decryption() {
        let (_, priv_k) = generate_vapid_keys();
        let kp = vapid_key_pair(&priv_k).unwrap();
        let (sub, ua_secret, auth) = ua_subscription("https://push.example.com/send/abc");
        let req = build_push_request(&sub, &kp, br#"{"title":"hi"}"#).expect("request builds");

        assert_eq!(req.method(), axum::http::Method::POST);
        assert_eq!(req.uri(), "https://push.example.com/send/abc");
        assert_eq!(req.headers()["content-encoding"], "aes128gcm");
        assert!(req.headers().contains_key("ttl"));
        let authz = req.headers()["authorization"].to_str().unwrap();
        assert!(authz.starts_with("vapid t="), "RFC 8292 vapid scheme: {authz}");
        assert!(authz.contains(", k="), "carries the VAPID public key: {authz}");

        // The device (holding ua_secret + auth) can decrypt exactly our payload.
        let plain = web_push_native::decrypt(req.into_body(), &ua_secret, &Auth::clone_from_slice(&auth)).expect("decrypts");
        assert_eq!(plain, br#"{"title":"hi"}"#);
    }

    #[test]
    fn push_request_rejects_bad_subscription_keys() {
        let (_, priv_k) = generate_vapid_keys();
        let kp = vapid_key_pair(&priv_k).unwrap();
        let (mut sub, _, _) = ua_subscription("https://push.example.com/x");
        sub.keys.auth = "c2hvcnQ".into(); // 5 bytes, not 16
        assert!(build_push_request(&sub, &kp, b"x").is_err());
        let (mut sub, _, _) = ua_subscription("https://push.example.com/x");
        sub.keys.p256dh = "AAAA".into();
        assert!(build_push_request(&sub, &kp, b"x").is_err());
        let (mut sub, _, _) = ua_subscription("not a uri with spaces");
        sub.endpoint = "not a uri with spaces".into();
        assert!(build_push_request(&sub, &kp, b"x").is_err());
    }

    #[test]
    fn expired_statuses_prune() {
        assert!(is_expired_status(StatusCode::GONE));
        assert!(is_expired_status(StatusCode::NOT_FOUND));
        assert!(!is_expired_status(StatusCode::CREATED));
        assert!(!is_expired_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(!is_expired_status(StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[tokio::test]
    async fn send_to_all_posts_and_prunes_gone_subscriptions() {
        use axum::extract::Path;
        let app = Router::new().route(
            "/push/{id}",
            post(|Path(id): Path<String>| async move {
                if id == "gone" {
                    StatusCode::GONE
                } else {
                    StatusCode::CREATED
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });

        let dir = tempfile::tempdir().unwrap();
        let (pub_k, priv_k) = generate_vapid_keys();
        let state = PushState {
            public_key: Some(pub_k),
            private_key: Some(priv_k),
            subs_path: dir.path().join("subs.json"),
            lock: Arc::new(Mutex::new(())),
        };
        let (live, _, _) = ua_subscription(&format!("http://{addr}/push/live"));
        let (gone, _, _) = ua_subscription(&format!("http://{addr}/push/gone"));
        state.save_subs(&[live.clone(), gone]).await;

        assert_eq!(state.send_to_all("t", "b").await, 1, "only the live endpoint accepted");
        assert_eq!(state.load_subs(), vec![live], "the 410 subscription was pruned");
    }

    #[test]
    fn load_or_generate_is_stable() {
        let dir = std::env::temp_dir().join(format!("vapid-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("vapid.json");
        let first = load_or_generate_vapid_at(&path);
        assert!(path.exists(), "keypair persisted on first run");
        let second = load_or_generate_vapid_at(&path);
        assert_eq!(first, second, "second run reuses the persisted pair");
        assert!(!first.0.is_empty() && !first.1.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn subs_roundtrip() {
        let dir = std::env::temp_dir().join(format!("push-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let state = PushState {
            public_key: Some("pub".into()),
            private_key: Some("priv".into()),
            subs_path: dir.join("subs.json"),
            lock: Arc::new(Mutex::new(())),
        };
        let sub = Subscription {
            endpoint: "https://x/1".into(),
            keys: SubscriptionKeys {
                p256dh: "p".into(),
                auth: "a".into(),
            },
        };
        state.save_subs(std::slice::from_ref(&sub)).await;
        assert_eq!(state.load_subs(), vec![sub]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

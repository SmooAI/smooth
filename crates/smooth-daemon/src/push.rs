//! Web Push — Big Smooth pushes a notification to the user's phone (an installed
//! PWA) even when the tab is closed. The browser subscribes (POST /push/subscribe
//! with its endpoint + keys); the daemon VAPID-signs + encrypts a payload and
//! POSTs it to that endpoint via the `web-push` crate; the service worker
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
#[cfg(not(target_os = "windows"))]
use web_push::{ContentEncoding, SubscriptionInfo, VapidSignatureBuilder, WebPushClient, WebPushMessageBuilder};

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
        // Windows has no sender (see `send_to_all`), so it must not advertise a
        // VAPID key either — handing one out would let the SPA register a
        // service worker for pushes that can never arrive. Dropping the keys
        // here makes every `/push/*` route 503 through the existing
        // `enabled()` checks, no per-route cfg needed.
        let configured = !cfg!(target_os = "windows");
        let (public_key, private_key) = resolve_vapid_keys(configured);
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
        if let Some(parent) = self.subs_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(subs) {
            let _ = std::fs::write(&self.subs_path, json);
        }
    }

    /// Send `{title, body}` to every subscribed device. Expired subscriptions
    /// (410/404) are pruned. Best-effort: a failing endpoint never blocks the rest.
    ///
    /// **Windows: always a no-op.** The `web-push` crate reaches `ece`, which
    /// hard-depends on `openssl` (no pure-Rust backend in 2.x) and finds no
    /// OpenSSL on a stock Windows host. The dependency is therefore declared
    /// only for non-Windows targets, which is what lets the rest of the daemon
    /// — the whole agent engine — compile and run there at all. Subscribing
    /// still 503s on Windows via [`Self::enabled`], so the SPA degrades
    /// cleanly rather than half-registering a service worker. Windows push is
    /// a follow-up (vendored OpenSSL, or a `web-push` release on `ece` 3.x).
    #[cfg(target_os = "windows")]
    pub async fn send_to_all(&self, title: &str, body: &str) -> usize {
        let _ = (title, body);
        tracing::debug!("web push is not available on Windows (web-push -> ece -> openssl); skipping notification");
        0
    }

    #[cfg(not(target_os = "windows"))]
    pub async fn send_to_all(&self, title: &str, body: &str) -> usize {
        if !self.enabled() {
            return 0;
        }
        let private = self.private_key.clone().unwrap();
        let payload = serde_json::json!({ "title": title, "body": body }).to_string();
        let client = match web_push::IsahcWebPushClient::new() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "web-push client init failed");
                return 0;
            }
        };
        let subs = self.load_subs();
        let mut kept = Vec::with_capacity(subs.len());
        let mut sent = 0;
        for sub in subs {
            let info = SubscriptionInfo::new(sub.endpoint.clone(), sub.keys.p256dh.clone(), sub.keys.auth.clone());
            let msg = (|| {
                let mut sig = VapidSignatureBuilder::from_base64(&private, &info)?;
                sig.add_claim("sub", "mailto:dev@smoo.ai");
                let mut builder = WebPushMessageBuilder::new(&info);
                builder.set_payload(ContentEncoding::Aes128Gcm, payload.as_bytes());
                builder.set_vapid_signature(sig.build()?);
                builder.build()
            })();
            match msg {
                Ok(m) => match client.send(m).await {
                    Ok(()) => {
                        sent += 1;
                        kept.push(sub);
                    }
                    Err(web_push::WebPushError::EndpointNotValid(_) | web_push::WebPushError::EndpointNotFound(_)) => {
                        tracing::info!(endpoint = %sub.endpoint, "pruning expired push subscription");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "web-push send failed");
                        kept.push(sub);
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "web-push build failed");
                    kept.push(sub);
                }
            }
        }
        self.save_subs(&kept).await;
        sent
    }
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
/// a freshly generated pair (persisted for next time). `configured == false`
/// (Windows) short-circuits to no keys, keeping push disabled there.
fn resolve_vapid_keys(configured: bool) -> (Option<String>, Option<String>) {
    if !configured {
        return (None, None);
    }
    let env_pub = std::env::var("SMOOTH_VAPID_PUBLIC").ok().filter(|s| !s.is_empty());
    let env_priv = std::env::var("SMOOTH_VAPID_PRIVATE").ok().filter(|s| !s.is_empty());
    if let (Some(pub_k), Some(priv_k)) = (env_pub, env_priv) {
        return (Some(pub_k), Some(priv_k));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let (pub_k, priv_k) = load_or_generate_vapid_at(&vapid_path());
        (Some(pub_k), Some(priv_k))
    }
    #[cfg(target_os = "windows")]
    {
        (None, None)
    }
}

/// Load the persisted VAPID keypair, generating + persisting one on first run.
/// Path-injectable so tests don't touch `~/.smooth`.
#[cfg(not(target_os = "windows"))]
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
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&file) {
        if std::fs::write(path, json).is_ok() {
            // The private key is a signing secret — lock it down like an ssh key.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
            tracing::info!(path = %path.display(), "auto-provisioned a VAPID keypair for Web Push");
        }
    }
    (pub_k, priv_k)
}

/// Generate a P-256 VAPID keypair as (public, private) raw base64url — the format
/// `VapidSignatureBuilder::from_base64` reads and the browser's
/// `applicationServerKey` expects (65-byte uncompressed point / 32-byte scalar).
#[cfg(not(target_os = "windows"))]
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
    fn windows_resolves_to_no_keys() {
        // `configured == false` (the Windows path) never provisions — push stays off.
        assert_eq!(resolve_vapid_keys(false), (None, None));
    }

    #[cfg(not(target_os = "windows"))]
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
        // web-push must accept our private key in its VAPID builder (p256dh reuses
        // the generated public point — a valid uncompressed key; auth is 16 bytes).
        let info = SubscriptionInfo::new("https://example.com/x", &pub_k, "MDEyMzQ1Njc4OWFiY2RlZg");
        assert!(VapidSignatureBuilder::from_base64(&priv_k, &info).is_ok(), "web-push accepts the generated key");
    }

    #[cfg(not(target_os = "windows"))]
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

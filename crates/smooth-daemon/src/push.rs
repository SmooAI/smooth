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
        let public_key = std::env::var("SMOOTH_VAPID_PUBLIC").ok().filter(|s| !s.is_empty());
        let private_key = std::env::var("SMOOTH_VAPID_PRIVATE").ok().filter(|s| !s.is_empty());
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
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    home.join(".smooth").join("push-subs.json")
}

/// `/push/*` — the browser subscribes here and the SPA reads the VAPID public key.
pub fn push_router() -> Router {
    Router::new()
        .route("/push/key", get(get_key))
        .route("/push/subscribe", post(subscribe))
        .route("/push/test", post(test_push))
        .with_state(PushState::from_env())
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

    #[tokio::test]
    async fn disabled_without_keys() {
        std::env::remove_var("SMOOTH_VAPID_PUBLIC");
        std::env::remove_var("SMOOTH_VAPID_PRIVATE");
        let state = PushState::from_env();
        assert!(!state.enabled());
        assert_eq!(state.send_to_all("t", "b").await, 0);
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
        state.save_subs(&[sub.clone()]).await;
        assert_eq!(state.load_subs(), vec![sub]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

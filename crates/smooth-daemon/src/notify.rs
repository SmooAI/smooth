//! Turn-completion notifications (pearl th-b9a636, EPIC th-5561c5) — the
//! TRIGGER layer push never had.
//!
//! `push.rs` has been able to *send* web pushes since th-c561f1, but nothing
//! ever called it except `POST /push/test` — a scheduled reminder ran its turn
//! and the result sat silently in the conversation. This module fires on
//! scheduled-turn completion and notifies through BOTH channels, best-effort:
//!
//! 1. **Web push** — the existing [`crate::push::PushState`] fan-out to the
//!    installed PWA (VAPID, `~/.smooth/push-subs.json`).
//! 2. **Phone push** — the Big Smooth iOS/Android apps register FCM tokens with
//!    Smoo AI; the daemon POSTs the EXISTING platform route
//!    `POST /organizations/{org}/notifications/self` (SMOODEV-2269: in-app +
//!    FCM to the authenticated user's own devices, rule-free) using its stored
//!    Smoo session. No push credentials on the daemon, no new backend surface.
//!    Signed out ⇒ the phone leg is skipped quietly.
//!
//! Every failure is a debug/warn log — notifications must never fail a turn.

use serde_json::{json, Value};
use smooai_client_shared::auth::storage::CredentialsStore;

use crate::push::PushState;

/// Default Smoo AI platform API base; `SMOOAI_API_URL` overrides (dev/test).
const DEFAULT_API_BASE: &str = "https://api.smoo.ai";
/// Push bodies are lock-screen strings — keep them a glance long.
const BODY_MAX: usize = 160;
/// The deep link the Big Smooth apps open on tap.
const DEEP_LINK: &str = "bigsmooth://chat";

/// The self-notify endpoint for `org` on `base`.
fn self_notify_url(base: &str, org: &str) -> String {
    format!("{}/organizations/{org}/notifications/self", base.trim_end_matches('/'))
}

/// Clamp a response to a notification-sized body (char-boundary safe, ellipsized).
fn clamp_body(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= BODY_MAX {
        return s.to_string();
    }
    let clipped: String = s.chars().take(BODY_MAX - 1).collect();
    format!("{clipped}…")
}

/// Pull the assistant's final text out of an `eventual_response` frame.
/// Canonical home is `/data/data/response`; probe defensively (same tolerance
/// the SPA applies to usage) and fall back through the known shapes.
pub fn response_text(v: &Value) -> Option<String> {
    for path in ["/data/data/response", "/data/response", "/response"] {
        if let Some(s) = v.pointer(path).and_then(Value::as_str) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Fires turn-completion notifications through every reachable channel.
pub struct TurnNotifier {
    web: PushState,
    api_base: String,
    store: Option<CredentialsStore>,
    http: reqwest::Client,
}

impl TurnNotifier {
    /// A notifier over the daemon's web-push state + the stored Smoo session.
    #[must_use]
    pub fn new(web: PushState) -> Self {
        Self {
            web,
            api_base: std::env::var("SMOOAI_API_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_API_BASE.to_string()),
            store: CredentialsStore::default_user().ok(),
            http: reqwest::Client::new(),
        }
    }

    /// Notify that a scheduled/proactive turn finished. `title` is the schedule's
    /// prompt gist; `body` the response gist. Best-effort on both channels.
    pub async fn turn_completed(&self, title: &str, body: &str) {
        let title = if title.trim().is_empty() { "Big Smooth" } else { title.trim() };
        let body = clamp_body(body);
        let web_sent = self.web.send_to_all(title, &body).await;
        let phone_sent = self.phone_notify(title, &body).await;
        tracing::info!(web = web_sent, phone = ?phone_sent, "turn notification fanned out");
    }

    /// The phone leg: self-notify through the platform (in-app + FCM). Returns
    /// the device count pushed, or `None` when skipped (signed out) / failed.
    async fn phone_notify(&self, title: &str, body: &str) -> Option<u64> {
        // Fresh read every call — the credential heartbeat rotates the session.
        let creds = self.store.as_ref()?.load().ok().flatten()?;
        let org = creds.active_org_id.filter(|o| !o.trim().is_empty())?;
        if creds.access_token.is_empty() {
            return None;
        }
        let resp = self
            .http
            .post(self_notify_url(&self.api_base, &org))
            .bearer_auth(&creds.access_token)
            .json(&json!({ "title": title, "body": body, "deepLink": DEEP_LINK }))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let v: Value = r.json().await.ok()?;
                v.get("pushed").and_then(Value::as_u64)
            }
            Ok(r) => {
                tracing::debug!(status = %r.status(), "self-notify refused (expired session?)");
                None
            }
            Err(e) => {
                tracing::debug!(error = %e, "self-notify unreachable");
                None
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn self_notify_url_shape() {
        assert_eq!(
            self_notify_url("https://api.smoo.ai", "org-1"),
            "https://api.smoo.ai/organizations/org-1/notifications/self"
        );
        // trailing slash tolerated
        assert_eq!(
            self_notify_url("https://api.smoo.ai/", "org-1"),
            "https://api.smoo.ai/organizations/org-1/notifications/self"
        );
    }

    #[test]
    fn clamp_body_passes_short_and_ellipsizes_long() {
        assert_eq!(clamp_body("  done!  "), "done!");
        let long = "x".repeat(500);
        let clamped = clamp_body(&long);
        assert_eq!(clamped.chars().count(), BODY_MAX);
        assert!(clamped.ends_with('…'));
        // multi-byte safety: no panic slicing a char boundary
        let emoji = "🎉".repeat(300);
        assert!(clamp_body(&emoji).chars().count() <= BODY_MAX);
    }

    #[test]
    fn response_text_probes_known_shapes() {
        assert_eq!(
            response_text(&serde_json::json!({"data":{"data":{"response":"the answer"}}})).as_deref(),
            Some("the answer")
        );
        assert_eq!(response_text(&serde_json::json!({"data":{"response":"alt"}})).as_deref(), Some("alt"));
        assert_eq!(response_text(&serde_json::json!({"response":"flat"})).as_deref(), Some("flat"));
        // blank / absent → None
        assert_eq!(response_text(&serde_json::json!({"data":{"data":{"response":"  "}}})), None);
        assert_eq!(response_text(&serde_json::json!({"type":"eventual_response"})), None);
    }
}

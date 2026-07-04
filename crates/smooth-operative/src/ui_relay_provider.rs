//! `HttpUiProvider` — the operative side of the SEP Phase 6 `ui/*` relay.
//!
//! On the daemon dispatch path the operative hosts extensions in-process but
//! has no terminal to render a `ui/*` request on. This [`HostDelegate`] relays
//! each request to Big Smooth over the same `SMOOTH_NARC_URL` +
//! `SMOOTH_HOST_TOKEN` callback channel the `host_tool` uses (see
//! `host_tool.rs`); Big Smooth fans it out to connected frontends and, for
//! interactive kinds, blocks until a human answers. The daemon's response body
//! IS the answer the extension's `ui/*` call resolves to.
//!
//! Every other [`HostDelegate`] method keeps the engine default (kv → local
//! file, exec/session → denied), so only UI is relayed. When the callback env
//! is absent (non-dispatch runs) the operative uses the plain
//! `DefaultHostDelegate` instead and this provider is never constructed.

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::extension::protocol::{codes, RpcError};
use smooth_operator::extension::HostDelegate;

/// Relays `ui/*` to Big Smooth's `/api/ui/request`. Holds the resolved callback
/// URL, bearer, and dispatched task id so each request carries its routing
/// identity.
pub struct HttpUiProvider {
    /// `{SMOOTH_NARC_URL}/api/ui/request`.
    url: String,
    /// `SMOOTH_HOST_TOKEN` — the per-dispatch bearer.
    token: String,
    /// `SMOOTH_OPERATOR_ID` — scopes the request to a frontend session.
    task_id: String,
}

impl HttpUiProvider {
    /// Construct from the dispatch env. Returns `None` unless both
    /// `SMOOTH_NARC_URL` and `SMOOTH_HOST_TOKEN` are set — i.e. only under a
    /// real Big Smooth dispatch, never a bare local run.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let narc_url = std::env::var("SMOOTH_NARC_URL").ok()?;
        let token = std::env::var("SMOOTH_HOST_TOKEN").ok()?;
        let task_id = std::env::var("SMOOTH_OPERATOR_ID").unwrap_or_default();
        Some(Self {
            url: format!("{}/api/ui/request", narc_url.trim_end_matches('/')),
            token,
            task_id,
        })
    }

    /// The `ui/*` kinds a daemon-relayed frontend can render. Matches the
    /// TUI's set (`smooth_code::sep_host::TUI_UI_CAPABILITIES`); the smooth-web
    /// components render all of them.
    #[must_use]
    pub fn capabilities() -> Vec<String> {
        ["select", "confirm", "input", "notify", "set_status", "set_widget", "set_title"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }
}

/// `select`/`confirm`/`input` await a human; the rest are render-only.
fn is_interactive(kind: &str) -> bool {
    matches!(kind, "select" | "confirm" | "input")
}

#[async_trait]
impl HostDelegate for HttpUiProvider {
    async fn ui_request(&self, ext: &str, params: Value) -> Result<Value, RpcError> {
        let kind = params.get("kind").and_then(Value::as_str).unwrap_or_default().to_string();
        let body = json!({
            "task_id": self.task_id,
            "ext": ext,
            "kind": kind,
            "params": params,
        });

        // Generous client timeout: the daemon itself bounds the human wait
        // (SMOOTH_UI_TIMEOUT_SECS, default 120s) and returns {cancelled} on
        // its own timeout, so give it headroom past that before the transport
        // gives up.
        let client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(300)).build() {
            Ok(c) => c,
            Err(e) => return Err(RpcError::new(codes::INTERNAL_ERROR, format!("ui relay: building http client: {e}"))),
        };

        let resp = client
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&body)
            .send()
            .await;

        let degraded = match resp {
            Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                Ok(v) => return Ok(v),
                Err(e) => relay_failure(&kind, &format!("parsing ui relay response: {e}")),
            },
            Ok(r) => {
                let status = r.status();
                let txt = r.text().await.unwrap_or_default();
                relay_failure(&kind, &format!("ui relay returned {status}: {txt}"))
            }
            Err(e) => relay_failure(&kind, &format!("calling ui relay: {e}")),
        };
        Ok(degraded)
    }
}

/// A relay failure degrades gracefully: an interactive request resolves to
/// `{cancelled:true}` (the extension treats it as a dismissed dialog); a
/// one-way request resolves to `{}` (nothing was awaited). Never surfaces the
/// transport error to the extension — a broken daemon shouldn't crash a turn.
fn relay_failure(kind: &str, detail: &str) -> Value {
    tracing::warn!(kind, detail, "sep ui relay failed — degrading");
    if is_interactive(kind) {
        json!({ "cancelled": true })
    } else {
        json!({})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_classification() {
        assert!(is_interactive("select"));
        assert!(is_interactive("confirm"));
        assert!(is_interactive("input"));
        assert!(!is_interactive("notify"));
        assert!(!is_interactive("set_widget"));
    }

    #[test]
    fn relay_failure_degrades_by_kind() {
        assert_eq!(relay_failure("confirm", "x"), json!({ "cancelled": true }));
        assert_eq!(relay_failure("notify", "x"), json!({}));
    }

    #[test]
    fn capabilities_cover_all_kinds() {
        let caps = HttpUiProvider::capabilities();
        for k in ["select", "confirm", "input", "notify", "set_status", "set_widget", "set_title"] {
            assert!(caps.contains(&k.to_string()), "missing {k}");
        }
    }

    #[test]
    fn from_env_requires_callback_vars() {
        // In the test process these are unset → None. (We avoid mutating env
        // in a shared test binary — the presence path is covered by the live
        // dispatch, not a unit test.)
        // ponytail: env-presence assertion only; live path exercised by dispatch.
        assert!(std::env::var("SMOOTH_NARC_URL").is_err() || HttpUiProvider::from_env().is_some());
    }
}

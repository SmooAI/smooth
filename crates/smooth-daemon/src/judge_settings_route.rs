//! `GET`/`POST /api/judge` — the Settings page's controls for Narc's LLM safety
//! judge (pearls th-eec7a5, th-7aa2af).
//!
//! Mirrors [`crate::mode_session_route`]: a tiny axum router over a shared
//! [`JudgeSettings`] store that the `NarcHook` reads on every tool call. Unlike
//! the per-conversation mode, the judge config is **daemon-wide** — one set of
//! knobs for the whole assistant — so there is no `session` key.
//!
//! `POST {enabled?, strictness?, model?}` applies a partial update (only the
//! provided fields change) and echoes the full config. 400 on an unknown
//! `strictness`. `GET` reads the current config.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::judge_settings::{JudgeConfig, JudgeSettings, Strictness};

#[derive(Deserialize)]
struct PatchBody {
    /// Turn the LLM-judge escalation on/off. Absent ⇒ unchanged.
    #[serde(default)]
    enabled: Option<bool>,
    /// `"lenient"`/`"normal"`/`"strict"`. Absent ⇒ unchanged; unknown ⇒ 400.
    #[serde(default)]
    strictness: Option<String>,
    /// The judge model. Absent ⇒ unchanged.
    #[serde(default)]
    model: Option<String>,
}

#[derive(Serialize)]
struct JudgeReply {
    enabled: bool,
    strictness: &'static str,
    model: String,
}

impl From<JudgeConfig> for JudgeReply {
    fn from(c: JudgeConfig) -> Self {
        Self {
            enabled: c.enabled,
            strictness: c.strictness.as_str(),
            model: c.model,
        }
    }
}

/// `POST /api/judge` — partial update. An unknown `strictness` is a 400 (a bad
/// value must not silently pick a weaker posture).
async fn set_judge(State(settings): State<JudgeSettings>, Json(body): Json<PatchBody>) -> Result<Json<JudgeReply>, (StatusCode, String)> {
    let strictness = match body.strictness.as_deref() {
        None => None,
        Some(raw) => {
            Some(Strictness::parse(raw).ok_or_else(|| (StatusCode::BAD_REQUEST, format!("unknown strictness `{raw}` (expected lenient|normal|strict)")))?)
        }
    };
    // A blank model is meaningless (the judge needs a model name); ignore it
    // rather than let it wipe the configured one.
    let model = body.model.filter(|m| !m.trim().is_empty());
    let cfg = settings.patch(body.enabled, strictness, model);
    Ok(Json(cfg.into()))
}

/// `GET /api/judge` — the current config, so the Settings page can sync.
async fn get_judge(State(settings): State<JudgeSettings>) -> Json<JudgeReply> {
    Json(settings.get().into())
}

/// The `/api/judge` router, backed by the shared [`JudgeSettings`] store.
// No `#[must_use]`: axum's `Router` is already `#[must_use]` (clippy
// `double_must_use` — the sibling cwd_router/mode_router omit it too).
pub fn judge_router(settings: JudgeSettings) -> Router {
    Router::new().route("/api/judge", post(set_judge).get(get_judge)).with_state(settings)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn post_req(json: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/api/judge")
            .header("content-type", "application/json")
            .body(Body::from(json.to_owned()))
            .unwrap()
    }

    #[tokio::test]
    async fn post_partial_update_toggles_enabled_only() {
        let settings = JudgeSettings::new(JudgeConfig::defaults("fast".into()));
        let router = judge_router(settings.clone());
        let resp = router.oneshot(post_req(r#"{"enabled":false}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["enabled"], false);
        assert_eq!(v["model"], "fast", "model untouched by a partial update");
        assert_eq!(v["strictness"], "normal");
        // The store the NarcHook reads reflects the POST.
        assert!(!settings.get().enabled);
    }

    #[tokio::test]
    async fn post_sets_strictness_and_model() {
        let settings = JudgeSettings::new(JudgeConfig::defaults("fast".into()));
        let router = judge_router(settings.clone());
        let resp = router.oneshot(post_req(r#"{"strictness":"strict","model":"judge-x"}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["strictness"], "strict");
        assert_eq!(v["model"], "judge-x");
        assert_eq!(settings.get().strictness, Strictness::Strict);
    }

    #[tokio::test]
    async fn post_unknown_strictness_is_400_and_leaves_state() {
        let settings = JudgeSettings::new(JudgeConfig::defaults("fast".into()));
        let router = judge_router(settings.clone());
        let resp = router.oneshot(post_req(r#"{"strictness":"paranoid"}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(settings.get().strictness, Strictness::Normal, "a rejected set leaves state unchanged");
    }

    #[tokio::test]
    async fn post_blank_model_is_ignored() {
        let settings = JudgeSettings::new(JudgeConfig::defaults("fast".into()));
        let router = judge_router(settings.clone());
        let resp = router.oneshot(post_req(r#"{"model":"   "}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(settings.get().model, "fast", "a blank model must not wipe the configured one");
    }

    #[tokio::test]
    async fn get_returns_current_config() {
        let settings = JudgeSettings::new(JudgeConfig {
            enabled: false,
            strictness: Strictness::Lenient,
            model: "m".into(),
        });
        let router = judge_router(settings);
        let resp = router
            .oneshot(Request::builder().method("GET").uri("/api/judge").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["enabled"], false);
        assert_eq!(v["strictness"], "lenient");
        assert_eq!(v["model"], "m");
    }
}

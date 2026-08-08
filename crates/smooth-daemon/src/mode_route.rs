//! `GET /api/mode` — what model Big Smooth would run a turn with right now.
//!
//! Pearl th-7630a7: at idle `th code`'s status bar read `fixer · unknown`
//! because nothing asked the daemon which model its next turn would use —
//! `model_label` only learns it after the first runner event. This route
//! answers from the same resolution the engine uses (`config::resolve_llm`:
//! env → providers.json), model name only — never credentials.
//!
//! Ungated like `/search` and `/api/skills`: the model name must render on a
//! tokenless connection, and it leaks nothing an attacker could use.

use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

/// The `{ "model": "..." | null }` envelope. `null` means the daemon has no
/// LLM credentials resolved — clients should keep saying "unknown".
#[derive(Debug, Serialize)]
pub struct ModeResponse {
    pub model: Option<String>,
}

/// Build the `/api/mode` router.
pub fn mode_router() -> Router {
    Router::new().route("/api/mode", get(mode_handler))
}

async fn mode_handler() -> Json<ModeResponse> {
    // resolve_llm reads env + providers.json from disk — keep it off the
    // async runtime like the other filesystem-touching routes.
    let model = tokio::task::spawn_blocking(|| crate::config::resolve_llm(None).ok().map(|cfg| cfg.model))
        .await
        .ok()
        .flatten();
    Json(ModeResponse { model })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handler_answers_with_a_model_key_and_never_a_credential() {
        let resp = mode_handler().await;
        let json = serde_json::to_value(&resp.0).unwrap();
        // The value depends on the host's credentials; the CONTRACT is the
        // envelope: a "model" key that is a string or null, and nothing else.
        assert!(json.get("model").is_some());
        assert!(json["model"].is_null() || json["model"].is_string());
        assert_eq!(json.as_object().unwrap().len(), 1, "model name only — no keys, no URLs");
    }
}

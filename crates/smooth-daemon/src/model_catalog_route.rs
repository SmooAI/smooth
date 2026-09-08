//! `GET /api/model-catalog` — the bench-scored model lineup, served as the
//! single source of truth so every client (web SPA, iOS, Android, `th code`)
//! reads one list instead of hand-copying it (th-1d8007).
//!
//! The lineup drifted because it was duplicated per platform: `smooth-web`'s
//! `model-scores.json` + `modes.ts`, iOS `SmoothMode.benchModels`, Android
//! `Modes.kt` BENCH_MODELS, the TUI. Serving the canonical bench output
//! (`docs/model-scores.json`, produced by `scripts/the-line/render-model-scores.sh`)
//! lets clients fetch the DATA at runtime and apply their own (identical) derive
//! — badges + sort — so a bench refresh updates one file and every live client
//! follows.
//!
//! Ungated like `/api/mode` and `/search`: the catalog is public data (model
//! names + pass rates + prices), so it must render on a tokenless connection and
//! leaks nothing sensitive.

use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

/// The canonical bench scoreboard, embedded at compile time. This is the exact
/// file `render-model-scores.sh` writes (`cp "$board" docs/model-scores.json`),
/// so the served bytes and the checked-in source of truth can never disagree.
const MODEL_SCORES: &str = include_str!("../../../docs/model-scores.json");

/// Build the `/api/model-catalog` router.
pub fn model_catalog_router() -> Router {
    Router::new().route("/api/model-catalog", get(catalog_handler))
}

async fn catalog_handler() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], MODEL_SCORES)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_is_the_bench_scoreboard_shape() {
        // The contract clients derive against: a `models` array whose entries
        // carry a model id + pass rate. If the embedded file ever stops being the
        // scoreboard shape, clients would fail to derive — catch it here.
        let v: serde_json::Value = serde_json::from_str(MODEL_SCORES).unwrap();
        let models = v.get("models").and_then(|m| m.as_array()).expect("`models` array present");
        assert!(!models.is_empty(), "the catalog must list at least one model");
        let first = &models[0];
        assert!(first.get("model").and_then(|m| m.as_str()).is_some(), "each entry has a `model` id");
        assert!(first.get("pass_rate_pct").is_some(), "each entry has a `pass_rate_pct`");
    }

    #[tokio::test]
    async fn handler_serves_json_content_type() {
        use axum::body::to_bytes;
        let resp = catalog_handler().await.into_response();
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert_eq!(ct, "application/json");
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.get("models").is_some());
    }
}

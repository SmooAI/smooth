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
    let providers = tokio::task::spawn_blocking(read_providers).await.ok().flatten();
    ([(header::CONTENT_TYPE, "application/json")], catalog_json(providers.as_ref()))
}

/// The user's `providers.json`, if readable (`SMOOTH_PROVIDERS_FILE` override,
/// else `~/.smooth/providers.json`).
fn read_providers() -> Option<serde_json::Value> {
    let path = std::env::var_os("SMOOTH_PROVIDERS_FILE")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs_next::home_dir().map(|h| h.join(".smooth/providers.json")))?;
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The served catalog: the bench scoreboard, adjusted for the direct-route
/// models (SMOODEV-4148). One the user has a provider for (Claude Haiku 5.5 on
/// an Anthropic key) is offered — listed under `unbenched` when the bench has
/// not scored it; one they cannot run is removed, so no picker offers a model
/// whose every turn would fail. Without direct-route models in play this is the
/// embedded file byte for byte.
fn catalog_json(providers: Option<&serde_json::Value>) -> String {
    catalog_json_from(MODEL_SCORES, providers)
}

fn catalog_json_from(board: &str, providers: Option<&serde_json::Value>) -> String {
    use smooth_policy::direct_models::DIRECT_MODELS;
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(board) else {
        return board.to_owned();
    };
    let available =
        |d: &smooth_policy::direct_models::DirectModel| providers.is_some_and(|p| smooth_cast::providers::direct_provider_from_value(p, d).is_some());
    let mut changed = false;
    for d in DIRECT_MODELS {
        let is = |m: &serde_json::Value| m.as_str().is_some_and(|m| m.eq_ignore_ascii_case(d.model));
        let benched = v
            .get("models")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|ms| ms.iter().any(|m| m.get("model").is_some_and(is)));
        if available(d) {
            if benched {
                continue;
            }
            let unbenched = v.as_object_mut().map(|o| o.entry("unbenched").or_insert_with(|| serde_json::json!([])));
            if let Some(list) = unbenched.and_then(serde_json::Value::as_array_mut) {
                if !list.iter().any(is) {
                    list.push(serde_json::Value::String(d.model.to_owned()));
                    changed = true;
                }
            }
        } else {
            if let Some(ms) = v.get_mut("models").and_then(serde_json::Value::as_array_mut) {
                let before = ms.len();
                ms.retain(|m| !m.get("model").is_some_and(is));
                changed |= ms.len() != before;
            }
            if let Some(list) = v.get_mut("unbenched").and_then(serde_json::Value::as_array_mut) {
                let before = list.len();
                list.retain(|m| !is(m));
                changed |= list.len() != before;
            }
        }
    }
    if changed {
        v.to_string()
    } else {
        board.to_owned()
    }
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

    fn with_anthropic() -> serde_json::Value {
        serde_json::json!({ "providers": [
            { "id": "smooth", "api_url": "https://llm.smoo.ai/v1", "api_key": "sk-gw" },
            { "id": "anthropic", "api_url": "https://api.anthropic.com/v1", "api_key": "sk-ant", "api_format": "Anthropic" }
        ]})
    }

    fn offered(json: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let mut out: Vec<String> = v["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["model"].as_str().unwrap().to_owned())
            .collect();
        out.extend(v["unbenched"].as_array().into_iter().flatten().map(|m| m.as_str().unwrap().to_owned()));
        out
    }

    #[test]
    fn haiku_is_offered_with_an_anthropic_key() {
        let models = offered(&catalog_json(Some(&with_anthropic())));
        assert!(models.iter().any(|m| m == "claude-haiku-5-5"), "{models:?}");
        assert!(models.iter().any(|m| m == "gpt-6-luna"), "luna stays offered: {models:?}");
    }

    #[test]
    fn haiku_is_hidden_without_a_provider_for_it() {
        let gateway_only = serde_json::json!({ "providers": [{ "id": "smooth", "api_url": "https://llm.smoo.ai/v1", "api_key": "sk-gw" }] });
        for providers in [None, Some(&gateway_only)] {
            let json = catalog_json(providers);
            assert!(!offered(&json).iter().any(|m| m == "claude-haiku-5-5"));
        }
    }

    #[test]
    fn a_benched_but_unrunnable_direct_model_is_removed() {
        let mut v: serde_json::Value = serde_json::from_str(MODEL_SCORES).unwrap();
        let mut row = v["models"][0].clone();
        row["model"] = "claude-haiku-5-5".into();
        v["models"].as_array_mut().unwrap().push(row);
        let without = strip_with(&v, None);
        assert!(!without.iter().any(|m| m == "claude-haiku-5-5"));
        let with = strip_with(&v, Some(&with_anthropic()));
        assert_eq!(
            with.iter().filter(|m| *m == "claude-haiku-5-5").count(),
            1,
            "benched row kept once, not re-added as unbenched"
        );
    }

    /// [`catalog_json`] over an arbitrary scoreboard (the embedded one is fixed
    /// at compile time).
    fn strip_with(board: &serde_json::Value, providers: Option<&serde_json::Value>) -> Vec<String> {
        offered(&catalog_json_from(&board.to_string(), providers))
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

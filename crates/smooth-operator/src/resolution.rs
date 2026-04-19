//! Alias resolution for LiteLLM-backed gateways (e.g. llm.smoo.ai).
//!
//! A typical Smooth setup points every routing slot at a `smooth-*`
//! semantic alias (`smooth-coding`, `smooth-thinking`, …). The gateway
//! maps each alias to a concrete upstream model server-side, so the
//! `/v1/chat/completions` response body still reports the alias. This
//! module hits the LiteLLM admin endpoint `GET /model/info` to recover
//! the real `alias → upstream` map and surface it in the CLI.
//!
//! The response shape we care about (LiteLLM docs):
//! ```json
//! {
//!   "data": [
//!     {
//!       "model_name": "smooth-coding",
//!       "litellm_params": { "model": "anthropic/claude-opus-4-7", ... },
//!       "model_info": { "id": "...", "max_tokens": 200000 }
//!     }
//!   ]
//! }
//! ```

use std::collections::BTreeMap;

use anyhow::{anyhow, Context};
use serde::Deserialize;

/// One routing entry returned by `/model/info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    /// The alias callers use (e.g. `smooth-coding`).
    pub alias: String,
    /// The concrete upstream (e.g. `anthropic/claude-opus-4-7`), when
    /// the gateway chose to surface it.
    pub upstream: Option<String>,
    /// Stable id from `model_info.id`, when present. Useful for
    /// logging so a rename can be traced across the server-side
    /// model list.
    pub id: Option<String>,
}

/// Fetch the alias map from a LiteLLM gateway.
///
/// Returns a sorted map keyed by alias so output is deterministic —
/// `th routing resolved` prints them in the same order every run.
///
/// # Errors
/// Propagates HTTP, timeout, and JSON-parse failures. A 401 means
/// the provider's API key is missing or rejected by the gateway;
/// either way the caller can't see the mapping.
pub async fn fetch_model_info(api_url: &str, api_key: &str) -> anyhow::Result<BTreeMap<String, ResolvedModel>> {
    let url = build_model_info_url(api_url);
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build()?;
    let resp = client.get(&url).bearer_auth(api_key).send().await.with_context(|| format!("GET {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("GET {url} returned {status}: {body}"));
    }
    let body = resp.text().await?;
    parse_model_info(&body)
}

/// Build the `/model/info` URL from a provider's OpenAI-compat
/// `api_url` (e.g. `https://llm.smoo.ai/v1`). Stripping `/v1` is
/// safe because `/model/info` lives at the gateway root in every
/// LiteLLM deployment we've seen.
pub fn build_model_info_url(api_url: &str) -> String {
    let trimmed = api_url.trim_end_matches('/');
    let base = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    format!("{base}/model/info")
}

#[derive(Deserialize)]
struct ModelInfoDoc {
    data: Vec<ModelInfoEntry>,
}

#[derive(Deserialize)]
struct ModelInfoEntry {
    model_name: String,
    #[serde(default)]
    litellm_params: LiteLlmParams,
    #[serde(default)]
    model_info: ModelInfoField,
}

#[derive(Deserialize, Default)]
struct LiteLlmParams {
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize, Default)]
struct ModelInfoField {
    #[serde(default)]
    id: Option<String>,
}

/// Parse a `/model/info` response body into the alias map. Split out
/// for unit testing without a live gateway.
///
/// # Errors
/// Returns an error when the body isn't valid JSON or the top-level
/// shape is missing the `data` array.
pub fn parse_model_info(body: &str) -> anyhow::Result<BTreeMap<String, ResolvedModel>> {
    let doc: ModelInfoDoc = serde_json::from_str(body).context("parsing /model/info response")?;
    let mut out = BTreeMap::new();
    for entry in doc.data {
        out.insert(
            entry.model_name.clone(),
            ResolvedModel {
                alias: entry.model_name,
                upstream: entry.litellm_params.model,
                id: entry.model_info.id,
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_strips_v1_suffix() {
        assert_eq!(build_model_info_url("https://llm.smoo.ai/v1"), "https://llm.smoo.ai/model/info");
        assert_eq!(build_model_info_url("https://llm.smoo.ai/v1/"), "https://llm.smoo.ai/model/info");
        assert_eq!(build_model_info_url("https://example.com"), "https://example.com/model/info");
        assert_eq!(build_model_info_url("https://example.com/"), "https://example.com/model/info");
    }

    #[test]
    fn parse_single_entry() {
        let body = r#"{
            "data": [
                {
                    "model_name": "smooth-coding",
                    "litellm_params": { "model": "anthropic/claude-opus-4-7" },
                    "model_info": { "id": "abc-123", "max_tokens": 200000 }
                }
            ]
        }"#;
        let map = parse_model_info(body).expect("parse");
        let entry = map.get("smooth-coding").expect("smooth-coding present");
        assert_eq!(entry.alias, "smooth-coding");
        assert_eq!(entry.upstream.as_deref(), Some("anthropic/claude-opus-4-7"));
        assert_eq!(entry.id.as_deref(), Some("abc-123"));
    }

    #[test]
    fn parse_entries_are_sorted_by_alias() {
        let body = r#"{
            "data": [
                { "model_name": "smooth-thinking", "litellm_params": { "model": "openrouter/z-ai/glm-5.1" } },
                { "model_name": "smooth-coding",   "litellm_params": { "model": "minimax/minimax-m2.7" } },
                { "model_name": "gpt-4o",          "litellm_params": { "model": "openai/gpt-4o" } }
            ]
        }"#;
        let map = parse_model_info(body).expect("parse");
        let keys: Vec<_> = map.keys().cloned().collect();
        assert_eq!(keys, vec!["gpt-4o", "smooth-coding", "smooth-thinking"]);
    }

    #[test]
    fn parse_missing_upstream_is_none_not_err() {
        let body = r#"{
            "data": [
                { "model_name": "custom-alias", "litellm_params": {}, "model_info": {} }
            ]
        }"#;
        let map = parse_model_info(body).expect("parse");
        let entry = map.get("custom-alias").expect("present");
        assert!(entry.upstream.is_none());
        assert!(entry.id.is_none());
    }

    #[test]
    fn parse_rejects_invalid_json() {
        let err = parse_model_info("not json").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("/model/info"), "expected parse context in error, got: {msg}");
    }

    #[test]
    fn parse_empty_data_array_is_ok() {
        let map = parse_model_info(r#"{"data": []}"#).expect("parse");
        assert!(map.is_empty());
    }
}

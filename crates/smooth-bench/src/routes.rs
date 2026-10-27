//! Per-model routing for models the LLM gateway does not serve.
//!
//! Every model the bench runs normally goes through llm.smoo.ai. Some do
//! not exist there yet — `claude-haiku-5-5` was missing from the gateway
//! when it was first benched (SMOODEV-4125) — but ARE reachable through a
//! provider already configured in `~/.smooth/providers.json`. Rather than
//! wait on a gateway deploy or hand-export another key, `--model` accepts
//! `<model>@<provider-id>`: the bench looks the provider up in the same
//! store the daemon reads and points that one model's engine at it.
//!
//! The row is still labelled by the bare model id, so it lines up with
//! every other row; the route is printed at the start of the run and
//! recorded in the results doc.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

/// One `--model` argument, split into the model id and an optional
/// provider override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub model: String,
    pub provider: Option<String>,
}

impl ModelSpec {
    /// Parse `model` or `model@provider`.
    ///
    /// # Errors
    /// Errors on an empty model or provider half (`@anthropic`, `haiku@`).
    pub fn parse(s: &str) -> Result<Self> {
        match s.split_once('@') {
            None => {
                anyhow::ensure!(!s.trim().is_empty(), "empty --model");
                Ok(Self {
                    model: s.trim().to_string(),
                    provider: None,
                })
            }
            Some((m, p)) => {
                anyhow::ensure!(!m.trim().is_empty() && !p.trim().is_empty(), "--model {s:?}: expected <model>@<provider-id>");
                Ok(Self {
                    model: m.trim().to_string(),
                    provider: Some(p.trim().to_string()),
                })
            }
        }
    }
}

/// Where one provider lives and how to authenticate to it.
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderRoute {
    pub id: String,
    pub api_url: String,
    pub api_key: String,
}

// Hand-written so a `{:?}` in a log line can never print the key.
impl std::fmt::Debug for ProviderRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRoute")
            .field("id", &self.id)
            .field("api_url", &self.api_url)
            .finish_non_exhaustive()
    }
}

/// Find provider `id` in a parsed providers.json document.
///
/// # Errors
/// Errors when the provider is absent, or present without a URL or key —
/// each named, because "no credentials" is useless to the person fixing it.
pub fn provider_from_value(root: &Value, id: &str) -> Result<ProviderRoute> {
    let providers = root
        .get("providers")
        .and_then(Value::as_array)
        .context("providers.json has no `providers` array")?;
    let p = providers.iter().find(|p| p.get("id").and_then(Value::as_str) == Some(id)).with_context(|| {
        let known: Vec<&str> = providers.iter().filter_map(|p| p.get("id").and_then(Value::as_str)).collect();
        format!("no provider {id:?} in providers.json (have: {known:?})")
    })?;
    let api_url = p
        .get("api_url")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .with_context(|| format!("provider {id:?} has no api_url"))?;
    let api_key = p
        .get("api_key")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty())
        .with_context(|| format!("provider {id:?} has no api_key"))?;
    Ok(ProviderRoute {
        id: id.to_string(),
        api_url: api_url.to_string(),
        api_key: api_key.to_string(),
    })
}

/// Look provider `id` up in the providers.json at `path`.
///
/// # Errors
/// Errors when the file cannot be read or the provider is unusable.
pub fn load_provider(path: &Path, id: &str) -> Result<ProviderRoute> {
    let root = smooth_cast::providers::load_value(path).with_context(|| format!("reading {}", path.display()))?;
    provider_from_value(&root, id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_bare_and_routed_specs() {
        assert_eq!(
            ModelSpec::parse("gpt-6-luna").unwrap(),
            ModelSpec {
                model: "gpt-6-luna".into(),
                provider: None
            }
        );
        assert_eq!(
            ModelSpec::parse("claude-haiku-5-5@anthropic").unwrap(),
            ModelSpec {
                model: "claude-haiku-5-5".into(),
                provider: Some("anthropic".into())
            }
        );
    }

    #[test]
    fn rejects_half_specs() {
        for bad in ["", "  ", "@anthropic", "claude-haiku-5-5@", "@"] {
            assert!(ModelSpec::parse(bad).is_err(), "{bad:?}");
        }
    }

    fn doc() -> Value {
        json!({"providers": [
            {"id": "smooth", "api_url": "https://llm.smoo.ai/v1", "api_key": "sk-gw"},
            {"id": "anthropic", "api_url": "https://api.anthropic.com/v1", "api_key": "sk-ant"},
            {"id": "nokey", "api_url": "https://x/v1", "api_key": ""},
            {"id": "nourl", "api_key": "k"}
        ]})
    }

    #[test]
    fn resolves_a_configured_provider() {
        let r = provider_from_value(&doc(), "anthropic").unwrap();
        assert_eq!(r.api_url, "https://api.anthropic.com/v1");
        assert_eq!(r.api_key, "sk-ant");
    }

    #[test]
    fn missing_or_incomplete_providers_say_why() {
        let e = provider_from_value(&doc(), "openai").unwrap_err().to_string();
        assert!(e.contains("openai") && e.contains("anthropic"), "{e}");
        assert!(provider_from_value(&doc(), "nokey").unwrap_err().to_string().contains("api_key"));
        assert!(provider_from_value(&doc(), "nourl").unwrap_err().to_string().contains("api_url"));
        assert!(provider_from_value(&json!({}), "x").is_err());
    }

    #[test]
    fn debug_never_prints_the_key() {
        let r = provider_from_value(&doc(), "anthropic").unwrap();
        let d = format!("{r:?}");
        assert!(!d.contains("sk-ant"), "{d}");
        assert!(d.contains("anthropic"));
    }
}

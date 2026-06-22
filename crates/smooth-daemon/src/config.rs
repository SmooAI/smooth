//! Daemon configuration + LLM credential resolution.
//!
//! Credentials resolve in priority order:
//! 1. **Explicit env** — `SMOOTH_API_URL` + `SMOOTH_API_KEY` (+ `SMOOTH_MODEL`
//!    or a per-task model override). Highest priority so a run can be pointed
//!    at any endpoint without touching config.
//! 2. **`providers.json`** — the credentials `th auth login` writes to
//!    `~/.smooth/providers.json` (overridable with `SMOOTH_PROVIDERS_FILE`).
//!    Resolved through the engine's [`ProviderRegistry`], so the always-on
//!    daemon Just Works with the same creds the rest of `th` uses.
//!
//! If neither is present the daemon errors with an actionable message.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use smooth_operator::providers::{Activity, ProviderRegistry};
use smooth_operator::LlmConfig;

/// The default loopback bind address — matches the legacy Big Smooth port so
/// existing frontends (`th code`, `smooth-web`) connect with no change.
pub const DEFAULT_BIND: &str = "127.0.0.1:4400";

/// Resolve the address the daemon binds to.
///
/// `SMOOTH_DAEMON_BIND` overrides; otherwise [`DEFAULT_BIND`]. Bound to
/// loopback by design — remote access goes over Tailscale (a later phase adds
/// the tailnet bind + bearer-token middleware).
///
/// # Errors
/// Returns an error if `SMOOTH_DAEMON_BIND` is set but unparseable.
pub fn resolve_bind() -> anyhow::Result<SocketAddr> {
    let raw = std::env::var("SMOOTH_DAEMON_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    raw.parse().with_context(|| format!("invalid SMOOTH_DAEMON_BIND: {raw:?}"))
}

/// Path to `providers.json` (`SMOOTH_PROVIDERS_FILE` override, else
/// `~/.smooth/providers.json`).
fn providers_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SMOOTH_PROVIDERS_FILE") {
        return Some(PathBuf::from(p));
    }
    dirs_next::home_dir().map(|h| h.join(".smooth/providers.json"))
}

/// Resolve an engine [`LlmConfig`], honoring a per-task model override.
///
/// # Errors
/// Returns an error if no credentials are available from either source.
pub fn resolve_llm(model_override: Option<&str>) -> anyhow::Result<LlmConfig> {
    resolve_llm_inner(
        std::env::var("SMOOTH_API_URL").ok(),
        std::env::var("SMOOTH_API_KEY").ok(),
        std::env::var("SMOOTH_MODEL").ok(),
        model_override,
        providers_path().as_deref(),
    )
}

/// Pure resolution core (no env / global reads) so the priority logic is unit
/// testable without races.
fn resolve_llm_inner(
    env_api_url: Option<String>,
    env_api_key: Option<String>,
    env_model: Option<String>,
    model_override: Option<&str>,
    providers_path: Option<&Path>,
) -> anyhow::Result<LlmConfig> {
    // 1. Explicit env endpoint.
    if let (Some(api_url), Some(api_key)) = (env_api_url, env_api_key) {
        let model = model_override
            .map(ToOwned::to_owned)
            .or(env_model)
            .context("SMOOTH_API_URL/KEY set but no model: pass `model` in TaskStart or set SMOOTH_MODEL")?;
        let api_format = if api_url.contains("anthropic.com") {
            smooth_operator::llm::ApiFormat::Anthropic
        } else {
            smooth_operator::llm::ApiFormat::OpenAiCompat
        };
        return Ok(LlmConfig {
            api_url,
            api_key,
            model,
            max_tokens: 32_768,
            temperature: 0.0,
            retry_policy: smooth_operator::llm::RetryPolicy::default(),
            api_format,
        });
    }

    // 2. providers.json (th auth login creds), via the engine's registry.
    if let Some(path) = providers_path {
        if path.exists() {
            let registry = ProviderRegistry::load_from_file(path).with_context(|| format!("reading {}", path.display()))?;
            let mut cfg = registry
                .llm_config_for(Activity::Coding)
                .context("resolving an LLM from providers.json (is a provider + routing configured?)")?;
            if let Some(model) = model_override {
                cfg = cfg.with_model(model);
            }
            return Ok(cfg);
        }
    }

    anyhow::bail!("no LLM credentials: run `th auth login` (writes ~/.smooth/providers.json) or set SMOOTH_API_URL + SMOOTH_API_KEY (+ SMOOTH_MODEL)")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn default_bind_parses() {
        assert_eq!(DEFAULT_BIND.parse::<SocketAddr>().unwrap().port(), 4400);
    }

    #[test]
    fn env_endpoint_builds_config() {
        let cfg = resolve_llm_inner(Some("https://llm.smoo.ai/v1".into()), Some("key123".into()), Some("gpt-4o".into()), None, None).unwrap();
        assert_eq!(cfg.api_url, "https://llm.smoo.ai/v1");
        assert_eq!(cfg.api_key, "key123");
        assert_eq!(cfg.model, "gpt-4o");
    }

    #[test]
    fn model_override_beats_env_model() {
        let cfg = resolve_llm_inner(
            Some("https://x/v1".into()),
            Some("k".into()),
            Some("env-model".into()),
            Some("override-model"),
            None,
        )
        .unwrap();
        assert_eq!(cfg.model, "override-model");
    }

    #[test]
    fn anthropic_endpoint_selects_native_format() {
        let cfg = resolve_llm_inner(Some("https://api.anthropic.com/v1".into()), Some("k".into()), Some("claude".into()), None, None).unwrap();
        assert!(matches!(cfg.api_format, smooth_operator::llm::ApiFormat::Anthropic));
    }

    #[test]
    fn env_without_model_errors() {
        let err = resolve_llm_inner(Some("https://x".into()), Some("k".into()), None, None, None).unwrap_err();
        assert!(err.to_string().contains("model"), "{err}");
    }

    #[test]
    fn no_credentials_errors_with_guidance() {
        // No env, and a providers path that does not exist.
        let bogus = Path::new("/nonexistent/smooth-daemon/providers.json");
        let err = resolve_llm_inner(None, None, None, None, Some(bogus)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("th auth login"), "actionable guidance: {msg}");
    }
}

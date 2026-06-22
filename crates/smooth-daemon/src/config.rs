//! Daemon configuration + LLM credential resolution.
//!
//! Phase 1 resolves the LLM endpoint from the environment (`SMOOTH_API_URL`,
//! `SMOOTH_API_KEY`, `SMOOTH_MODEL`), mirroring how `smooth-operative` is
//! configured today. Later phases move this to `@smooai/config` /
//! `~/.smooth/providers.json` so an always-on instance reads durable config,
//! per the EPIC's "config is the only source of truth" stance — but the
//! resolver shape (a fallible `resolve_llm`) stays the same.

use std::net::SocketAddr;

use anyhow::Context;

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

/// Build an engine [`LlmConfig`](smooth_operator::LlmConfig) from the
/// environment, honoring a per-task model override.
///
/// # Errors
/// Returns an error if `SMOOTH_API_URL` / `SMOOTH_API_KEY` are unset, or if no
/// model is available (neither a `TaskStart` override nor `SMOOTH_MODEL`).
pub fn resolve_llm(model_override: Option<&str>) -> anyhow::Result<smooth_operator::LlmConfig> {
    let api_url = std::env::var("SMOOTH_API_URL").context("SMOOTH_API_URL is not set — the daemon needs an LLM endpoint")?;
    let api_key = std::env::var("SMOOTH_API_KEY").context("SMOOTH_API_KEY is not set — the daemon needs an LLM API key")?;
    let model = model_override
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("SMOOTH_MODEL").ok())
        .context("no model: pass `model` in TaskStart or set SMOOTH_MODEL")?;

    // Pick the wire format from the endpoint. Anthropic-native endpoints speak
    // a different schema than OpenAI-compatible gateways (llm.smoo.ai, etc.).
    let api_format = if api_url.contains("anthropic.com") {
        smooth_operator::llm::ApiFormat::Anthropic
    } else {
        smooth_operator::llm::ApiFormat::OpenAiCompat
    };

    Ok(smooth_operator::LlmConfig {
        api_url,
        api_key,
        model,
        max_tokens: 32_768,
        temperature: 0.0,
        retry_policy: smooth_operator::llm::RetryPolicy::default(),
        api_format,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn default_bind_parses() {
        assert_eq!(DEFAULT_BIND.parse::<SocketAddr>().unwrap().port(), 4400);
    }
}

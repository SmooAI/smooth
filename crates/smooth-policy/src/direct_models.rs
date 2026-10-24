//! Main models that are NOT on the `llm.smoo.ai` gateway (SMOODEV-4148).
//!
//! Big Smooth and `th code` both run turns through the daemon's `LocalServer`,
//! whose per-turn `model` override only swaps the model id on the ONE configured
//! gateway. A model the gateway does not serve — Claude Haiku 5.5 today — would
//! 400 there. The daemon's loopback model router reads this table to send such a
//! model straight to the user's own provider instead, and the pickers read it to
//! offer the model only when that provider is configured.
//!
//! Every entry is called on the provider's **OpenAI-compatible** endpoint
//! (`{api_url}/chat/completions`), which is the wire format the engine already
//! speaks, so the router forwards the request body nearly verbatim.

/// One model the daemon routes directly to a provider, bypassing the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectModel {
    /// The model id the pickers offer and the provider expects.
    pub model: &'static str,
    /// The `providers.json` provider id this model prefers.
    pub provider_id: &'static str,
    /// The provider `api_format` that also qualifies when no provider carries
    /// [`Self::provider_id`] (a key added under another id).
    pub api_format: &'static str,
    /// Top-level request fields the provider rejects, removed before forwarding.
    /// Haiku 5.5 400s any request carrying `temperature` ("`temperature` is
    /// deprecated for this model"), and the engine always sends one
    /// ([`crate::llm_params::AGENT_TEMPERATURE`]).
    pub strip_fields: &'static [&'static str],
    /// One-line description for the pickers.
    pub description: &'static str,
}

/// The direct-route main models. Measured 2026-10-24 against
/// `api.anthropic.com/v1/chat/completions`: tool calls and streaming both work
/// for `claude-haiku-5-5` once `temperature` is removed.
pub const DIRECT_MODELS: &[DirectModel] = &[DirectModel {
    model: "claude-haiku-5-5",
    provider_id: "anthropic",
    api_format: "Anthropic",
    strip_fields: &["temperature"],
    description: "Claude Haiku 5.5 — fast Anthropic main model, on your Anthropic key.",
}];

/// The direct-route entry for `model`, if it is one. Exact, case-insensitive
/// match on the trimmed id.
#[must_use]
pub fn direct_model(model: &str) -> Option<&'static DirectModel> {
    let model = model.trim();
    DIRECT_MODELS.iter().find(|d| d.model.eq_ignore_ascii_case(model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haiku_5_5_is_a_direct_anthropic_model() {
        let d = direct_model("claude-haiku-5-5").expect("haiku 5.5 routes direct");
        assert_eq!(d.provider_id, "anthropic");
        assert_eq!(d.api_format, "Anthropic");
        assert!(d.strip_fields.contains(&"temperature"), "haiku 5.5 rejects temperature");
    }

    #[test]
    fn lookup_trims_and_ignores_case() {
        assert!(direct_model("  Claude-Haiku-5-5 ").is_some());
    }

    #[test]
    fn gateway_models_are_not_direct() {
        for m in [
            "gpt-6-luna",
            "gpt-6-luna-fast",
            "groq-gpt-oss-120b",
            "claude-haiku-4-5",
            "claude-haiku-5-5-x",
            "",
        ] {
            assert!(direct_model(m).is_none(), "{m:?} must stay on the gateway");
        }
    }

    #[test]
    fn every_entry_is_well_formed() {
        for d in DIRECT_MODELS {
            assert!(!d.model.is_empty() && !d.provider_id.is_empty() && !d.description.is_empty());
            assert_eq!(d.model, d.model.trim());
        }
    }
}

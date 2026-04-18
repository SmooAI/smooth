use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

use crate::llm::{ApiFormat, LlmConfig};

/// Preset model configurations for common provider setups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    /// Smoo AI Gateway — the hosted LiteLLM-backed gateway run by Smoo AI.
    /// Handles billing, moderation, governance, and upstream provider
    /// selection on the server side so callers only need one API key.
    /// This is the recommended default for most users.
    SmoaiGateway,
    /// OpenRouter + Chinese frontier models — cheapest option
    OpenRouterLowCost,
    /// LLM Gateway + Chinese frontier models — cheapest via gateway
    LlmGatewayLowCost,
    /// OpenAI models — GPT-4o/o3
    OpenAI,
    /// Anthropic models — Claude Opus/Sonnet
    Anthropic,
}

impl Preset {
    /// All available preset names. The first entry is the recommended
    /// default — `th auth login` shows them in this order.
    pub const ALL: &[(&str, &str, &str)] = &[
        (
            "smooai-gateway",
            "Smoo AI Gateway (recommended)",
            "Hosted LiteLLM gateway run by Smoo AI — billing, moderation, governance, 100+ models. One key, one URL, no config.",
        ),
        (
            "openrouter-low-cost",
            "OpenRouter Low Cost",
            "GLM-5.1 thinking (#1 SWE-Bench Pro), MiniMax-M2.7 coding (56% SWE-Pro, 10B params), DeepSeek-V3.2 default",
        ),
        (
            "llmgateway-low-cost",
            "LLM Gateway Low Cost",
            "GLM-5 thinking, MiniMax-M2.7 coding, DeepSeek-V3.2 default — unified billing, 224 models",
        ),
        ("openai", "OpenAI", "o3-mini thinking, GPT-4o coding — OpenAI ecosystem"),
        ("anthropic", "Anthropic", "Claude Opus thinking, Sonnet coding — highest quality"),
    ];

    /// Parse a preset name from string.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "smooai-gateway" | "smooai" | "gateway" => Some(Self::SmoaiGateway),
            "openrouter-low-cost" | "low-cost" => Some(Self::OpenRouterLowCost),
            "llmgateway-low-cost" | "gateway-low-cost" => Some(Self::LlmGatewayLowCost),
            "openai" | "codex" => Some(Self::OpenAI),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }

    /// The provider ID this preset requires.
    pub fn provider_id(&self) -> &str {
        match self {
            Self::SmoaiGateway => "smooai-gateway",
            Self::OpenRouterLowCost => "openrouter",
            Self::LlmGatewayLowCost => "llmgateway",
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
        }
    }
}

/// Configuration for a single LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub api_url: String,
    pub api_key: String,
    pub api_format: ApiFormat,
    pub default_model: String,
}

impl ProviderConfig {
    /// OpenRouter — OpenAI-compatible proxy for many models.
    pub fn openrouter(api_key: impl Into<String>) -> Self {
        Self {
            id: "openrouter".into(),
            api_url: "https://openrouter.ai/api/v1".into(),
            api_key: api_key.into(),
            api_format: ApiFormat::OpenAiCompat,
            default_model: "openai/gpt-4o".into(),
        }
    }

    /// OpenAI direct API.
    pub fn openai(api_key: impl Into<String>) -> Self {
        Self {
            id: "openai".into(),
            api_url: "https://api.openai.com/v1".into(),
            api_key: api_key.into(),
            api_format: ApiFormat::OpenAiCompat,
            default_model: "gpt-4o".into(),
        }
    }

    /// Anthropic native API.
    pub fn anthropic(api_key: impl Into<String>) -> Self {
        Self {
            id: "anthropic".into(),
            api_url: "https://api.anthropic.com/v1".into(),
            api_key: api_key.into(),
            api_format: ApiFormat::Anthropic,
            default_model: "claude-sonnet-4-20250514".into(),
        }
    }

    /// Local Ollama instance — no API key needed.
    pub fn ollama() -> Self {
        Self {
            id: "ollama".into(),
            api_url: "http://localhost:11434/v1".into(),
            api_key: String::new(),
            api_format: ApiFormat::OpenAiCompat,
            default_model: "llama3".into(),
        }
    }

    /// Google Gemini API.
    pub fn google(api_key: impl Into<String>) -> Self {
        Self {
            id: "google".into(),
            api_url: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
            api_key: api_key.into(),
            api_format: ApiFormat::OpenAiCompat,
            default_model: "gemini-2.0-flash".into(),
        }
    }

    /// Kimi Code — OpenAI-compatible API.
    /// Kimi — Moonshot AI's general-purpose API (OpenAI-compatible).
    pub fn kimi(api_key: impl Into<String>) -> Self {
        Self {
            id: "kimi".into(),
            api_url: "https://api.moonshot.ai/v1".into(),
            api_key: api_key.into(),
            api_format: ApiFormat::OpenAiCompat,
            default_model: "kimi-k2.5".into(),
        }
    }

    /// LLM Gateway — unified API for 210+ models from 25+ providers.
    pub fn llmgateway(api_key: impl Into<String>) -> Self {
        Self {
            id: "llmgateway".into(),
            api_url: "https://api.llmgateway.io/v1".into(),
            api_key: api_key.into(),
            api_format: ApiFormat::OpenAiCompat,
            default_model: "openai/gpt-4o".into(),
        }
    }

    /// Smoo AI Gateway — the hosted LiteLLM-backed gateway run by Smoo AI.
    ///
    /// One API key, one URL, OpenAI-compatible. The gateway handles
    /// provider selection, billing, moderation, governance, and cost
    /// tracking on the server side. Consumers reference models by
    /// semantic aliases (`smooth-coding`, `smooth-judge`, …) that the
    /// gateway's LiteLLM config maps to whichever underlying model is
    /// currently best — upgrades ship server-side with no client
    /// release needed.
    ///
    /// The base URL is configurable via the `SMOOAI_GATEWAY_URL`
    /// environment variable for self-hosted installs or dev overrides.
    /// Defaults to the production endpoint.
    pub fn smooai_gateway(api_key: impl Into<String>) -> Self {
        let api_url = std::env::var("SMOOAI_GATEWAY_URL").unwrap_or_else(|_| "https://llm.smooai.com/v1".into());
        Self {
            id: "smooai-gateway".into(),
            api_url,
            api_key: api_key.into(),
            api_format: ApiFormat::OpenAiCompat,
            default_model: "smooth-default".into(),
        }
    }

    /// Kimi Code — Moonshot's coding-optimized API (Anthropic-compatible).
    pub fn kimi_code(api_key: impl Into<String>) -> Self {
        Self {
            id: "kimi-code".into(),
            api_url: "https://api.kimi.com/coding/v1".into(),
            api_key: api_key.into(),
            api_format: ApiFormat::Anthropic,
            default_model: "kimi-for-coding".into(),
        }
    }
}

/// Activity type that determines which model slot to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Activity {
    Thinking,
    Coding,
    Planning,
    Reviewing,
    Judge,
    Summarize,
}

/// A model slot binding a provider ID and model name, with optional fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSlot {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Box<Self>>,
}

impl ModelSlot {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            fallback: None,
        }
    }

    pub fn with_fallback(mut self, fallback: Self) -> Self {
        self.fallback = Some(Box::new(fallback));
        self
    }
}

/// Per-activity model routing configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRouting {
    pub thinking: ModelSlot,
    pub coding: ModelSlot,
    pub planning: ModelSlot,
    pub reviewing: ModelSlot,
    pub judge: ModelSlot,
    pub summarize: ModelSlot,
    pub default: ModelSlot,
}

impl Default for ModelRouting {
    fn default() -> Self {
        Self {
            thinking: ModelSlot::new("openrouter", "deepseek/deepseek-r1"),
            coding: ModelSlot::new("openrouter", "openai/gpt-4o"),
            planning: ModelSlot::new("openrouter", "moonshot/kimi-k2.5"),
            reviewing: ModelSlot::new("openrouter", "z-ai/glm-5.1"),
            judge: ModelSlot::new("openrouter", "google/gemini-2.5-flash"),
            summarize: ModelSlot::new("openrouter", "minimax/minimax-m2.5"),
            default: ModelSlot::new("openrouter", "openai/gpt-4o"),
        }
    }
}

impl ModelRouting {
    /// Get the model slot for a given activity.
    pub fn slot_for(&self, activity: Activity) -> &ModelSlot {
        match activity {
            Activity::Thinking => &self.thinking,
            Activity::Coding => &self.coding,
            Activity::Planning => &self.planning,
            Activity::Reviewing => &self.reviewing,
            Activity::Judge => &self.judge,
            Activity::Summarize => &self.summarize,
        }
    }
}

/// Serializable form for save/load.
#[derive(Debug, Serialize, Deserialize)]
struct RegistryFile {
    providers: Vec<ProviderConfig>,
    routing: ModelRouting,
}

/// Registry of LLM providers with per-activity model routing.
pub struct ProviderRegistry {
    providers: HashMap<String, ProviderConfig>,
    pub routing: ModelRouting,
}

impl ProviderRegistry {
    /// Create a registry pre-configured with a preset model configuration.
    ///
    /// Each preset registers the appropriate provider and sets up per-activity
    /// model routing optimized for the preset's goals (cost, quality, etc.).
    pub fn from_preset(preset: Preset, api_key: &str) -> Self {
        let mut registry = Self::new();

        match preset {
            Preset::SmoaiGateway => {
                // Smoo AI Gateway uses semantic model aliases that the
                // server-side LiteLLM config maps to whichever underlying
                // model is currently best for each activity. Changing the
                // underlying model is a server-side deploy — no client
                // release needed.
                //
                // Aliases:
                //   smooth-thinking  → frontier reasoning model
                //   smooth-coding    → coding workhorse
                //   smooth-planning  → planning / architecture model
                //   smooth-reviewing → code review / critique model
                //   smooth-judge     → cheap fast judge for Narc + guardrails
                //   smooth-summarize → cheap summarizer
                //   smooth-default   → balanced default
                registry.register_provider(ProviderConfig::smooai_gateway(api_key));
                registry.routing = ModelRouting {
                    thinking: ModelSlot::new("smooai-gateway", "smooth-thinking"),
                    coding: ModelSlot::new("smooai-gateway", "smooth-coding"),
                    planning: ModelSlot::new("smooai-gateway", "smooth-planning"),
                    reviewing: ModelSlot::new("smooai-gateway", "smooth-reviewing"),
                    judge: ModelSlot::new("smooai-gateway", "smooth-judge"),
                    summarize: ModelSlot::new("smooai-gateway", "smooth-summarize"),
                    default: ModelSlot::new("smooai-gateway", "smooth-default"),
                };
            }
            Preset::OpenRouterLowCost => {
                // OpenRouter: provider-prefixed model IDs
                // GLM-5.1 for thinking (#1 SWE-Bench Pro 58.4%)
                // MiniMax-M2.7 for coding (56.2% SWE-Pro, 10B active params, cheapest tier-1)
                // DeepSeek-V3.2 as default ($0.28/M, great all-rounder)
                registry.register_provider(ProviderConfig::openrouter(api_key));
                registry.routing = ModelRouting {
                    thinking: ModelSlot::new("openrouter", "z-ai/glm-5.1"),
                    coding: ModelSlot::new("openrouter", "minimax/minimax-m2.7").with_fallback(ModelSlot::new("openrouter", "minimax/minimax-m2.5")),
                    planning: ModelSlot::new("openrouter", "z-ai/glm-5.1"),
                    reviewing: ModelSlot::new("openrouter", "deepseek/deepseek-v3.2"),
                    judge: ModelSlot::new("openrouter", "google/gemini-2.5-flash"),
                    summarize: ModelSlot::new("openrouter", "deepseek/deepseek-v3.2"),
                    default: ModelSlot::new("openrouter", "deepseek/deepseek-v3.2"),
                };
            }
            Preset::LlmGatewayLowCost => {
                // LLM Gateway: bare model names
                registry.register_provider(ProviderConfig::llmgateway(api_key));
                registry.routing = ModelRouting {
                    thinking: ModelSlot::new("llmgateway", "glm-5"),
                    coding: ModelSlot::new("llmgateway", "minimax-m2.7").with_fallback(ModelSlot::new("llmgateway", "minimax-m2.5")),
                    planning: ModelSlot::new("llmgateway", "glm-5"),
                    reviewing: ModelSlot::new("llmgateway", "deepseek-v3.2"),
                    judge: ModelSlot::new("llmgateway", "gemini-2.5-flash"),
                    summarize: ModelSlot::new("llmgateway", "deepseek-v3.2"),
                    default: ModelSlot::new("llmgateway", "deepseek-v3.2"),
                };
            }
            Preset::OpenAI => {
                registry.register_provider(ProviderConfig::openai(api_key));
                registry.routing = ModelRouting {
                    thinking: ModelSlot::new("openai", "o3-mini"),
                    coding: ModelSlot::new("openai", "gpt-4o"),
                    planning: ModelSlot::new("openai", "gpt-4o"),
                    reviewing: ModelSlot::new("openai", "gpt-4o"),
                    judge: ModelSlot::new("openai", "gpt-4o-mini"),
                    summarize: ModelSlot::new("openai", "gpt-4o-mini"),
                    default: ModelSlot::new("openai", "gpt-4o"),
                };
            }
            Preset::Anthropic => {
                registry.register_provider(ProviderConfig::anthropic(api_key));
                registry.routing = ModelRouting {
                    thinking: ModelSlot::new("anthropic", "claude-opus-4-20250514"),
                    coding: ModelSlot::new("anthropic", "claude-sonnet-4-20250514"),
                    planning: ModelSlot::new("anthropic", "claude-sonnet-4-20250514"),
                    reviewing: ModelSlot::new("anthropic", "claude-sonnet-4-20250514"),
                    judge: ModelSlot::new("anthropic", "claude-haiku-4-5-20251001"),
                    summarize: ModelSlot::new("anthropic", "claude-haiku-4-5-20251001"),
                    default: ModelSlot::new("anthropic", "claude-sonnet-4-20250514"),
                };
            }
        }

        registry
    }

    /// Create a new empty registry with default routing.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            routing: ModelRouting::default(),
        }
    }

    /// Register a provider configuration.
    pub fn register_provider(&mut self, config: ProviderConfig) {
        self.providers.insert(config.id.clone(), config);
    }

    /// Remove a provider by ID.
    pub fn remove_provider(&mut self, id: &str) {
        self.providers.remove(id);
    }

    /// Set all routing slots to use the given provider with its default model.
    pub fn set_default_provider(&mut self, provider_id: &str) {
        let model = self.providers.get(provider_id).map(|p| p.default_model.clone()).unwrap_or_default();
        let slot = ModelSlot::new(provider_id, &model);
        self.routing = ModelRouting {
            thinking: slot.clone(),
            coding: slot.clone(),
            planning: slot.clone(),
            reviewing: slot.clone(),
            judge: slot.clone(),
            summarize: slot.clone(),
            default: slot,
        };
    }

    /// Look up a provider by ID.
    pub fn get_provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.get(id)
    }

    /// List all registered provider IDs.
    pub fn list_providers(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.providers.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }

    /// Set custom routing.
    pub fn with_routing(mut self, routing: ModelRouting) -> Self {
        self.routing = routing;
        self
    }

    /// Resolve a `ModelSlot` to an `LlmConfig`, walking the fallback chain
    /// if the primary provider is not registered.
    fn resolve_slot(&self, slot: &ModelSlot) -> anyhow::Result<LlmConfig> {
        if let Some(provider) = self.providers.get(&slot.provider) {
            return Ok(LlmConfig {
                api_url: provider.api_url.clone(),
                api_key: provider.api_key.clone(),
                model: slot.model.clone(),
                max_tokens: 8192,
                temperature: 0.0,
                retry_policy: crate::llm::RetryPolicy::default(),
                api_format: provider.api_format.clone(),
            });
        }

        // Try fallback chain
        if let Some(ref fallback) = slot.fallback {
            return self.resolve_slot(fallback);
        }

        Err(anyhow!("provider '{}' not registered and no fallback available", slot.provider))
    }

    /// Get an `LlmConfig` for a specific activity.
    ///
    /// # Errors
    ///
    /// Returns an error if the provider for the activity's model slot (and all
    /// fallbacks) is not registered.
    pub fn llm_config_for(&self, activity: Activity) -> anyhow::Result<LlmConfig> {
        let slot = self.routing.slot_for(activity);
        self.resolve_slot(slot)
    }

    /// Get the default `LlmConfig`.
    ///
    /// # Errors
    ///
    /// Returns an error if the default provider is not registered and has no fallback.
    pub fn default_llm_config(&self) -> anyhow::Result<LlmConfig> {
        self.resolve_slot(&self.routing.default)
    }

    /// Load registry from a JSON file (e.g. `~/.smooth/providers.json`).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or contains invalid JSON.
    pub fn load_from_file(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file: RegistryFile = serde_json::from_str(&contents).with_context(|| format!("parsing {}", path.display()))?;

        let mut registry = Self::new().with_routing(file.routing);
        for provider in file.providers {
            registry.register_provider(provider);
        }
        Ok(registry)
    }

    /// Save registry to a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_to_file(&self, path: &Path) -> anyhow::Result<()> {
        let file = RegistryFile {
            providers: self.providers.values().cloned().collect(),
            routing: self.routing.clone(),
        };
        let json = serde_json::to_string_pretty(&file)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Load a minimal registry from environment variables.
    ///
    /// Reads `SMOOTH_PROVIDER` (defaults to `"openrouter"`), `SMOOTH_API_KEY`,
    /// and optionally `SMOOTH_MODEL`.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("SMOOTH_API_KEY").ok()?;
        let provider_id = std::env::var("SMOOTH_PROVIDER").unwrap_or_else(|_| "openrouter".into());
        let model = std::env::var("SMOOTH_MODEL").ok();

        let config = match provider_id.as_str() {
            "openai" => ProviderConfig::openai(&api_key),
            "anthropic" => ProviderConfig::anthropic(&api_key),
            "ollama" => {
                let mut c = ProviderConfig::ollama();
                c.api_key = api_key;
                c
            }
            "google" => ProviderConfig::google(&api_key),
            "kimi" => ProviderConfig::kimi(&api_key),
            "kimi-code" => ProviderConfig::kimi_code(&api_key),
            "llmgateway" => ProviderConfig::llmgateway(&api_key),
            _ => ProviderConfig::openrouter(&api_key),
        };

        let default_model = model.unwrap_or_else(|| config.default_model.clone());

        let mut registry = Self::new();
        registry.register_provider(config);

        // Update default routing to use this provider
        let slot = ModelSlot::new(&provider_id, &default_model);
        registry.routing = ModelRouting {
            thinking: slot.clone(),
            coding: slot.clone(),
            planning: slot.clone(),
            reviewing: slot.clone(),
            judge: slot.clone(),
            summarize: slot.clone(),
            default: slot,
        };

        Some(registry)
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1. ProviderConfig presets have correct URLs
    #[test]
    fn provider_config_presets_have_correct_urls() {
        let or = ProviderConfig::openrouter("key");
        assert_eq!(or.api_url, "https://openrouter.ai/api/v1");
        assert_eq!(or.api_format, ApiFormat::OpenAiCompat);

        let oai = ProviderConfig::openai("key");
        assert_eq!(oai.api_url, "https://api.openai.com/v1");
        assert_eq!(oai.api_format, ApiFormat::OpenAiCompat);

        let ollama = ProviderConfig::ollama();
        assert_eq!(ollama.api_url, "http://localhost:11434/v1");
        assert!(ollama.api_key.is_empty());
        assert_eq!(ollama.api_format, ApiFormat::OpenAiCompat);

        let anthropic = ProviderConfig::anthropic("key");
        assert_eq!(anthropic.api_url, "https://api.anthropic.com/v1");
        assert_eq!(anthropic.api_format, ApiFormat::Anthropic);

        let google = ProviderConfig::google("key");
        assert!(google.api_url.contains("generativelanguage.googleapis.com"));
        assert_eq!(google.api_format, ApiFormat::OpenAiCompat);

        let kimi = ProviderConfig::kimi("key");
        assert_eq!(kimi.api_url, "https://api.moonshot.ai/v1");
        assert_eq!(kimi.default_model, "kimi-k2.5");
        assert_eq!(kimi.api_format, ApiFormat::OpenAiCompat);

        let kimi_code = ProviderConfig::kimi_code("key");
        assert_eq!(kimi_code.api_url, "https://api.kimi.com/coding/v1");
        assert_eq!(kimi_code.default_model, "kimi-for-coding");
        assert_eq!(kimi_code.api_format, ApiFormat::Anthropic);
    }

    // 2. ModelSlot creation + serialization
    #[test]
    fn model_slot_creation_and_serialization() {
        let slot = ModelSlot::new("openrouter", "openai/gpt-4o");
        assert_eq!(slot.provider, "openrouter");
        assert_eq!(slot.model, "openai/gpt-4o");
        assert!(slot.fallback.is_none());

        let json = serde_json::to_string(&slot).unwrap();
        let deserialized: ModelSlot = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.provider, "openrouter");
        assert_eq!(deserialized.model, "openai/gpt-4o");

        // Verify fallback is omitted when None
        assert!(!json.contains("fallback"));
    }

    // 3. ModelRouting default has all activities set
    #[test]
    fn model_routing_default_has_all_activities() {
        let routing = ModelRouting::default();
        assert_eq!(routing.thinking.model, "deepseek/deepseek-r1");
        assert_eq!(routing.coding.model, "openai/gpt-4o");
        assert_eq!(routing.planning.model, "moonshot/kimi-k2.5");
        assert_eq!(routing.reviewing.model, "z-ai/glm-5.1");
        assert_eq!(routing.judge.model, "google/gemini-2.5-flash");
        assert_eq!(routing.summarize.model, "minimax/minimax-m2.5");
        assert_eq!(routing.default.model, "openai/gpt-4o");
    }

    // 4. ProviderRegistry register + get
    #[test]
    fn registry_register_and_get() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(ProviderConfig::openrouter("test-key"));

        let provider = registry.get_provider("openrouter").unwrap();
        assert_eq!(provider.api_key, "test-key");
        assert_eq!(provider.id, "openrouter");

        assert!(registry.get_provider("nonexistent").is_none());
    }

    // 5. ProviderRegistry list_providers
    #[test]
    fn registry_list_providers() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(ProviderConfig::openrouter("k1"));
        registry.register_provider(ProviderConfig::openai("k2"));
        registry.register_provider(ProviderConfig::ollama());

        let ids = registry.list_providers();
        assert_eq!(ids.len(), 3);
        // Sorted alphabetically
        assert_eq!(ids, vec!["ollama", "openai", "openrouter"]);
    }

    // 6. llm_config_for returns correct model for each activity
    #[test]
    fn llm_config_for_returns_correct_model() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(ProviderConfig::openrouter("test-key"));

        let config = registry.llm_config_for(Activity::Thinking).unwrap();
        assert_eq!(config.model, "deepseek/deepseek-r1");
        assert_eq!(config.api_url, "https://openrouter.ai/api/v1");

        let config = registry.llm_config_for(Activity::Coding).unwrap();
        assert_eq!(config.model, "openai/gpt-4o");

        let config = registry.llm_config_for(Activity::Judge).unwrap();
        assert_eq!(config.model, "google/gemini-2.5-flash");
    }

    // 7. llm_config_for falls back when provider missing
    #[test]
    fn llm_config_for_falls_back_when_provider_missing() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(ProviderConfig::openai("fallback-key"));

        // Default routing uses "openrouter" which is not registered.
        // Set up a slot with fallback to openai.
        let slot = ModelSlot::new("openrouter", "openai/gpt-4o").with_fallback(ModelSlot::new("openai", "gpt-4o"));

        registry.routing.coding = slot;

        let config = registry.llm_config_for(Activity::Coding).unwrap();
        assert_eq!(config.api_url, "https://api.openai.com/v1");
        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.api_key, "fallback-key");
    }

    // 8. default_llm_config works
    #[test]
    fn default_llm_config_works() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(ProviderConfig::openrouter("default-key"));

        let config = registry.default_llm_config().unwrap();
        assert_eq!(config.model, "openai/gpt-4o");
        assert_eq!(config.api_key, "default-key");
    }

    // 9. save_to_file + load_from_file roundtrip
    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");

        let mut registry = ProviderRegistry::new();
        registry.register_provider(ProviderConfig::openrouter("rt-key"));
        registry.register_provider(ProviderConfig::openai("oai-key"));

        registry.save_to_file(&path).unwrap();

        let loaded = ProviderRegistry::load_from_file(&path).unwrap();
        assert_eq!(loaded.list_providers().len(), 2);

        let or = loaded.get_provider("openrouter").unwrap();
        assert_eq!(or.api_key, "rt-key");

        let oai = loaded.get_provider("openai").unwrap();
        assert_eq!(oai.api_key, "oai-key");

        // Routing survives roundtrip
        let config = loaded.llm_config_for(Activity::Thinking).unwrap();
        assert_eq!(config.model, "deepseek/deepseek-r1");
    }

    // 10. from_env reads SMOOTH_PROVIDER and SMOOTH_API_KEY
    #[test]
    fn from_env_reads_variables() {
        // Save and restore env vars
        let prev_key = std::env::var("SMOOTH_API_KEY").ok();
        let prev_provider = std::env::var("SMOOTH_PROVIDER").ok();
        let prev_model = std::env::var("SMOOTH_MODEL").ok();

        std::env::set_var("SMOOTH_API_KEY", "env-test-key");
        std::env::set_var("SMOOTH_PROVIDER", "openai");
        std::env::remove_var("SMOOTH_MODEL");

        let registry = ProviderRegistry::from_env().expect("should load from env");
        let provider = registry.get_provider("openai").unwrap();
        assert_eq!(provider.api_key, "env-test-key");

        let config = registry.default_llm_config().unwrap();
        assert_eq!(config.model, "gpt-4o"); // default model for openai

        // Restore env
        match prev_key {
            Some(v) => std::env::set_var("SMOOTH_API_KEY", v),
            None => std::env::remove_var("SMOOTH_API_KEY"),
        }
        match prev_provider {
            Some(v) => std::env::set_var("SMOOTH_PROVIDER", v),
            None => std::env::remove_var("SMOOTH_PROVIDER"),
        }
        match prev_model {
            Some(v) => std::env::set_var("SMOOTH_MODEL", v),
            None => std::env::remove_var("SMOOTH_MODEL"),
        }
    }

    // 11. Activity serialization
    #[test]
    fn activity_serialization() {
        let activity = Activity::Thinking;
        let json = serde_json::to_string(&activity).unwrap();
        assert_eq!(json, "\"Thinking\"");

        let deserialized: Activity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, Activity::Thinking);

        // All variants roundtrip
        for activity in [
            Activity::Thinking,
            Activity::Coding,
            Activity::Planning,
            Activity::Reviewing,
            Activity::Judge,
            Activity::Summarize,
        ] {
            let json = serde_json::to_string(&activity).unwrap();
            let rt: Activity = serde_json::from_str(&json).unwrap();
            assert_eq!(rt, activity);
        }
    }

    // 12. ModelSlot with fallback chain
    #[test]
    fn model_slot_with_fallback_chain() {
        let slot =
            ModelSlot::new("primary", "model-a").with_fallback(ModelSlot::new("secondary", "model-b").with_fallback(ModelSlot::new("tertiary", "model-c")));

        assert_eq!(slot.provider, "primary");
        let fb1 = slot.fallback.as_ref().unwrap();
        assert_eq!(fb1.provider, "secondary");
        assert_eq!(fb1.model, "model-b");
        let fb2 = fb1.fallback.as_ref().unwrap();
        assert_eq!(fb2.provider, "tertiary");
        assert_eq!(fb2.model, "model-c");
        assert!(fb2.fallback.is_none());

        // Serialization roundtrip preserves chain
        let json = serde_json::to_string(&slot).unwrap();
        let deserialized: ModelSlot = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.provider, "primary");
        assert_eq!(deserialized.fallback.as_ref().unwrap().provider, "secondary");
        assert_eq!(deserialized.fallback.as_ref().unwrap().fallback.as_ref().unwrap().provider, "tertiary");

        // Registry resolves through the chain
        let mut registry = ProviderRegistry::new();
        registry.register_provider(ProviderConfig {
            id: "tertiary".into(),
            api_url: "https://tertiary.example.com/v1".into(),
            api_key: "t-key".into(),
            api_format: ApiFormat::OpenAiCompat,
            default_model: "model-c".into(),
        });

        registry.routing.coding = slot;
        let config = registry.llm_config_for(Activity::Coding).unwrap();
        assert_eq!(config.api_url, "https://tertiary.example.com/v1");
        assert_eq!(config.model, "model-c");
    }

    // 13. LowCost preset creates correct routing
    #[test]
    fn low_cost_preset_creates_correct_routing() {
        let registry = ProviderRegistry::from_preset(Preset::OpenRouterLowCost, "or-key");

        let thinking = registry.llm_config_for(Activity::Thinking).unwrap();
        assert_eq!(thinking.model, "z-ai/glm-5.1");
        assert_eq!(thinking.api_url, "https://openrouter.ai/api/v1");

        let coding = registry.llm_config_for(Activity::Coding).unwrap();
        assert_eq!(coding.model, "minimax/minimax-m2.7");

        let planning = registry.llm_config_for(Activity::Planning).unwrap();
        assert_eq!(planning.model, "z-ai/glm-5.1");

        let reviewing = registry.llm_config_for(Activity::Reviewing).unwrap();
        assert_eq!(reviewing.model, "deepseek/deepseek-v3.2");

        let judge = registry.llm_config_for(Activity::Judge).unwrap();
        assert_eq!(judge.model, "google/gemini-2.5-flash");

        let summarize = registry.llm_config_for(Activity::Summarize).unwrap();
        assert_eq!(summarize.model, "deepseek/deepseek-v3.2");

        let default = registry.default_llm_config().unwrap();
        assert_eq!(default.model, "deepseek/deepseek-v3.2");
    }

    // 14. Codex preset creates correct routing
    #[test]
    fn codex_preset_creates_correct_routing() {
        let registry = ProviderRegistry::from_preset(Preset::OpenAI, "oai-key");

        let thinking = registry.llm_config_for(Activity::Thinking).unwrap();
        assert_eq!(thinking.model, "o3-mini");
        assert_eq!(thinking.api_url, "https://api.openai.com/v1");

        let coding = registry.llm_config_for(Activity::Coding).unwrap();
        assert_eq!(coding.model, "gpt-4o");

        let planning = registry.llm_config_for(Activity::Planning).unwrap();
        assert_eq!(planning.model, "gpt-4o");

        let reviewing = registry.llm_config_for(Activity::Reviewing).unwrap();
        assert_eq!(reviewing.model, "gpt-4o");

        let judge = registry.llm_config_for(Activity::Judge).unwrap();
        assert_eq!(judge.model, "gpt-4o-mini");

        let summarize = registry.llm_config_for(Activity::Summarize).unwrap();
        assert_eq!(summarize.model, "gpt-4o-mini");

        let default = registry.default_llm_config().unwrap();
        assert_eq!(default.model, "gpt-4o");
    }

    // Serialize tests that mutate `SMOOAI_GATEWAY_URL` — cargo test runs
    // tests in parallel, so two tests touching the same process-global env
    // var race and either order can fail.
    fn smooai_gateway_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    // 14b. Smoo AI Gateway preset creates correct routing with semantic aliases
    #[test]
    fn smooai_gateway_preset_creates_correct_routing() {
        let _guard = smooai_gateway_env_lock().lock().unwrap_or_else(|e| e.into_inner());

        // Clear any host override for a deterministic assert on the default
        // production URL. We re-set it at the end so other tests see the
        // same state they started with.
        let prior = std::env::var("SMOOAI_GATEWAY_URL").ok();
        std::env::remove_var("SMOOAI_GATEWAY_URL");

        let registry = ProviderRegistry::from_preset(Preset::SmoaiGateway, "smooai-key");

        // Every slot routes to the `smooai-gateway` provider with a
        // semantic `smooth-*` alias. The alias → upstream model mapping
        // lives in the gateway's LiteLLM config, not here.
        let thinking = registry.llm_config_for(Activity::Thinking).unwrap();
        assert_eq!(thinking.model, "smooth-thinking");
        assert_eq!(thinking.api_url, "https://llm.smooai.com/v1");
        assert_eq!(thinking.api_key, "smooai-key");

        let coding = registry.llm_config_for(Activity::Coding).unwrap();
        assert_eq!(coding.model, "smooth-coding");

        let planning = registry.llm_config_for(Activity::Planning).unwrap();
        assert_eq!(planning.model, "smooth-planning");

        let reviewing = registry.llm_config_for(Activity::Reviewing).unwrap();
        assert_eq!(reviewing.model, "smooth-reviewing");

        let judge = registry.llm_config_for(Activity::Judge).unwrap();
        assert_eq!(judge.model, "smooth-judge");

        let summarize = registry.llm_config_for(Activity::Summarize).unwrap();
        assert_eq!(summarize.model, "smooth-summarize");

        let default = registry.default_llm_config().unwrap();
        assert_eq!(default.model, "smooth-default");

        // Restore any prior override.
        if let Some(v) = prior {
            std::env::set_var("SMOOAI_GATEWAY_URL", v);
        }
    }

    // 14c. SMOOAI_GATEWAY_URL env var overrides the default base URL
    #[test]
    fn smooai_gateway_respects_env_url_override() {
        let _guard = smooai_gateway_env_lock().lock().unwrap_or_else(|e| e.into_inner());

        let prior = std::env::var("SMOOAI_GATEWAY_URL").ok();
        std::env::set_var("SMOOAI_GATEWAY_URL", "https://llm.dev.smooai.com/v1");

        let registry = ProviderRegistry::from_preset(Preset::SmoaiGateway, "dev-key");
        let cfg = registry.default_llm_config().unwrap();
        assert_eq!(cfg.api_url, "https://llm.dev.smooai.com/v1");
        assert_eq!(cfg.api_key, "dev-key");

        // Restore prior state.
        match prior {
            Some(v) => std::env::set_var("SMOOAI_GATEWAY_URL", v),
            None => std::env::remove_var("SMOOAI_GATEWAY_URL"),
        }
    }

    // 15. Anthropic preset creates correct routing
    #[test]
    fn anthropic_preset_creates_correct_routing() {
        let registry = ProviderRegistry::from_preset(Preset::Anthropic, "ant-key");

        let thinking = registry.llm_config_for(Activity::Thinking).unwrap();
        assert_eq!(thinking.model, "claude-opus-4-20250514");
        assert_eq!(thinking.api_url, "https://api.anthropic.com/v1");
        assert_eq!(thinking.api_format, ApiFormat::Anthropic);

        let coding = registry.llm_config_for(Activity::Coding).unwrap();
        assert_eq!(coding.model, "claude-sonnet-4-20250514");

        let judge = registry.llm_config_for(Activity::Judge).unwrap();
        assert_eq!(judge.model, "claude-haiku-4-5-20251001");

        let summarize = registry.llm_config_for(Activity::Summarize).unwrap();
        assert_eq!(summarize.model, "claude-haiku-4-5-20251001");

        let default = registry.default_llm_config().unwrap();
        assert_eq!(default.model, "claude-sonnet-4-20250514");
    }

    // 16. from_preset registers the provider
    #[test]
    fn from_preset_registers_provider() {
        let smooai = ProviderRegistry::from_preset(Preset::SmoaiGateway, "sg-key");
        assert!(smooai.get_provider("smooai-gateway").is_some());
        assert_eq!(smooai.get_provider("smooai-gateway").unwrap().api_key, "sg-key");

        let low_cost = ProviderRegistry::from_preset(Preset::OpenRouterLowCost, "lc-key");
        assert!(low_cost.get_provider("openrouter").is_some());
        assert_eq!(low_cost.get_provider("openrouter").unwrap().api_key, "lc-key");

        let codex = ProviderRegistry::from_preset(Preset::OpenAI, "cx-key");
        assert!(codex.get_provider("openai").is_some());
        assert_eq!(codex.get_provider("openai").unwrap().api_key, "cx-key");

        let anthropic = ProviderRegistry::from_preset(Preset::Anthropic, "an-key");
        assert!(anthropic.get_provider("anthropic").is_some());
        assert_eq!(anthropic.get_provider("anthropic").unwrap().api_key, "an-key");
    }

    // 16b. Preset names and aliases parse correctly
    #[test]
    fn preset_from_name_recognizes_smooai_gateway_aliases() {
        assert_eq!(Preset::from_name("smooai-gateway"), Some(Preset::SmoaiGateway));
        assert_eq!(Preset::from_name("smooai"), Some(Preset::SmoaiGateway));
        assert_eq!(Preset::from_name("gateway"), Some(Preset::SmoaiGateway));
        assert_eq!(Preset::from_name("bogus"), None);
    }

    // 16c. Smoo AI Gateway is listed first in Preset::ALL (recommended default)
    #[test]
    fn smooai_gateway_is_first_in_preset_list() {
        let first = Preset::ALL.first().expect("Preset::ALL must not be empty");
        assert_eq!(first.0, "smooai-gateway");
        assert!(first.1.contains("recommended"), "label should say recommended: {:?}", first.1);
    }

    // 17. llm_config_for works with preset
    #[test]
    fn llm_config_for_works_with_preset() {
        let registry = ProviderRegistry::from_preset(Preset::OpenAI, "test-key");

        // Every activity should resolve without error
        for activity in [
            Activity::Thinking,
            Activity::Coding,
            Activity::Planning,
            Activity::Reviewing,
            Activity::Judge,
            Activity::Summarize,
        ] {
            let config = registry.llm_config_for(activity);
            assert!(config.is_ok(), "Activity {activity:?} should resolve for Codex preset");
            assert_eq!(config.unwrap().api_key, "test-key");
        }

        let default = registry.default_llm_config();
        assert!(default.is_ok());
        assert_eq!(default.unwrap().api_key, "test-key");
    }

    // 18. Preset serialization roundtrip
    #[test]
    fn preset_serialization_roundtrip() {
        for preset in [Preset::OpenRouterLowCost, Preset::LlmGatewayLowCost, Preset::OpenAI, Preset::Anthropic] {
            let json = serde_json::to_string(&preset).unwrap();
            let deserialized: Preset = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, preset);
        }

        // Verify specific serialized values
        assert_eq!(serde_json::to_string(&Preset::OpenRouterLowCost).unwrap(), "\"OpenRouterLowCost\"");
        assert_eq!(serde_json::to_string(&Preset::OpenAI).unwrap(), "\"OpenAI\"");
        assert_eq!(serde_json::to_string(&Preset::Anthropic).unwrap(), "\"Anthropic\"");
    }
}

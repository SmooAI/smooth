use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

use crate::llm::{ApiFormat, LlmConfig};

/// Preset model configurations for common provider setups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    /// OpenRouter + Chinese models — cheapest option
    LowCost,
    /// OpenAI models — uses Codex/ChatGPT subscription
    Codex,
    /// Anthropic models — highest quality, most expensive
    Anthropic,
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
    pub fn kimi(api_key: impl Into<String>) -> Self {
        Self {
            id: "kimi".into(),
            api_url: "https://api.kimicode.com/v1".into(),
            api_key: api_key.into(),
            api_format: ApiFormat::OpenAiCompat,
            default_model: "kimi-k2.5".into(),
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
            reviewing: ModelSlot::new("openrouter", "zhipu/glm-5.1"),
            judge: ModelSlot::new("openrouter", "google/gemini-flash-2.0"),
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
    routing: ModelRouting,
}

impl ProviderRegistry {
    /// Create a registry pre-configured with a preset model configuration.
    ///
    /// Each preset registers the appropriate provider and sets up per-activity
    /// model routing optimized for the preset's goals (cost, quality, etc.).
    pub fn from_preset(preset: Preset, api_key: &str) -> Self {
        let mut registry = Self::new();

        match preset {
            Preset::LowCost => {
                registry.register_provider(ProviderConfig::openrouter(api_key));
                registry.routing = ModelRouting {
                    thinking: ModelSlot::new("openrouter", "deepseek/deepseek-r1"),
                    coding: ModelSlot::new("openrouter", "minimax/minimax-m2.5").with_fallback(ModelSlot::new("openrouter", "deepseek/deepseek-v3")),
                    planning: ModelSlot::new("openrouter", "moonshot/kimi-k2.5"),
                    reviewing: ModelSlot::new("openrouter", "zhipu/glm-5.1"),
                    judge: ModelSlot::new("openrouter", "google/gemini-flash-2.0"),
                    summarize: ModelSlot::new("openrouter", "minimax/minimax-m2.5"),
                    default: ModelSlot::new("openrouter", "deepseek/deepseek-v3"),
                };
            }
            Preset::Codex => {
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
        assert_eq!(kimi.api_url, "https://api.kimicode.com/v1");
        assert_eq!(kimi.default_model, "kimi-k2.5");
        assert_eq!(kimi.api_format, ApiFormat::OpenAiCompat);
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
        assert_eq!(routing.reviewing.model, "zhipu/glm-5.1");
        assert_eq!(routing.judge.model, "google/gemini-flash-2.0");
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
        assert_eq!(config.model, "google/gemini-flash-2.0");
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
        let registry = ProviderRegistry::from_preset(Preset::LowCost, "or-key");

        let thinking = registry.llm_config_for(Activity::Thinking).unwrap();
        assert_eq!(thinking.model, "deepseek/deepseek-r1");
        assert_eq!(thinking.api_url, "https://openrouter.ai/api/v1");

        let coding = registry.llm_config_for(Activity::Coding).unwrap();
        assert_eq!(coding.model, "minimax/minimax-m2.5");

        let planning = registry.llm_config_for(Activity::Planning).unwrap();
        assert_eq!(planning.model, "moonshot/kimi-k2.5");

        let reviewing = registry.llm_config_for(Activity::Reviewing).unwrap();
        assert_eq!(reviewing.model, "zhipu/glm-5.1");

        let judge = registry.llm_config_for(Activity::Judge).unwrap();
        assert_eq!(judge.model, "google/gemini-flash-2.0");

        let summarize = registry.llm_config_for(Activity::Summarize).unwrap();
        assert_eq!(summarize.model, "minimax/minimax-m2.5");

        let default = registry.default_llm_config().unwrap();
        assert_eq!(default.model, "deepseek/deepseek-v3");
    }

    // 14. Codex preset creates correct routing
    #[test]
    fn codex_preset_creates_correct_routing() {
        let registry = ProviderRegistry::from_preset(Preset::Codex, "oai-key");

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
        let low_cost = ProviderRegistry::from_preset(Preset::LowCost, "lc-key");
        assert!(low_cost.get_provider("openrouter").is_some());
        assert_eq!(low_cost.get_provider("openrouter").unwrap().api_key, "lc-key");

        let codex = ProviderRegistry::from_preset(Preset::Codex, "cx-key");
        assert!(codex.get_provider("openai").is_some());
        assert_eq!(codex.get_provider("openai").unwrap().api_key, "cx-key");

        let anthropic = ProviderRegistry::from_preset(Preset::Anthropic, "an-key");
        assert!(anthropic.get_provider("anthropic").is_some());
        assert_eq!(anthropic.get_provider("anthropic").unwrap().api_key, "an-key");
    }

    // 17. llm_config_for works with preset
    #[test]
    fn llm_config_for_works_with_preset() {
        let registry = ProviderRegistry::from_preset(Preset::Codex, "test-key");

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
        for preset in [Preset::LowCost, Preset::Codex, Preset::Anthropic] {
            let json = serde_json::to_string(&preset).unwrap();
            let deserialized: Preset = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, preset);
        }

        // Verify specific serialized values
        assert_eq!(serde_json::to_string(&Preset::LowCost).unwrap(), "\"LowCost\"");
        assert_eq!(serde_json::to_string(&Preset::Codex).unwrap(), "\"Codex\"");
        assert_eq!(serde_json::to_string(&Preset::Anthropic).unwrap(), "\"Anthropic\"");
    }
}

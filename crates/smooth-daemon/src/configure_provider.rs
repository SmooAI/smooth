//! `configure_provider` (SMOODEV-3918): Big Smooth adds or switches its own
//! model endpoint conversationally — "use my Anthropic key", "point yourself at
//! my LM Studio box".
//!
//! Shaped like `add_harness`: a pure request parser, a testable core
//! ([`configure`]) with the network probe injected, and a structured JSON
//! result the agent relays. The shared BYO rules (validation, the one-request
//! probe, the field-preserving write that points every routing slot at the new
//! provider) live in `smooth_cast::provider_setup`, so `th up`'s picker, the
//! web first-run card (`POST /api/llm/provider`) and this tool agree.
//!
//! The tool is on the daemon's confirmation floor (`CONFIRM_TOOLS`): it writes
//! a credential and changes which service every future turn is sent to, so the
//! user approves each call. Plan mode drops it like every other writer.
//!
//! **No hot reload.** The engine's `LocalServer` fixes its gateway URL, key and
//! model in an `Arc<ServerConfig>` at build time and its builder exposes no
//! per-turn key resolver, so a switch takes effect on the next start. The
//! result says so (`restart_required`) instead of pretending otherwise.

use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use smooth_cast::provider_setup::{self, ByoInput, ByoProvider};
use smooth_operator::{Tool, ToolSchema};

/// The tool's answer. Never carries the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigureReport {
    /// `configured` | `invalid` | `probe_failed` | `write_failed`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Big Smooth's chat engine (OpenAI-compatible only) can run turns on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_compatible: Option<bool>,
    /// The switch applies on the next daemon start.
    pub restart_required: bool,
    /// What to tell the user.
    pub message: String,
}

impl ConfigureReport {
    fn failed(status: &str, message: String) -> Self {
        Self {
            status: status.into(),
            provider_id: None,
            model: None,
            base_url: None,
            chat_compatible: None,
            restart_required: false,
            message,
        }
    }

    /// Pretty JSON for the agent.
    #[must_use]
    pub fn render(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| self.message.clone())
    }
}

/// Parse the tool arguments (or the web card's body) into a [`ByoInput`].
///
/// # Errors
/// When a required field is missing or not a string.
pub fn parse_request(args: &Value) -> Result<ByoInput, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(ByoInput {
        format: s("format").ok_or("`format` is required (openai | anthropic)")?,
        base_url: s("base_url").ok_or("`base_url` is required")?,
        api_key: s("api_key"),
        model: s("model").ok_or("`model` is required")?,
        id: s("id"),
    })
}

/// Validate → probe → write. `probe` is the network test (injected so tests
/// stay offline); the file at `path` is only touched after it succeeds.
pub async fn configure<P, Fut>(args: &Value, path: &Path, probe: P) -> ConfigureReport
where
    P: FnOnce(ByoProvider) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let input = match parse_request(args) {
        Ok(i) => i,
        Err(e) => return ConfigureReport::failed("invalid", e),
    };
    let provider = match provider_setup::validate_byo(&input) {
        Ok(p) => p,
        Err(e) => return ConfigureReport::failed("invalid", e),
    };
    if let Err(e) = probe(provider.clone()).await {
        return ConfigureReport::failed(
            "probe_failed",
            format!(
                "The test request to {} failed, so nothing was saved: {e:#}. Check the base URL, key and model name.",
                provider.base_url
            ),
        );
    }
    let write = || -> Result<()> {
        let mut root = smooth_cast::providers::load_value(path)?;
        provider_setup::apply_byo(&mut root, &provider);
        smooth_cast::providers::save_value(path, &root)
    };
    if let Err(e) = write() {
        return ConfigureReport::failed("write_failed", format!("The endpoint answered, but saving {} failed: {e:#}", path.display()));
    }
    let chat_compatible = provider_setup::daemon_chat_compatible(&provider);
    let mut message = format!(
        "Saved `{}` ({} at {}) and pointed every routing slot at it. Big Smooth reads its model at startup, so the switch takes effect after a restart: `th down && th up`, or quit and reopen the Big Smooth app.",
        provider.id, provider.model, provider.base_url
    );
    if !chat_compatible {
        message.push_str(" Note: Big Smooth's chat speaks OpenAI-compatible APIs only, so chat can't run on this Anthropic-format endpoint (`th code` can). Use an OpenAI-compatible URL for chat.");
    }
    ConfigureReport {
        status: "configured".into(),
        provider_id: Some(provider.id),
        model: Some(provider.model),
        base_url: Some(provider.base_url),
        chat_compatible: Some(chat_compatible),
        restart_required: true,
        message,
    }
}

/// `~/.smooth/providers.json`.
#[must_use]
pub fn default_providers_path() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".smooth").join("providers.json"))
}

/// The tool. `providers_path` overrides `~/.smooth/providers.json` (tests).
pub struct ConfigureProviderTool {
    pub providers_path: Option<PathBuf>,
}

#[async_trait]
impl Tool for ConfigureProviderTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "configure_provider".into(),
            description: "Add or switch Big Smooth's own LLM endpoint: an OpenAI-compatible or Anthropic API (base URL + API key + model). Sends one tiny test request first and saves nothing if it fails; on success writes ~/.smooth/providers.json and points every routing slot at it. The user approves each call. The switch applies after Big Smooth restarts — say so. Never echo the key back. Use when the user asks to use their own key/endpoint/model provider (\"use my OpenAI key\", \"switch to Anthropic\", \"use my local LM Studio\"). For the hosted Family plan, tell them to run `th model setup` or use the first-run card in the web UI instead."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "format": { "type": "string", "enum": ["openai", "anthropic"], "description": "Wire format: `openai` for any OpenAI-compatible /chat/completions API (OpenAI, Groq, OpenRouter, vLLM, LM Studio, Ollama), `anthropic` for Anthropic's Messages API." },
                    "base_url": { "type": "string", "description": "API base URL including the version path, e.g. https://api.openai.com/v1, https://api.anthropic.com/v1, http://localhost:11434/v1." },
                    "api_key": { "type": "string", "description": "The API key. May be omitted for a local server." },
                    "model": { "type": "string", "description": "Model id at that endpoint, e.g. gpt-4o-mini, claude-sonnet-4-5, llama3.3." },
                    "id": { "type": "string", "description": "Optional provider id in providers.json (derived from the host when omitted)." }
                },
                "required": ["format", "base_url", "model"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        // Writes ~/.smooth/providers.json.
        false
    }

    async fn execute(&self, arguments: Value) -> Result<String> {
        let path = self
            .providers_path
            .clone()
            .or_else(default_providers_path)
            .ok_or_else(|| anyhow::anyhow!("could not resolve the home directory"))?;
        let report = configure(&arguments, &path, |p| async move { provider_setup::probe(&p).await }).await;
        Ok(report.render())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn args(format: &str, url: &str, key: &str, model: &str) -> Value {
        json!({ "format": format, "base_url": url, "api_key": key, "model": model })
    }

    #[test]
    fn parse_request_names_the_missing_field() {
        assert!(parse_request(&json!({"base_url": "u", "model": "m"})).unwrap_err().contains("format"));
        assert!(parse_request(&json!({"format": "openai", "model": "m"})).unwrap_err().contains("base_url"));
        assert!(parse_request(&json!({"format": "openai", "base_url": "u"})).unwrap_err().contains("model"));
        let ok = parse_request(&json!({"format": "openai", "base_url": "u", "model": "m"})).unwrap();
        assert_eq!(ok.api_key, None);
    }

    #[tokio::test]
    async fn good_endpoint_is_probed_then_saved_without_echoing_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        let r = configure(
            &args("openai", "https://api.openai.com/v1", "sk-secret", "gpt-4o-mini"),
            &p,
            |prov| async move {
                assert_eq!(prov.api_key, "sk-secret", "the probe gets the real key");
                Ok("ok".to_string())
            },
        )
        .await;
        assert_eq!(r.status, "configured");
        assert_eq!(r.provider_id.as_deref(), Some("openai"));
        assert_eq!(r.chat_compatible, Some(true));
        assert!(r.restart_required);
        assert!(r.message.contains("th down && th up"));
        assert!(!r.render().contains("sk-secret"), "the key never goes back to the model");
        let (url, key, model) = smooth_cast::providers::route_at(&p, "coding").unwrap();
        assert_eq!(
            (url.as_str(), key.as_str(), model.as_str()),
            ("https://api.openai.com/v1", "sk-secret", "gpt-4o-mini")
        );
    }

    #[tokio::test]
    async fn invalid_input_never_probes_or_writes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        let r = configure(&args("openai", "http://api.example.com/v1", "k", "m"), &p, |_| async {
            panic!("must not probe")
        })
        .await;
        assert_eq!(r.status, "invalid");
        assert!(r.message.contains("https://"));
        assert!(!p.exists());
        let r = configure(&json!({"format": "cohere", "base_url": "https://x/v1", "model": "m"}), &p, |_| async {
            panic!("must not probe")
        })
        .await;
        assert_eq!(r.status, "invalid");
    }

    #[tokio::test]
    async fn failed_probe_saves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        std::fs::write(
            &p,
            r#"{"providers":[{"id":"keep","api_url":"https://k/v1","api_key":"k","api_format":"OpenAiCompat","default_model":"m"}]}"#,
        )
        .unwrap();
        let before = std::fs::read_to_string(&p).unwrap();
        let r = configure(&args("anthropic", "https://api.anthropic.com/v1", "bad", "claude-x"), &p, |_| async {
            Err(anyhow::anyhow!("401 invalid x-api-key"))
        })
        .await;
        assert_eq!(r.status, "probe_failed");
        assert!(r.message.contains("401") && r.message.contains("nothing was saved"));
        assert!(!r.restart_required);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    }

    #[tokio::test]
    async fn anthropic_proxy_is_saved_but_flagged_for_chat() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        let r = configure(&args("anthropic", "https://proxy.example.com/v1", "k", "claude-x"), &p, |_| async {
            Ok(String::new())
        })
        .await;
        assert_eq!(r.status, "configured");
        assert_eq!(r.chat_compatible, Some(false));
        assert!(r.message.contains("OpenAI-compatible APIs only"));
    }

    #[test]
    fn schema_requires_the_three_fields_and_is_serial() {
        let t = ConfigureProviderTool { providers_path: None };
        let s = t.schema();
        assert_eq!(s.name, "configure_provider");
        assert_eq!(s.parameters["required"], json!(["format", "base_url", "model"]));
        assert!(s.description.contains("approves"));
        assert!(!t.is_concurrent_safe());
    }

    #[tokio::test]
    async fn execute_reports_invalid_input_as_a_result_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let t = ConfigureProviderTool {
            providers_path: Some(dir.path().join("providers.json")),
        };
        let out = t.execute(json!({"format": "openai"})).await.unwrap();
        assert!(out.contains("\"status\": \"invalid\""), "{out}");
    }
}

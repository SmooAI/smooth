//! Bring-your-own-key provider setup, shared by every onboarding surface
//! (SMOODEV-3918): `th up`'s first-run picker, `th doctor --onboard`, the web
//! SPA's first-run card (`POST /api/llm/provider`) and the daemon's
//! `configure_provider` tool.
//!
//! One shape for all of them: an OpenAI-compatible or Anthropic-compatible
//! endpoint (base URL + key + model) → [`validate_byo`] (pure) → [`probe`]
//! (one cheap test request) → [`apply_byo`] (field-preserving write to
//! `providers.json` that points every routing slot at it).
//!
//! Big Smooth's chat engine (`smooth-operator-server`) speaks OpenAI-compatible
//! chat completions only. Anthropic's own API also serves that shape at the
//! same `/v1` base, so `api.anthropic.com` works for chat; any other
//! Anthropic-format endpoint is saved (and works for `th code`) but
//! [`daemon_chat_compatible`] reports that Big Smooth's chat can't use it.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The wire format a BYO endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ByoFormat {
    /// `POST {base}/chat/completions` (OpenAI, OpenRouter, Groq, Ollama, LM Studio, vLLM, …).
    OpenAi,
    /// `POST {base}/messages` (Anthropic and Anthropic-compatible proxies).
    Anthropic,
}

impl ByoFormat {
    /// Loose parse: `openai`, `openai-compatible`, `openai_compat`, `anthropic`, …
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        match s.as_str() {
            "openai" | "openai-compatible" | "openai-compat" | "openaicompat" | "oai" => Some(Self::OpenAi),
            "anthropic" | "anthropic-compatible" | "anthropic-compat" | "claude" => Some(Self::Anthropic),
            _ => None,
        }
    }

    /// The `--format` spelling `providers.json` / `normalize_format` accepts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    /// The engine's client format.
    #[must_use]
    pub const fn api_format(self) -> smooth_operator::llm::ApiFormat {
        match self {
            Self::OpenAi => smooth_operator::llm::ApiFormat::OpenAiCompat,
            Self::Anthropic => smooth_operator::llm::ApiFormat::Anthropic,
        }
    }
}

/// What a user (or the agent) typed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByoInput {
    /// `openai` | `anthropic` (loose; see [`ByoFormat::parse`]).
    pub format: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    pub model: String,
    /// Provider id in providers.json; derived from the host when absent.
    #[serde(default)]
    pub id: Option<String>,
}

/// A validated provider, ready to probe and save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByoProvider {
    pub id: String,
    pub format: ByoFormat,
    /// No trailing slash.
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

/// Local servers (Ollama, LM Studio, …) ignore the key, but the daemon only
/// treats a provider with a non-empty key as configured — so a keyless local
/// endpoint is saved with this placeholder.
pub const LOCAL_PLACEHOLDER_KEY: &str = "local";

fn host_of(url: &str) -> &str {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or_default()
}

fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// The providers.json id for an endpoint: the well-known vendors keep their
/// catalog ids (`openai`, `anthropic`, `openrouter`, `groq`, `ollama`), anything
/// else becomes `byo-<host>`.
#[must_use]
pub fn provider_id_for(format: ByoFormat, base_url: &str) -> String {
    let host = host_of(base_url).to_ascii_lowercase();
    let bare = host.split(':').next().unwrap_or_default();
    match bare {
        "api.openai.com" => "openai".into(),
        "api.anthropic.com" => "anthropic".into(),
        "openrouter.ai" => "openrouter".into(),
        "api.groq.com" => "groq".into(),
        _ if host.ends_with(":11434") => "ollama".into(),
        _ => {
            let s = slug(&host);
            if s.is_empty() {
                format!("byo-{}", format.as_str())
            } else {
                format!("byo-{}", s.chars().take(40).collect::<String>().trim_end_matches('-'))
            }
        }
    }
}

/// Validate and normalize a BYO request. Pure: no I/O.
///
/// # Errors
/// A user-facing message naming the bad field.
pub fn validate_byo(input: &ByoInput) -> Result<ByoProvider, String> {
    let format = ByoFormat::parse(&input.format).ok_or_else(|| format!("format must be `openai` or `anthropic` (got `{}`)", input.format.trim()))?;
    let raw_url = input.base_url.trim();
    if !(raw_url.starts_with("https://") || raw_url.starts_with("http://")) {
        return Err("base_url must start with https:// (or http:// for a local server)".into());
    }
    let base_url = raw_url.trim_end_matches('/').to_string();
    if host_of(raw_url).is_empty() || base_url.contains(char::is_whitespace) {
        return Err(format!("base_url `{raw_url}` has no host"));
    }
    let local = crate::providers::is_local_url(&base_url);
    let bare_host = host_of(&base_url).split(':').next().unwrap_or_default().to_ascii_lowercase();
    if base_url.starts_with("http://") && !local && !bare_host.ends_with(".local") && !is_private_host(&bare_host) {
        return Err("a remote endpoint must use https:// — the key would travel in clear text".into());
    }
    let key = input.api_key.as_deref().map(str::trim).unwrap_or_default();
    let api_key = if key.is_empty() {
        if !local {
            return Err("api_key is required for a remote endpoint".into());
        }
        LOCAL_PLACEHOLDER_KEY.to_string()
    } else {
        if key.contains(char::is_whitespace) {
            return Err("api_key must not contain whitespace".into());
        }
        key.to_string()
    };
    let model = input.model.trim().to_string();
    if model.is_empty() || model.contains(char::is_whitespace) {
        return Err("model is required (e.g. gpt-4o-mini, claude-sonnet-4-5, llama3.3)".into());
    }
    let id = match input.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => {
            if id.len() > 48 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err("id must be 1-48 chars of letters, digits, `-` or `_`".into());
            }
            id.to_string()
        }
        None => provider_id_for(format, &base_url),
    };
    Ok(ByoProvider {
        id,
        format,
        base_url,
        api_key,
        model,
    })
}

/// RFC 1918 / tailnet (`100.64/10`) literal IPv4 hosts — a LAN inference box
/// over plain http is fine; a public host is not.
fn is_private_host(host: &str) -> bool {
    let bare = host.split(':').next().unwrap_or_default();
    let Ok(ip) = bare.parse::<std::net::Ipv4Addr>() else { return false };
    let [a, b, ..] = ip.octets();
    ip.is_private() || ip.is_loopback() || (a == 100 && (64..128).contains(&b))
}

/// Whether Big Smooth's chat engine (OpenAI-compatible only) can run turns on
/// this provider. Anthropic's own API serves the OpenAI shape at the same base.
#[must_use]
pub fn daemon_chat_compatible(p: &ByoProvider) -> bool {
    match p.format {
        ByoFormat::OpenAi => true,
        ByoFormat::Anthropic => host_of(&p.base_url).eq_ignore_ascii_case("api.anthropic.com"),
    }
}

/// Write `p` into a parsed providers.json and point every routing slot at it —
/// an explicit setup is a switch, not an "also". Field-preserving: sibling
/// providers and unknown keys survive. Returns `true` when an entry with that
/// id was updated rather than added.
pub fn apply_byo(root: &mut Value, p: &ByoProvider) -> bool {
    let updated = crate::providers::upsert_provider(
        root,
        &crate::providers::NewProvider {
            id: p.id.clone(),
            api_url: p.base_url.clone(),
            api_key: Some(p.api_key.clone()),
            api_format: Some(p.format.as_str().to_string()),
            default_model: Some(p.model.clone()),
            max_tokens: None,
        },
    );
    crate::providers::wire_routing_to(root, &p.id, &p.model);
    updated
}

/// The engine config the probe uses (tiny token budget, no retries).
#[must_use]
pub fn probe_config(p: &ByoProvider) -> smooth_operator::llm::LlmConfig {
    smooth_operator::llm::LlmConfig {
        api_url: p.base_url.clone(),
        api_key: p.api_key.clone(),
        model: p.model.clone(),
        max_tokens: 16,
        temperature: smooth_policy::llm_params::AGENT_TEMPERATURE,
        retry_policy: smooth_operator::llm::RetryPolicy {
            max_retries: 0,
            ..smooth_operator::llm::RetryPolicy::default()
        },
        api_format: p.format.api_format(),
    }
}

/// One cheap test request ("say ok", 16 tokens, 30 s cap). Returns the first
/// few characters of the reply.
///
/// # Errors
/// The endpoint's error (bad key, unknown model, unreachable host, timeout).
pub async fn probe(p: &ByoProvider) -> anyhow::Result<String> {
    let client = smooth_operator::llm::LlmClient::new(probe_config(p));
    let msg = smooth_operator::conversation::Message::user("Reply with the single word ok.");
    let resp = tokio::time::timeout(Duration::from_secs(30), client.chat(&[&msg], &[]))
        .await
        .map_err(|_| anyhow::anyhow!("no answer from {} within 30s", p.base_url))??;
    Ok(resp.content.trim().chars().take(20).collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(format: &str, url: &str, key: Option<&str>, model: &str) -> ByoInput {
        ByoInput {
            format: format.into(),
            base_url: url.into(),
            api_key: key.map(str::to_string),
            model: model.into(),
            id: None,
        }
    }

    #[test]
    fn format_parses_loosely() {
        assert_eq!(ByoFormat::parse("OpenAI-Compatible"), Some(ByoFormat::OpenAi));
        assert_eq!(ByoFormat::parse("openai_compat"), Some(ByoFormat::OpenAi));
        assert_eq!(ByoFormat::parse(" anthropic "), Some(ByoFormat::Anthropic));
        assert_eq!(ByoFormat::parse("gemini"), None);
        assert_eq!(ByoFormat::OpenAi.as_str(), "openai");
        assert_eq!(ByoFormat::Anthropic.as_str(), "anthropic");
        assert!(matches!(ByoFormat::Anthropic.api_format(), smooth_operator::llm::ApiFormat::Anthropic));
    }

    #[test]
    fn validate_normalizes_a_good_openai_endpoint() {
        let p = validate_byo(&input("openai", " https://api.openai.com/v1/ ", Some(" sk-1 "), " gpt-4o-mini ")).unwrap();
        assert_eq!(p.id, "openai");
        assert_eq!(p.base_url, "https://api.openai.com/v1");
        assert_eq!(p.api_key, "sk-1");
        assert_eq!(p.model, "gpt-4o-mini");
        assert!(daemon_chat_compatible(&p));
    }

    #[test]
    fn validate_rejects_each_bad_field() {
        assert!(validate_byo(&input("gemini", "https://x.ai/v1", Some("k"), "m"))
            .unwrap_err()
            .contains("format"));
        assert!(validate_byo(&input("openai", "x.ai/v1", Some("k"), "m")).unwrap_err().contains("https://"));
        assert!(validate_byo(&input("openai", "https://", Some("k"), "m")).unwrap_err().contains("no host"));
        assert!(validate_byo(&input("openai", "http://api.example.com/v1", Some("k"), "m"))
            .unwrap_err()
            .contains("clear text"));
        assert!(validate_byo(&input("openai", "https://api.example.com/v1", None, "m"))
            .unwrap_err()
            .contains("api_key"));
        assert!(validate_byo(&input("openai", "https://api.example.com/v1", Some("a b"), "m"))
            .unwrap_err()
            .contains("whitespace"));
        assert!(validate_byo(&input("openai", "https://api.example.com/v1", Some("k"), ""))
            .unwrap_err()
            .contains("model"));
        assert!(validate_byo(&input("openai", "https://api.example.com/v1", Some("k"), "two words"))
            .unwrap_err()
            .contains("model"));
        let mut bad_id = input("openai", "https://api.example.com/v1", Some("k"), "m");
        bad_id.id = Some("no/slashes".into());
        assert!(validate_byo(&bad_id).unwrap_err().contains("id must"));
    }

    #[test]
    fn local_and_lan_servers_need_no_key_and_may_use_http() {
        let p = validate_byo(&input("openai", "http://localhost:11434/v1", None, "llama3.3")).unwrap();
        assert_eq!(p.id, "ollama");
        assert_eq!(p.api_key, LOCAL_PLACEHOLDER_KEY, "the daemon needs a non-empty key");
        assert!(validate_byo(&input("openai", "http://192.168.1.20:1234/v1", Some("x"), "qwen3")).is_ok());
        assert!(
            validate_byo(&input("openai", "http://100.100.1.2:8000/v1", Some("x"), "qwen3")).is_ok(),
            "tailnet"
        );
        assert!(validate_byo(&input("openai", "http://gpu.local:8000/v1", Some("x"), "qwen3")).is_ok());
    }

    #[test]
    fn ids_follow_the_vendor_or_the_host() {
        assert_eq!(provider_id_for(ByoFormat::Anthropic, "https://api.anthropic.com/v1"), "anthropic");
        assert_eq!(provider_id_for(ByoFormat::OpenAi, "https://openrouter.ai/api/v1"), "openrouter");
        assert_eq!(provider_id_for(ByoFormat::OpenAi, "https://api.groq.com/openai/v1"), "groq");
        assert_eq!(
            provider_id_for(ByoFormat::OpenAi, "https://llm.corp.example.com:8443/v1"),
            "byo-llm-corp-example-com-8443"
        );
        let mut explicit = input("openai", "https://x.example/v1", Some("k"), "m");
        explicit.id = Some("work".into());
        assert_eq!(validate_byo(&explicit).unwrap().id, "work");
    }

    #[test]
    fn anthropic_chat_compat_only_for_anthropic_itself() {
        let real = validate_byo(&input("anthropic", "https://api.anthropic.com/v1", Some("k"), "claude-sonnet-4-5")).unwrap();
        assert!(daemon_chat_compatible(&real));
        let proxy = validate_byo(&input("anthropic", "https://proxy.example.com/v1", Some("k"), "claude-sonnet-4-5")).unwrap();
        assert!(!daemon_chat_compatible(&proxy));
    }

    #[test]
    fn apply_byo_switches_every_slot_and_keeps_siblings() {
        let mut root = json!({
            "providers": [{ "id": "smooai-gateway", "api_url": "https://llm.smoo.ai/v1", "api_key": "g", "api_format": "OpenAiCompat", "default_model": "d", "max_tokens": 9 }],
            "routing": { "coding": { "provider": "smooai-gateway", "model": "d", "fallback": "x" } },
            "keep": true
        });
        let p = validate_byo(&input("anthropic", "https://api.anthropic.com/v1", Some("sk-ant"), "claude-sonnet-4-5")).unwrap();
        assert!(!apply_byo(&mut root, &p));
        assert_eq!(root["keep"], true);
        assert_eq!(root["providers"][0]["max_tokens"], 9, "sibling fields survive");
        assert_eq!(root["providers"][1]["api_format"], "Anthropic");
        assert_eq!(root["routing"]["coding"]["provider"], "anthropic");
        assert_eq!(root["routing"]["coding"]["fallback"], "x", "slot extras survive");
        assert_eq!(root["routing"]["fast"]["model"], "claude-sonnet-4-5");
        let (url, key, model) = crate::providers::route_from_value(&root, "coding").unwrap();
        assert_eq!(
            (url.as_str(), key.as_str(), model.as_str()),
            ("https://api.anthropic.com/v1", "sk-ant", "claude-sonnet-4-5")
        );
        // Re-applying updates in place.
        assert!(apply_byo(&mut root, &p));
        assert_eq!(root["providers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn probe_config_is_cheap_and_never_retries() {
        let p = validate_byo(&input("openai", "https://api.openai.com/v1", Some("k"), "gpt-4o-mini")).unwrap();
        let c = probe_config(&p);
        assert_eq!(c.max_tokens, 16);
        assert_eq!(c.retry_policy.max_retries, 0);
        assert_eq!(c.api_url, "https://api.openai.com/v1");
    }

    #[tokio::test]
    async fn probe_reports_an_unreachable_endpoint() {
        // Port 9 (discard) on loopback: refused immediately, no network needed.
        let p = validate_byo(&input("openai", "http://127.0.0.1:9/v1", Some("k"), "m")).unwrap();
        assert!(probe(&p).await.is_err());
    }
}

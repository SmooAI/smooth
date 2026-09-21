//! The LLM provider gate (pearl th-473294): does this daemon have model
//! credentials at all, and if not, what are the two ways to get some?
//!
//! `GET /api/llm/provider` answers for clients (`th harness add --agentic`
//! asks before it drives a turn, and prompts for a provider when the answer
//! is "none"); [`status`] is the same answer for the `add_harness` tool, which
//! returns a structured `needs_provider` result instead of failing mid-run.
//!
//! Resolution mirrors `operator::resolve_gateway_config`: the operator's env
//! gateway (`SMOOAI_GATEWAY_KEY`) first, else the `coding` slot of
//! `~/.smooth/providers.json`. Model name + gateway host only — never a key.
//!
//! The daemon reads its gateway ONCE at boot (the operator's `ServerConfig`
//! is fixed for the server's lifetime), so a provider added while it runs is
//! reported as `configured` **with `restart_required: true`** — the honest
//! answer, and the reason the CLI tells the user to bounce Big Smooth.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

/// One way to get a provider, offered when there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderOption {
    /// `smoo-gateway` | `byo-key`.
    pub id: String,
    pub title: String,
    pub description: String,
    /// What to run.
    pub command: String,
}

/// The gate's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub configured: bool,
    /// `env` | `providers.json` when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Host of the gateway URL (`llm.smoo.ai`), never the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_host: Option<String>,
    /// Credentials exist on disk but the running daemon booted without any —
    /// restart it before a turn can use them.
    #[serde(default)]
    pub restart_required: bool,
    /// How to get one, when `configured` is false.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ProviderOption>,
}

/// The two offers, Smoo's gateway first.
#[must_use]
pub fn options() -> Vec<ProviderOption> {
    vec![
        ProviderOption {
            id: "smoo-gateway".into(),
            title: "Smoo AI Gateway (recommended)".into(),
            description: "Sign in with Smoo, mint your org's llm.smoo.ai key, save it to ~/.smooth/providers.json — one key, 100+ models, billed to your org.".into(),
            command: "th harness add --agentic <name>   # choose “Smoo AI Gateway” at the prompt (or: smoo auth login && smoo llm create-key && th model login smooai-gateway)".into(),
        },
        ProviderOption {
            id: "byo-key".into(),
            title: "Bring your own key".into(),
            description: "Any provider th knows (openai, anthropic, openrouter, ollama, …) saved to ~/.smooth/providers.json.".into(),
            command: "th model login <provider> --api-key <key>".into(),
        },
    ]
}

static BOOT_CONFIGURED: OnceLock<bool> = OnceLock::new();

/// Record whether the daemon booted with a gateway key (called once from
/// `serve_local_flavor`). Later calls are ignored.
pub fn mark_boot(configured: bool) {
    let _ = BOOT_CONFIGURED.set(configured);
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split('/').next().map(str::to_string).filter(|h| !h.is_empty())
}

/// Pure core of [`status`].
///
/// `env_key` is `SMOOAI_GATEWAY_KEY`, `env_url` its URL, `providers` the
/// providers.json path, `boot_configured` what the daemon booted with
/// (`None` outside a running daemon ⇒ no restart advice).
#[must_use]
pub fn status_from(
    env_url: Option<&str>,
    env_key: Option<&str>,
    env_model: Option<&str>,
    providers: Option<&Path>,
    boot_configured: Option<bool>,
) -> ProviderStatus {
    let (configured, source, model, host) = if env_key.is_some_and(|k| !k.trim().is_empty()) {
        (true, Some("env".to_string()), env_model.map(str::to_string), env_url.and_then(host_of))
    } else if let Some((url, _key, model)) = providers.and_then(|p| crate::operator::gateway_from_providers_at(p, "coding")) {
        (true, Some("providers.json".to_string()), Some(model), host_of(&url))
    } else {
        (false, None, None, None)
    };
    ProviderStatus {
        configured,
        source,
        model,
        gateway_host: host,
        restart_required: configured && boot_configured == Some(false),
        options: if configured { Vec::new() } else { options() },
    }
}

fn providers_path() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".smooth").join("providers.json"))
}

/// The live answer: env + `~/.smooth/providers.json` + the boot marker.
#[must_use]
pub fn status() -> ProviderStatus {
    status_from(
        std::env::var("SMOOAI_GATEWAY_URL").ok().as_deref(),
        std::env::var("SMOOAI_GATEWAY_KEY").ok().as_deref(),
        std::env::var("SMOOTH_AGENT_MODEL").ok().as_deref(),
        providers_path().as_deref(),
        BOOT_CONFIGURED.get().copied(),
    )
}

/// `GET /api/llm/provider`. Ungated like `/api/mode`: a model name and a
/// gateway host must render on a tokenless connection, and neither is a
/// secret.
pub fn provider_router() -> Router {
    Router::new().route("/api/llm/provider", get(provider_handler))
}

async fn provider_handler() -> Json<ProviderStatus> {
    let s = tokio::task::spawn_blocking(status)
        .await
        .unwrap_or_else(|_| status_from(None, None, None, None, None));
    Json(s)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn env_key_wins_and_reports_the_host_not_the_key() {
        let s = status_from(Some("https://llm.smoo.ai/v1"), Some("sk-secret"), Some("m1"), None, Some(true));
        assert!(s.configured);
        assert_eq!(s.source.as_deref(), Some("env"));
        assert_eq!(s.gateway_host.as_deref(), Some("llm.smoo.ai"));
        assert_eq!(s.model.as_deref(), Some("m1"));
        assert!(!s.restart_required);
        assert!(s.options.is_empty());
        let j = serde_json::to_string(&s).unwrap();
        assert!(!j.contains("sk-secret"), "{j}");
    }

    #[test]
    fn blank_env_key_falls_through_to_providers_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        std::fs::write(
            &p,
            r#"{"providers":[{"id":"smooai-gateway","api_url":"https://llm.smoo.ai/v1","api_key":"k","api_format":"openai","default_model":"d"}],"routing":{"coding":{"provider":"smooai-gateway","model":"deepseek-v4-flash"}}}"#,
        )
        .unwrap();
        let s = status_from(None, Some("   "), None, Some(&p), Some(false));
        assert!(s.configured);
        assert_eq!(s.source.as_deref(), Some("providers.json"));
        assert_eq!(s.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(s.gateway_host.as_deref(), Some("llm.smoo.ai"));
        assert!(s.restart_required, "booted without a key, now has one ⇒ restart");
        assert!(!serde_json::to_string(&s).unwrap().contains("\"k\""));
    }

    #[test]
    fn nothing_configured_offers_smoo_first_then_byo() {
        let s = status_from(None, None, None, Some(Path::new("/nonexistent/providers.json")), None);
        assert!(!s.configured);
        assert!(!s.restart_required);
        assert_eq!(s.options.len(), 2);
        assert_eq!(s.options[0].id, "smoo-gateway");
        assert_eq!(s.options[1].id, "byo-key");
        assert!(s.options[0].command.contains("--agentic"));
        assert!(s.options[1].command.contains("th model login"));
    }

    #[test]
    fn restart_advice_only_when_the_boot_marker_says_unconfigured() {
        let s = status_from(Some("https://x/v1"), Some("k"), None, None, None);
        assert!(!s.restart_required, "no marker (not inside a daemon) ⇒ no advice");
        let s = status_from(Some("https://x/v1"), Some("k"), None, None, Some(true));
        assert!(!s.restart_required);
    }

    #[test]
    fn host_of_handles_bare_and_pathed_urls() {
        assert_eq!(host_of("https://llm.smoo.ai/v1").as_deref(), Some("llm.smoo.ai"));
        assert_eq!(host_of("localhost:11434").as_deref(), Some("localhost:11434"));
        assert_eq!(host_of("").as_deref(), None);
    }

    #[tokio::test]
    async fn handler_returns_the_envelope_without_secrets() {
        let resp = provider_handler().await;
        let j = serde_json::to_value(&resp.0).unwrap();
        assert!(j.get("configured").is_some());
        for forbidden in ["api_key", "key\":", "sk-"] {
            assert!(!j.to_string().contains(forbidden), "{j}");
        }
    }
}

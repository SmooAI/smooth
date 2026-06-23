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
/// loopback by design — remote access goes over Tailscale. A non-loopback bind
/// without [`resolve_auth_token`] set logs a startup warning (see `server.rs`).
///
/// # Errors
/// Returns an error if `SMOOTH_DAEMON_BIND` is set but unparseable.
pub fn resolve_bind() -> anyhow::Result<SocketAddr> {
    let raw = std::env::var("SMOOTH_DAEMON_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    raw.parse().with_context(|| format!("invalid SMOOTH_DAEMON_BIND: {raw:?}"))
}

/// Resolve the daemon's bearer token from `SMOOTH_DAEMON_TOKEN`.
///
/// Auth is **opt-in**: with no token set the daemon serves open (the loopback
/// default trusts the local user). Set a token before binding to a tailnet so
/// programmatic clients must present `Authorization: Bearer <token>`. An
/// all-whitespace value is treated as unset.
#[must_use]
pub fn resolve_auth_token() -> Option<String> {
    std::env::var("SMOOTH_DAEMON_TOKEN").ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// Default loopback address the egress proxy binds to when the boundary is on.
pub const DEFAULT_EGRESS_PROXY_ADDR: &str = "127.0.0.1:4419";

/// A curated default egress allowlist.
///
/// The hosts an agent's shell legitimately reaches for routine dev work
/// (package registries, source hosts, the Smoo platform). Opt in by putting the
/// `defaults` token in `SMOOTH_EGRESS_ALLOWLIST` (alone, or alongside your own
/// exact hosts). Exact hosts only, by design.
pub const DEFAULT_EGRESS_HOSTS: &[&str] = &[
    // package registries
    "registry.npmjs.org",
    "registry.yarnpkg.com",
    "crates.io",
    "static.crates.io",
    "index.crates.io",
    "pypi.org",
    "files.pythonhosted.org",
    // source hosts
    "github.com",
    "api.github.com",
    "raw.githubusercontent.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
    // Smoo platform
    "api.smoo.ai",
    "llm.smoo.ai",
    "auth.smoo.ai",
];

/// The egress boundary's resolved configuration.
pub struct EgressSetup {
    /// The exact-host allowlist the proxy enforces.
    pub allowlist: smooth_goalie::EgressAllowlist,
    /// Entries that failed to parse (wildcards, ports, …) — logged on startup.
    pub rejected: Vec<String>,
    /// `host:port` the proxy binds to and the bash tool is pointed at.
    pub proxy_addr: String,
}

/// Resolve the egress boundary from the environment.
///
/// **Opt-in**: returns `Some` only when `SMOOTH_EGRESS_ALLOWLIST` is set (a
/// comma/whitespace-separated list of exact hosts). With it unset, the bash
/// tool's network is unrestricted (matching the auth/sandbox opt-in posture).
/// The `defaults` token expands to [`DEFAULT_EGRESS_HOSTS`] (mergeable with your
/// own hosts). `SMOOTH_EGRESS_PROXY_ADDR` overrides the proxy bind address.
#[must_use]
pub fn resolve_egress() -> Option<EgressSetup> {
    resolve_egress_inner(std::env::var("SMOOTH_EGRESS_ALLOWLIST").ok(), std::env::var("SMOOTH_EGRESS_PROXY_ADDR").ok())
}

/// Pure core (no env reads) so the parse/expand logic is unit-testable without
/// racing on process env. `allowlist_env` is the raw `SMOOTH_EGRESS_ALLOWLIST`.
fn resolve_egress_inner(allowlist_env: Option<String>, proxy_addr_env: Option<String>) -> Option<EgressSetup> {
    let raw = allowlist_env?;
    let mut entries: Vec<String> = Vec::new();
    for tok in raw.split([',', ' ', '\t', '\n']).map(str::trim).filter(|s| !s.is_empty()) {
        if tok.eq_ignore_ascii_case("default") || tok.eq_ignore_ascii_case("defaults") {
            entries.extend(DEFAULT_EGRESS_HOSTS.iter().map(|h| (*h).to_owned()));
        } else {
            entries.push(tok.to_owned());
        }
    }
    let (allowlist, rejected) = smooth_goalie::EgressAllowlist::from_entries(entries);
    let proxy_addr = proxy_addr_env.unwrap_or_else(|| DEFAULT_EGRESS_PROXY_ADDR.to_owned());
    Some(EgressSetup {
        allowlist,
        rejected,
        proxy_addr,
    })
}

/// Where the egress proxy writes its JSON-lines audit (`~/.smooth/audit/
/// egress-proxy.jsonl`, or `./egress-proxy.jsonl` if HOME is unavailable).
#[must_use]
pub fn egress_audit_path() -> PathBuf {
    dirs_next::home_dir().map_or_else(
        || PathBuf::from("egress-proxy.jsonl"),
        |h| h.join(".smooth").join("audit").join("egress-proxy.jsonl"),
    )
}

/// Resolve the Gate-1 permission mode from `SMOOTH_PERMISSION_MODE` (default
/// [`PermissionMode::Default`](crate::permission::PermissionMode::Default) —
/// reads auto, mutations prompt).
#[must_use]
pub fn resolve_permission_mode() -> crate::permission::PermissionMode {
    std::env::var("SMOOTH_PERMISSION_MODE")
        .ok()
        .and_then(|s| crate::permission::PermissionMode::parse(&s))
        .unwrap_or_default()
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
    fn resolve_egress_is_opt_in_and_parses_hosts() {
        // Pure core → no env mutation, so no races with the rest of the suite.
        assert!(resolve_egress_inner(None, None).is_none(), "unset → egress boundary off (opt-in)");

        let setup = resolve_egress_inner(Some("github.com, api.smoo.ai *.bad.com".to_owned()), None).expect("set → Some");
        assert!(setup.allowlist.is_allowed("github.com"));
        assert!(setup.allowlist.is_allowed("api.smoo.ai"));
        assert!(!setup.allowlist.is_allowed("evil.com"));
        assert_eq!(setup.rejected, vec!["*.bad.com".to_owned()], "wildcard entry rejected + surfaced");
        assert_eq!(setup.proxy_addr, DEFAULT_EGRESS_PROXY_ADDR);
    }

    #[test]
    fn resolve_egress_defaults_token_expands_and_merges() {
        let setup = resolve_egress_inner(Some("defaults, mycorp.internal".to_owned()), None).expect("set → Some");
        // The curated defaults are present…
        assert!(setup.allowlist.is_allowed("github.com"));
        assert!(setup.allowlist.is_allowed("registry.npmjs.org"));
        assert!(setup.allowlist.is_allowed("llm.smoo.ai"));
        // …merged with the user's own host…
        assert!(setup.allowlist.is_allowed("mycorp.internal"));
        // …and the `defaults` sentinel is NOT treated as a (rejected) host.
        assert!(setup.rejected.is_empty(), "sentinel must not surface as rejected: {:?}", setup.rejected);
        assert!(setup.allowlist.len() > DEFAULT_EGRESS_HOSTS.len());
    }

    #[test]
    fn resolve_egress_honors_proxy_addr_override() {
        let setup = resolve_egress_inner(Some("github.com".to_owned()), Some("127.0.0.1:9999".to_owned())).expect("Some");
        assert_eq!(setup.proxy_addr, "127.0.0.1:9999");
    }

    #[test]
    fn auth_token_blank_is_unset() {
        // Direct env tests would race with other tests; assert the trim/empty
        // policy on the value-shaping path instead.
        assert_eq!(Some("   ".to_owned()).map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()), None);
        assert_eq!(
            Some("  secret  ".to_owned()).map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()),
            Some("secret".to_owned())
        );
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

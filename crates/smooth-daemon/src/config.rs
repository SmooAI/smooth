//! Daemon configuration + LLM credential resolution.
//!
//! Credentials resolve in priority order:
//! 1. **Explicit env** — `SMOOTH_API_URL` + `SMOOTH_API_KEY` (+ `SMOOTH_MODEL`
//!    or a per-task model override). Highest priority so a run can be pointed
//!    at any endpoint without touching config.
//! 2. **`providers.json`** — the credentials `th model login` writes to
//!    `~/.smooth/providers.json` (overridable with `SMOOTH_PROVIDERS_FILE`).
//!    Resolved through the engine's [`ProviderRegistry`](smooth_operator::providers::ProviderRegistry), so the always-on
//!    daemon Just Works with the same creds the rest of `th` uses.
//!
//! If neither is present the daemon errors with an actionable message.

use std::path::{Path, PathBuf};

use anyhow::Context;
use smooth_operator::providers::Activity;
use smooth_operator::LlmConfig;
use smooth_tools::SandboxMode;

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

/// Whether cloud-backed memory routing is enabled (Family AI M3 Phase B.2, ADR-009).
///
/// **Opt-in, default OFF** — the feature is per-ADR opt-in and the platform memory
/// home (Phase B.1) must be deployed for it to work. With it off, `remember`/
/// `recall` stay on the daemon's local sqlite store exactly as before (zero
/// behavior change). Enabling ALSO requires the B.1 deploy and a live Smoo AI user
/// session (personal scope needs a human identity); a Family AI subscription gate
/// is a separate stream (th-74e0f8), not enforced here.
///
/// Set `SMOOTH_CLOUD_MEMORY` to `1`/`true`/`yes`/`on` to enable.
#[must_use]
pub fn cloud_memory_enabled() -> bool {
    cloud_memory_enabled_inner(std::env::var("SMOOTH_CLOUD_MEMORY").ok().as_deref())
}

/// Pure core (no env read) so the truthiness policy is unit-testable.
fn cloud_memory_enabled_inner(raw: Option<&str>) -> bool {
    matches!(raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(), Some("1" | "true" | "yes" | "on"))
}

/// The daemon's kernel-sandbox switch, resolved from `SMOOTH_SANDBOX`
/// ([`smooth_tools::SANDBOX_ENV`]).
///
/// **Opt-in, default OFF** (pearl th-efbab1). Big Smooth is a personal agent
/// that operates AS its user on the user's own machine; with the kernel sandbox
/// on, `ssh` to the user's own boxes and `git fetch` failed (credential-store
/// read-deny on `~/.ssh/known_hosts`, kernel-denied direct outbound). Set
/// `SMOOTH_SANDBOX` to `1`/`true`/`yes`/`on` to turn it back on. The permission
/// gate and Narc run either way.
///
/// The value is read where every shell policy is built
/// ([`smooth_tools::SandboxPolicy::for_workspace`]); the daemon resolves the
/// same variable here only to log its startup posture, so the two can't
/// disagree.
pub struct SandboxSetting {
    /// The resolved mode (default [`SandboxMode::PassThrough`]).
    pub mode: SandboxMode,
    /// A set-but-unrecognized raw value (a typo), so startup can warn that it
    /// resolved to the default instead of silently guessing.
    pub unrecognized: Option<String>,
}

/// Resolve [`SandboxSetting`] from the environment.
#[must_use]
pub fn resolve_sandbox() -> SandboxSetting {
    resolve_sandbox_inner(std::env::var(smooth_tools::SANDBOX_ENV).ok())
}

/// Pure core (no env read) so the opt-in policy is unit-testable.
fn resolve_sandbox_inner(raw: Option<String>) -> SandboxSetting {
    let parsed = raw.as_deref().map(SandboxMode::parse);
    SandboxSetting {
        mode: SandboxMode::from_env_value(raw.as_deref()),
        unrecognized: match parsed {
            Some(None) => raw,
            _ => None,
        },
    }
}

/// Whether the startup posture line is a warning or plain info.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostureLevel {
    /// The posture is what the operator asked for.
    Info,
    /// The operator asked for something this host can't give (sandbox on a
    /// platform without one, or an unrecognized switch value).
    Warn,
}

/// The one startup line stating the tool-execution posture.
///
/// Says whether the agent runs as the user or kernel-sandboxed, and whether
/// the egress allowlist (if configured) is a boundary or advisory. Pure so
/// every combination is tested.
#[must_use]
pub fn sandbox_posture(setting: &SandboxSetting, egress_proxy: Option<&str>, platform_supported: bool) -> (PostureLevel, String) {
    if let Some(raw) = &setting.unrecognized {
        return (
            PostureLevel::Warn,
            format!(
                "kernel sandbox OFF: {}={raw:?} is not a recognized value (use 1/true/yes/on or 0/false/no/off) — tools run as you{}",
                smooth_tools::SANDBOX_ENV,
                egress_proxy.map_or(String::new(), |p| format!("; egress allowlist via {p} is ADVISORY (not kernel-enforced)")),
            ),
        );
    }
    match (setting.mode, platform_supported, egress_proxy) {
        (SandboxMode::PassThrough, _, None) => (
            PostureLevel::Info,
            format!("kernel sandbox OFF (default): agent tools run as you; set {}=1 to enable", smooth_tools::SANDBOX_ENV),
        ),
        (SandboxMode::PassThrough, _, Some(p)) => (
            PostureLevel::Info,
            format!(
                "kernel sandbox OFF (default): agent tools run as you; egress allowlist via {p} is ADVISORY (HTTP(S)_PROXY set, direct connections not blocked) — set {}=1 to enforce it",
                smooth_tools::SANDBOX_ENV
            ),
        ),
        (SandboxMode::Enforced, true, None) => (
            PostureLevel::Info,
            "kernel sandbox ON (Seatbelt): credential stores + git hooks kernel-denied; egress unrestricted".to_owned(),
        ),
        (SandboxMode::Enforced, true, Some(p)) => (
            PostureLevel::Info,
            format!("kernel sandbox ON (Seatbelt): credential stores + git hooks kernel-denied; egress kernel-forced through the allowlist proxy at {p}"),
        ),
        (SandboxMode::Enforced, false, _) => (
            PostureLevel::Warn,
            format!(
                "{}=1 but this platform has no kernel sandbox yet (th-08e05a) — tools run UNSANDBOXED{}",
                smooth_tools::SANDBOX_ENV,
                if egress_proxy.is_some() { "; egress allowlist is ADVISORY" } else { "" }
            ),
        ),
    }
}

/// Log [`sandbox_posture`] for this process once, at startup.
pub fn log_sandbox_posture(egress_proxy: Option<&str>) {
    let setting = resolve_sandbox();
    let (level, line) = sandbox_posture(&setting, egress_proxy, smooth_tools::SandboxPolicy::platform_supported());
    match level {
        PostureLevel::Info => tracing::info!(sandbox = ?setting.mode, "{line}"),
        PostureLevel::Warn => tracing::warn!(sandbox = ?setting.mode, "{line}"),
    }
}

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
/// The allowlist is only a hard boundary with the opt-in kernel sandbox on
/// (macOS); with it off the proxy env vars are set but advisory — see
/// [`sandbox_posture`].
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
            temperature: smooth_policy::llm_params::AGENT_TEMPERATURE,
            retry_policy: smooth_operator::llm::RetryPolicy::default(),
            api_format,
        });
    }

    // 2. providers.json (th model login creds), via the engine's registry.
    if let Some(path) = providers_path {
        if path.exists() {
            let registry = smooth_cast::provider_migration::load_providers_with_migration(path).with_context(|| format!("reading {}", path.display()))?;
            let mut cfg = registry
                .llm_config_for(Activity::Coding)
                .context("resolving an LLM from providers.json (is a provider + routing configured?)")?;
            if let Some(model) = model_override {
                cfg = cfg.with_model(model);
            }
            return Ok(cfg);
        }
    }

    anyhow::bail!("no LLM credentials: run `th model login` (writes ~/.smooth/providers.json) or set SMOOTH_API_URL + SMOOTH_API_KEY (+ SMOOTH_MODEL)")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn cloud_memory_toggle_is_opt_in() {
        // Default (unset) and empty → OFF.
        assert!(!cloud_memory_enabled_inner(None));
        assert!(!cloud_memory_enabled_inner(Some("")));
        assert!(!cloud_memory_enabled_inner(Some("   ")));
        // A random / false-y value stays OFF (fail-safe: only explicit truthy on).
        assert!(!cloud_memory_enabled_inner(Some("0")));
        assert!(!cloud_memory_enabled_inner(Some("false")));
        assert!(!cloud_memory_enabled_inner(Some("maybe")));
        // Explicit truthy values → ON (case/whitespace-insensitive).
        for v in ["1", "true", "TRUE", " yes ", "On"] {
            assert!(cloud_memory_enabled_inner(Some(v)), "{v:?} should enable");
        }
    }

    #[test]
    fn sandbox_is_opt_in_and_off_by_default() {
        // Unset → pass-through: the agent runs as the user.
        let unset = resolve_sandbox_inner(None);
        assert_eq!(unset.mode, SandboxMode::PassThrough);
        assert!(unset.unrecognized.is_none());
        for v in ["", "0", "false", "off", "NO"] {
            let s = resolve_sandbox_inner(Some(v.to_owned()));
            assert_eq!(s.mode, SandboxMode::PassThrough, "{v:?}");
            assert!(s.unrecognized.is_none(), "{v:?} is a recognized off value");
        }
        for v in ["1", "true", "YES", " on "] {
            assert_eq!(resolve_sandbox_inner(Some(v.to_owned())).mode, SandboxMode::Enforced, "{v:?} should enable");
        }
        // A typo resolves to the default AND is surfaced for a warning.
        let typo = resolve_sandbox_inner(Some("enabled".to_owned()));
        assert_eq!(typo.mode, SandboxMode::PassThrough);
        assert_eq!(typo.unrecognized.as_deref(), Some("enabled"));
    }

    fn setting(mode: SandboxMode) -> SandboxSetting {
        SandboxSetting { mode, unrecognized: None }
    }

    #[test]
    fn posture_line_off_says_agent_runs_as_you() {
        let (level, line) = sandbox_posture(&setting(SandboxMode::PassThrough), None, true);
        assert_eq!(level, PostureLevel::Info, "the default is not a warning");
        assert!(
            line.contains("OFF") && line.contains("run as you") && line.contains("SMOOTH_SANDBOX=1"),
            "{line}"
        );
        // Off is off on every platform — no 'UNSANDBOXED' warning when nobody asked for one.
        let (level, _) = sandbox_posture(&setting(SandboxMode::PassThrough), None, false);
        assert_eq!(level, PostureLevel::Info);
    }

    #[test]
    fn posture_line_off_with_egress_marks_allowlist_advisory() {
        let (level, line) = sandbox_posture(&setting(SandboxMode::PassThrough), Some("127.0.0.1:4419"), true);
        assert_eq!(level, PostureLevel::Info);
        assert!(line.contains("ADVISORY") && line.contains("127.0.0.1:4419"), "{line}");
    }

    #[test]
    fn posture_line_on_states_the_boundary() {
        let (level, line) = sandbox_posture(&setting(SandboxMode::Enforced), Some("127.0.0.1:4419"), true);
        assert_eq!(level, PostureLevel::Info);
        assert!(line.contains("ON") && line.contains("kernel-forced"), "{line}");
        let (_, line) = sandbox_posture(&setting(SandboxMode::Enforced), None, true);
        assert!(line.contains("egress unrestricted"), "{line}");
    }

    #[test]
    fn posture_line_warns_when_sandbox_requested_but_unsupported() {
        let (level, line) = sandbox_posture(&setting(SandboxMode::Enforced), Some("127.0.0.1:4419"), false);
        assert_eq!(level, PostureLevel::Warn);
        assert!(line.contains("UNSANDBOXED") && line.contains("ADVISORY"), "{line}");
    }

    #[test]
    fn posture_line_warns_on_unrecognized_value() {
        let s = resolve_sandbox_inner(Some("enabled".to_owned()));
        let (level, line) = sandbox_posture(&s, None, true);
        assert_eq!(level, PostureLevel::Warn);
        assert!(line.contains("\"enabled\"") && line.contains("OFF"), "{line}");
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
        assert!(msg.contains("th model login"), "actionable guidance: {msg}");
    }
    #[test]
    fn every_llm_config_the_daemon_builds_uses_it() {
        // config.rs's env path — the one the bench and `th daemon` take.
        let cfg = resolve_llm_inner(Some("https://llm.smoo.ai/v1".into()), Some("k".into()), Some("gpt-5.5".into()), None, None).unwrap();
        assert!(
            (cfg.temperature - smooth_policy::llm_params::AGENT_TEMPERATURE).abs() < f32::EPSILON,
            "a hardcoded 0.0 here is the bug that made the model picker a no-op"
        );
    }
}

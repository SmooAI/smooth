//! The local deployment flavor — embed smooth-operator's `LocalServer`
//! in-process (EPIC th-c89c2a).
//!
//! Instead of the daemon's bespoke `/ws`, the daemon hosts the **operator's
//! local flavor**: the canonical schema-driven WS protocol, so the official
//! widget and the polyglot SDK clients work natively. Lean build (no cloud
//! adapters — in-memory storage + backplane).
//!
//! **Auth:** the local flavor enables the operator's **strict-auth** mode, so a
//! `/ws` connection with a missing/invalid token is **rejected** (HTTP 401),
//! not degraded to anonymous. So the [`LocalTokenVerifier`] genuinely gates
//! connections — a stray local process or tailnet peer can't drive the agent.
//! (Default operator behavior is still lenient/anonymous for the embeddable
//! widget's public flow; the local flavor opts into strict.)
//!
//! This is additive: it runs alongside the bespoke `serve_persistent` path
//! while the embed is validated; the bespoke surface retires once parity lands.
//!
//! # Configuration (env)
//!
//! - `SMOOTH_LOCAL_TOKEN` — the auth token (else auto-generated at
//!   `~/.smooth/operator-token`).
//! - `SMOOTH_WORKSPACE` — the dir the sandboxed fs/shell tools are confined to
//!   (else the daemon's cwd).
//! - `SMOOTH_AGENT_CONFIRM_TOOLS` — **inherited from the operator**:
//!   comma-separated tool-name substrings that require human confirmation
//!   (write-confirmation HITL). Because the daemon *runs the operator*, setting
//!   e.g. `SMOOTH_AGENT_CONFIRM_TOOLS=bash` makes every `bash` call park and emit
//!   `write_confirmation_required`, which the served widget renders as an
//!   approve/deny prompt — the "ask" half of the permission model, for free. The
//!   kernel sandbox + egress allowlist remain the load-bearing boundary; this is
//!   defense-in-depth. (Content-aware hard-deny circuit-breakers — `rm -rf /` and
//!   friends — need a host `ToolHook` seam in the operator; see pearl th-1f694a.)
//! - `SMOOAI_GATEWAY_URL` / `SMOOAI_GATEWAY_KEY` — the LLM gateway (read by the
//!   operator); with no key the server boots and `send_message` errors cleanly.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The local flavor's agent persona — "Big Smooth", the user's own always-on
/// personal AI. Installed as the operator's default system prompt (via
/// [`LocalServerBuilder::persona`]) so every turn runs as Big Smooth instead of
/// the operator's stock customer-support agent (th-5f059b).
///
/// Two things it MUST get right, both observed broken on the stock prompt:
///   1. **Personal assistant, not support.** It must never act like
///      customer-support or volunteer "the organization's knowledge base / the
///      org's docs" unless the user explicitly asks about org knowledge.
///   2. **No reasoning narration.** The local model (deepseek-v4-flash) inlines
///      its chain-of-thought into the reply ("The user is asking a general
///      knowledge question… so I can answer directly…The song 'Let It Go'…").
///      The directive below is firm and explicit so the model replies with only
///      the answer. (The operator's engine already drops a *separate*
///      reasoning-channel from the final reply; this handles the inline case.)
const BIG_SMOOTH_PERSONA: &str = "You are Big Smooth, the user's own always-on personal AI assistant. \
You are warm, smooth, and carry a little swagger — confident and easygoing, never stuffy — and you keep it concise: \
say what matters and stop.\n\n\
You are a PERSONAL assistant, not customer support. Never describe yourself as a support agent, and never mention \
\"the organization\", \"the org's docs\", or \"the knowledge base\" unless the user explicitly asks about organization \
knowledge. When a question is general knowledge, just answer it directly from what you know.\n\n\
Answer the user DIRECTLY. Reply with only your answer — never show your reasoning, planning, or chain of thought, and \
never restate or summarize the user's question before answering. No \"The user is asking…\", no \"I searched and found \
nothing…\", no meta-narration of any kind. Just the answer, in your own smooth voice.";

/// Fast mode's model. `SMOOTH_FAST_MODE=1` points Big Smooth at this snappy Groq
/// model instead of the `coding`-route default. Chosen (groq-gpt-oss-120b) for
/// Groq speed + capability; it's non-deprecated and reasons on the harmony
/// channel, so its thinking renders as a clean "thinking" aside (th-4d8682).
/// The gateway's own `fast` routing slot is intentionally NOT used — it's stale
/// (deprecated llama). `SMOOTH_AGENT_MODEL` overrides this.
const FAST_MODEL: &str = "groq-gpt-oss-120b";

use anyhow::{Context, Result};
use async_trait::async_trait;
use smooth_operator::Tool;
use smooth_operator_server::local::LocalServer;
use smooth_operator_server::ServerConfig;
use smooth_operator_svc::auth::LocalTokenVerifier;
use smooth_operator_svc::{ToolProvider, ToolProviderContext};

/// A [`ToolProvider`] that hands the operator the daemon's kernel-sandboxed tool
/// set on every turn (the operator's `#68` injection seam): the
/// workspace-confined fs/grep set + an OS-sandboxed `bash` whose egress routes
/// through the goalie proxy. This is where the daemon's kernel-enforced security
/// re-homes onto the operator's per-turn registry.
struct SandboxedToolProvider {
    workspace: PathBuf,
    proxy: Option<String>,
}

#[async_trait]
impl ToolProvider for SandboxedToolProvider {
    async fn tools_for(&self, _ctx: &ToolProviderContext) -> Vec<Arc<dyn Tool>> {
        smooth_tools::default_tools_with_proxy(self.workspace.clone(), self.proxy.clone())
    }
}

/// The local flavor's tool provider — the daemon's kernel-sandboxed tool set.
///
/// Workspace-confined fs/grep + an OS-sandboxed `bash` routed through `proxy`.
/// Exposed so an integration/e2e test can install it on a `LocalServer` exactly
/// the way [`serve_local_flavor`] does.
#[must_use]
pub fn local_tool_provider(workspace: PathBuf, proxy: Option<String>) -> Arc<dyn ToolProvider> {
    Arc::new(SandboxedToolProvider { workspace, proxy })
}

/// The workspace the local flavor's filesystem + shell tools are confined to:
/// `SMOOTH_WORKSPACE` if set, else the daemon's current directory.
fn workspace_dir() -> PathBuf {
    std::env::var_os("SMOOTH_WORKSPACE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolve the path to the local operator token (`~/.smooth/operator-token`).
fn token_path() -> PathBuf {
    dirs_next::home_dir().map_or_else(|| PathBuf::from("operator-token"), |h| h.join(".smooth").join("operator-token"))
}

/// Resolve the path to the operator's durable storage db
/// (`~/.smooth/operator-storage.db`). `SMOOTH_OPERATOR_DB` overrides.
fn operator_storage_path() -> PathBuf {
    if let Ok(p) = std::env::var("SMOOTH_OPERATOR_DB") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs_next::home_dir().map_or_else(|| PathBuf::from("operator-storage.db"), |h| h.join(".smooth").join("operator-storage.db"))
}

/// Resolve the path to the durable schedule store (`~/.smooth/schedules.db`).
/// `SMOOTH_SCHEDULE_DB` overrides. Shared by the daemon's scheduler loop and the
/// `schedule` CLI so both read/write the same store.
#[must_use]
pub fn schedule_store_path() -> PathBuf {
    if let Ok(p) = std::env::var("SMOOTH_SCHEDULE_DB") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs_next::home_dir().map_or_else(|| PathBuf::from("schedules.db"), |h| h.join(".smooth").join("schedules.db"))
}

/// Tighten a file to owner-only (mode 600) on Unix; no-op elsewhere.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

/// Provision the local-flavor auth token, **auto-generating it on first run**.
///
/// Resolution order: `SMOOTH_LOCAL_TOKEN` (env) → `~/.smooth/operator-token`
/// (existing) → a freshly generated token persisted there (mode 600). This makes
/// the token zero-friction (no manual setup) while still gating stray local
/// processes; the served widget/SDK clients read it from the same place.
///
/// # Errors
/// Returns an error if the token directory/file can't be created or written.
pub fn provision_local_token() -> Result<String> {
    if let Ok(env_token) = std::env::var("SMOOTH_LOCAL_TOKEN") {
        let env_token = env_token.trim().to_owned();
        if !env_token.is_empty() {
            return Ok(env_token);
        }
    }
    let path = token_path();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_owned();
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    // First run: generate + persist a fresh token, owner-only.
    let token = uuid::Uuid::new_v4().simple().to_string();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, &token).with_context(|| format!("writing {}", path.display()))?;
    restrict_permissions(&path);
    tracing::info!(path = %path.display(), "provisioned a local operator token");
    Ok(token)
}

/// Explicit model override (`SMOOTH_AGENT_MODEL`) — the highest-priority model
/// selector. Wins over fast-mode and the providers routing. `None`/empty falls
/// through to the routing default.
fn model_override() -> Option<String> {
    std::env::var("SMOOTH_AGENT_MODEL").ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Whether **fast mode** is on (`SMOOTH_FAST_MODE`). Fast mode points Big Smooth
/// at the gateway's `fast` routing slot (a snappy model) instead of `coding`.
/// Treats unset / `0` / `false` / `no` / `off` as disabled.
fn fast_mode_enabled() -> bool {
    matches!(std::env::var("SMOOTH_FAST_MODE"), Ok(v) if !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false" | "no" | "off"))
}

/// Read the `smooth` provider (the llm.smoo.ai gateway) from
/// `~/.smooth/providers.json` — the credentials `th auth login smooth` writes.
/// Returns `(api_url, api_key, model)`; `None` if the file/provider/key is
/// absent. The model is taken from the given `route` slot (`coding`/`fast`/…),
/// else the provider default.
fn gateway_from_providers(route: &str) -> Option<(String, String, String)> {
    gateway_from_providers_at(&dirs_next::home_dir()?.join(".smooth").join("providers.json"), route)
}

/// [`gateway_from_providers`] against an explicit path — the testable core.
fn gateway_from_providers_at(path: &Path, route: &str) -> Option<(String, String, String)> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let smooth = v
        .get("providers")?
        .as_array()?
        .iter()
        .find(|p| p.get("id").and_then(serde_json::Value::as_str) == Some("smooth"))?;
    let url = smooth.get("api_url")?.as_str()?.to_owned();
    let key = smooth.get("api_key")?.as_str().filter(|k| !k.trim().is_empty())?.to_owned();
    let model = v
        .pointer(&format!("/routing/{route}/model"))
        .and_then(serde_json::Value::as_str)
        // Fall back to the `coding` slot, then the provider default, then a sane const.
        .or_else(|| v.pointer("/routing/coding/model").and_then(serde_json::Value::as_str))
        .or_else(|| smooth.get("default_model").and_then(serde_json::Value::as_str))
        .unwrap_or("claude-haiku-4-5")
        .to_owned();
    Some((url, key, model))
}

/// The LLM gateway config for the local flavor: the operator's env-based config
/// first (`SMOOAI_GATEWAY_*`), and when no key is set, the user's
/// `th auth login smooth` credentials from `providers.json` — so `th code` works
/// in a plain terminal with no env exports. (Proper JWT→org-session: th-f7b20f.)
fn resolve_gateway_config() -> ServerConfig {
    let mut config = smooth_operator_server::local::local_config();
    let env_has_key = config.gateway_key.as_deref().is_some_and(|k| !k.trim().is_empty());
    // Model selection, highest priority first: explicit SMOOTH_AGENT_MODEL →
    // fast-mode's `fast` routing slot → the `coding` slot.
    let fast = fast_mode_enabled();
    if !env_has_key {
        if let Some((url, key, coding_model)) = gateway_from_providers("coding") {
            config.gateway_url = url;
            config.gateway_key = Some(key);
            // Fast mode pins a current Groq model (the gateway's own `fast` slot is
            // stale). Explicit SMOOTH_AGENT_MODEL always wins.
            let default_model = if fast { FAST_MODEL.to_owned() } else { coding_model };
            config.model = model_override().unwrap_or(default_model);
        }
    } else if let Some(m) = model_override() {
        // Env-gateway path: still honor an explicit model pin.
        config.model = m;
    }
    tracing::info!(gateway = %config.gateway_url, model = %config.model, fast_mode = fast, "gateway + model resolved");
    config
}

/// Boot the operator's local deployment flavor on `addr`, gated by an
/// auto-provisioned [`LocalTokenVerifier`], and serve until Ctrl-C.
///
/// The LLM gateway is read from the environment by the operator
/// (`SMOOAI_GATEWAY_URL` / `SMOOAI_GATEWAY_KEY`); with no key the server still
/// boots and `send_message` errors cleanly.
///
/// # Errors
/// Returns an error if the token can't be provisioned or the server can't bind.
pub async fn serve_local_flavor(addr: SocketAddr) -> Result<()> {
    let token = provision_local_token()?;
    // The local flavor's tools: the workspace-confined fs/grep set + an
    // OS-sandboxed `bash` whose egress is routed through the goalie proxy (when
    // SMOOTH_EGRESS_ALLOWLIST is configured). This is where the daemon's
    // kernel-enforced security re-homes onto the operator's tool registry.
    let workspace = workspace_dir();
    let egress_proxy = crate::start_egress_proxy();
    tracing::info!(
        workspace = %workspace.display(),
        egress = egress_proxy.as_deref().unwrap_or("unrestricted"),
        "local-flavor sandboxed tools wired (per-turn via ToolProvider)",
    );
    let provider = local_tool_provider(workspace, egress_proxy);
    // Durable local storage: the operator local flavor is in-memory by default,
    // which loses every conversation/session on restart. Inject a sqlite-backed
    // adapter (via the operator's `storage()` seam) so the always-on daemon
    // persists across restarts — no Postgres (EPIC th-c89c2a, th-558df1).
    let storage_path = operator_storage_path();
    let storage = Arc::new(crate::operator_storage::SqliteStorageAdapter::open(&storage_path)?);
    tracing::info!(db = %storage_path.display(), "operator durable storage");

    let server = LocalServer::builder()
        .addr(addr)
        // LLM gateway: env (`SMOOAI_GATEWAY_*`) first, else the user's
        // `th auth login smooth` creds from providers.json — so `th code` works
        // in a plain terminal without exporting a key.
        .config(resolve_gateway_config())
        .storage(storage)
        .auth(Arc::new(LocalTokenVerifier::new(token.clone())))
        // Reject (don't degrade to anonymous) any `/ws` connection without a
        // valid token — so a stray local process / tailnet peer can't drive the
        // agent. The widget + SDK clients carry the token, so they're unaffected.
        .strict_auth(true)
        .tools(provider)
        // The agent's personality: "Big Smooth", the user's personal assistant —
        // NOT the operator's stock customer-support persona, and no reasoning
        // narration (th-5f059b).
        .persona(BIG_SMOOTH_PERSONA)
        // Serve the smooth-web SPA same-origin at `/`, with the auth token injected
        // into its index.html so the browser connects to `/ws?token=…` (validated
        // by the verifier above) — no `?api`/`?token` query string needed
        // (th-a28904). Replaces the operator's stock widget.
        .serve_spa(smooth_web::web_router_with_token(Some(&token)))
        .spawn()
        .await
        .context("spawning the local-flavor operator")?;
    tracing::info!(addr = %server.addr(), url = %format!("http://{}/", server.addr()), "smooth local-flavor operator listening (smooth-web SPA same-origin + canonical WS protocol)");

    // Reachability: if Tailscale is present and the node is up, expose the daemon
    // over the user's *tailnet* via `tailscale serve` (never funnel — tailnet-
    // private) so other devices reach it at https://<host>.<tailnet>.ts.net with
    // no query string. Best-effort: a missing/down tailscale leaves the daemon on
    // loopback. The guard lives to shutdown so its Drop tears the serve config
    // down and nothing leaks across restarts (th-ce286d).
    // Held to shutdown: its Drop tears the `tailscale serve` config back down.
    let tailscale_guard = crate::tailscale::TailscaleServe::start(server.addr().port());
    if let Some(url) = tailscale_guard.as_ref().and_then(crate::tailscale::TailscaleServe::url) {
        tracing::info!(%url, "tailnet reachability armed via `tailscale serve` (tailnet-private, not funnel)");
    }

    // Proactivity: the always-on agent fires due schedules into its *own*
    // operator as a loopback WS client (canonical send_message) — "just another
    // client on the protocol" (EPIC th-c89c2a, th-2ff975). Durable across
    // restarts via the sqlite schedule store; a missing/unwritable store disables
    // the loop without taking the daemon down.
    let _scheduler = match crate::schedule::SqliteScheduleStore::open(&schedule_store_path()) {
        Ok(store) => {
            let driver = crate::scheduler::OperatorTurnDriver::new(format!("http://{}", server.addr()), token.clone());
            let handle = crate::scheduler::spawn_scheduler(Arc::new(store), Arc::new(driver), std::time::Duration::from_secs(30));
            tracing::info!("scheduler armed (30s tick) — proactive schedules fire into the operator");
            Some(handle)
        }
        Err(e) => {
            tracing::warn!(error = %e, "scheduler disabled — could not open the schedule store");
            None
        }
    };

    tokio::signal::ctrl_c().await.ok();
    tracing::info!("shutdown signal received");
    server.shutdown().await.context("shutting down local operator")?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn big_smooth_persona_is_personal_and_no_reasoning() {
        let p = BIG_SMOOTH_PERSONA;
        // Identity: a personal assistant named Big Smooth, NOT customer support.
        assert!(p.contains("Big Smooth"), "names the persona");
        assert!(p.contains("personal"), "frames as a personal assistant");
        assert!(p.to_lowercase().contains("not customer support"), "explicitly not support");
        // The firm no-reasoning-narration directive (the core of th-5f059b).
        assert!(p.contains("never show your reasoning"), "forbids reasoning narration");
        assert!(p.contains("never restate"), "forbids restating the question");
        // Does not gratuitously volunteer the org knowledge base.
        assert!(p.contains("unless the user explicitly asks about organization"), "org-knowledge is opt-in");
    }

    #[test]
    fn gateway_from_providers_reads_smooth_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(
            &path,
            r#"{"providers":[
                {"id":"anthropic","api_url":"https://api.anthropic.com","api_key":"sk-ant"},
                {"id":"smooth","api_url":"https://llm.smoo.ai/v1","api_key":"sk-smooth","default_model":"m-default"}
            ],"routing":{"coding":{"provider":"smooth","model":"m-coding"},"fast":{"provider":"smooth","model":"m-fast"}}}"#,
        )
        .unwrap();
        let (url, key, model) = gateway_from_providers_at(&path, "coding").expect("smooth provider resolves");
        assert_eq!(url, "https://llm.smoo.ai/v1");
        assert_eq!(key, "sk-smooth");
        assert_eq!(model, "m-coding", "the coding route wins over default_model");
        // Fast mode picks the `fast` slot.
        let (_, _, fast_model) = gateway_from_providers_at(&path, "fast").expect("fast route resolves");
        assert_eq!(fast_model, "m-fast", "the fast route selects the fast model");
        // An unknown route falls back to the coding slot, then default.
        let (_, _, fallback) = gateway_from_providers_at(&path, "nonexistent").expect("falls back");
        assert_eq!(fallback, "m-coding", "unknown route falls back to coding");
    }

    #[test]
    fn fast_mode_enabled_parses_truthiness() {
        for (val, want) in [
            ("1", true),
            ("true", true),
            ("on", true),
            ("yes", true),
            ("0", false),
            ("false", false),
            ("off", false),
            ("", false),
        ] {
            std::env::set_var("SMOOTH_FAST_MODE", val);
            assert_eq!(fast_mode_enabled(), want, "SMOOTH_FAST_MODE={val:?}");
        }
        std::env::remove_var("SMOOTH_FAST_MODE");
        assert!(!fast_mode_enabled(), "unset is disabled");
    }

    #[test]
    fn model_override_trims_and_filters_empty() {
        std::env::set_var("SMOOTH_AGENT_MODEL", "  groq-gpt-oss-20b  ");
        assert_eq!(model_override().as_deref(), Some("groq-gpt-oss-20b"));
        std::env::set_var("SMOOTH_AGENT_MODEL", "   ");
        assert_eq!(model_override(), None, "blank override is ignored");
        std::env::remove_var("SMOOTH_AGENT_MODEL");
        assert_eq!(model_override(), None);
    }

    #[test]
    fn gateway_from_providers_none_when_no_key_or_provider() {
        let dir = tempfile::tempdir().unwrap();
        // No `smooth` provider.
        let p1 = dir.path().join("a.json");
        std::fs::write(&p1, r#"{"providers":[{"id":"anthropic","api_url":"x","api_key":"k"}]}"#).unwrap();
        assert!(gateway_from_providers_at(&p1, "coding").is_none());
        // `smooth` present but key empty.
        let p2 = dir.path().join("b.json");
        std::fs::write(&p2, r#"{"providers":[{"id":"smooth","api_url":"x","api_key":""}]}"#).unwrap();
        assert!(gateway_from_providers_at(&p2, "coding").is_none());
        // Missing file.
        assert!(gateway_from_providers_at(&dir.path().join("nope.json"), "coding").is_none());
    }

    #[test]
    fn provision_prefers_env_token() {
        std::env::set_var("SMOOTH_LOCAL_TOKEN", "  env-tok-123  ");
        assert_eq!(provision_local_token().unwrap(), "env-tok-123", "env token wins, trimmed");
        std::env::remove_var("SMOOTH_LOCAL_TOKEN");
    }

    #[test]
    fn provision_generates_and_persists_when_unset() {
        // Isolate HOME so we read/write a temp ~/.smooth/operator-token.
        std::env::remove_var("SMOOTH_LOCAL_TOKEN");
        let home = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let first = provision_local_token().unwrap();
        assert!(!first.is_empty(), "a token is generated");
        // The same token is returned on the next call (persisted, not regenerated).
        let second = provision_local_token().unwrap();
        assert_eq!(first, second, "token persists across calls");
        assert!(home.path().join(".smooth/operator-token").exists());

        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
    }
}

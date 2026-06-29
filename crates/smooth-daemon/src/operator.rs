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

/// Read the `smooth` provider (the llm.smoo.ai gateway) from
/// `~/.smooth/providers.json` — the credentials `th auth login smooth` writes.
/// Returns `(api_url, api_key, model)`; `None` if the file/provider/key is
/// absent. The model prefers the `coding` route, else the provider default.
fn gateway_from_providers() -> Option<(String, String, String)> {
    gateway_from_providers_at(&dirs_next::home_dir()?.join(".smooth").join("providers.json"))
}

/// [`gateway_from_providers`] against an explicit path — the testable core.
fn gateway_from_providers_at(path: &Path) -> Option<(String, String, String)> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let smooth = v
        .get("providers")?
        .as_array()?
        .iter()
        .find(|p| p.get("id").and_then(serde_json::Value::as_str) == Some("smooth"))?;
    let url = smooth.get("api_url")?.as_str()?.to_owned();
    let key = smooth.get("api_key")?.as_str().filter(|k| !k.trim().is_empty())?.to_owned();
    let model = v
        .pointer("/routing/coding/model")
        .and_then(serde_json::Value::as_str)
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
    if !env_has_key {
        if let Some((url, key, model)) = gateway_from_providers() {
            tracing::info!(gateway = %url, model = %model, "gateway key sourced from ~/.smooth/providers.json (smooth provider)");
            config.gateway_url = url;
            config.gateway_key = Some(key);
            // Only override the model when the env didn't pin one.
            if std::env::var("SMOOTH_AGENT_MODEL").is_err() {
                config.model = model;
            }
        }
    }
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
        // Serve the official widget at `/`, with the same token injected so the
        // browser connects to `/ws?token=…` (validated by the verifier above).
        .serve_widget(Some(token.clone()))
        .spawn()
        .await
        .context("spawning the local-flavor operator")?;
    tracing::info!(addr = %server.addr(), url = %format!("http://{}/", server.addr()), "smooth local-flavor operator listening (widget + canonical WS protocol)");

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
    fn gateway_from_providers_reads_smooth_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(
            &path,
            r#"{"providers":[
                {"id":"anthropic","api_url":"https://api.anthropic.com","api_key":"sk-ant"},
                {"id":"smooth","api_url":"https://llm.smoo.ai/v1","api_key":"sk-smooth","default_model":"m-default"}
            ],"routing":{"coding":{"provider":"smooth","model":"m-coding"}}}"#,
        )
        .unwrap();
        let (url, key, model) = gateway_from_providers_at(&path).expect("smooth provider resolves");
        assert_eq!(url, "https://llm.smoo.ai/v1");
        assert_eq!(key, "sk-smooth");
        assert_eq!(model, "m-coding", "the coding route wins over default_model");
    }

    #[test]
    fn gateway_from_providers_none_when_no_key_or_provider() {
        let dir = tempfile::tempdir().unwrap();
        // No `smooth` provider.
        let p1 = dir.path().join("a.json");
        std::fs::write(&p1, r#"{"providers":[{"id":"anthropic","api_url":"x","api_key":"k"}]}"#).unwrap();
        assert!(gateway_from_providers_at(&p1).is_none());
        // `smooth` present but key empty.
        let p2 = dir.path().join("b.json");
        std::fs::write(&p2, r#"{"providers":[{"id":"smooth","api_url":"x","api_key":""}]}"#).unwrap();
        assert!(gateway_from_providers_at(&p2).is_none());
        // Missing file.
        assert!(gateway_from_providers_at(&dir.path().join("nope.json")).is_none());
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

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

/// The local flavor's tool provider — the daemon's kernel-sandboxed tool set
/// (workspace-confined fs/grep + an OS-sandboxed `bash` routed through `proxy`).
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
    let server = LocalServer::builder()
        .addr(addr)
        .auth(Arc::new(LocalTokenVerifier::new(token.clone())))
        // Reject (don't degrade to anonymous) any `/ws` connection without a
        // valid token — so a stray local process / tailnet peer can't drive the
        // agent. The widget + SDK clients carry the token, so they're unaffected.
        .strict_auth(true)
        .tools(provider)
        // Serve the official widget at `/`, with the same token injected so the
        // browser connects to `/ws?token=…` (validated by the verifier above).
        .serve_widget(Some(token))
        .spawn()
        .await
        .context("spawning the local-flavor operator")?;
    tracing::info!(addr = %server.addr(), url = %format!("http://{}/", server.addr()), "smooth local-flavor operator listening (widget + canonical WS protocol)");
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

//! Big Smooth, reborn — the always-on, single-tenant personal-agent daemon.
//!
//! `smooth-daemon` is a clean rewrite of `smooth-bigsmooth` on top of the
//! [`smooth_operator`] agent engine. It targets a **single trusted operator**
//! self-hosting their own instance (hermes-style) on a personal machine
//! reachable over SSH/Tailscale — NOT a multi-tenant service. Because there is
//! no untrusted tenant, the microsandbox microVM substrate is dropped; security
//! becomes a kernel-enforced OS sandbox on tool subprocesses + an egress proxy
//! + a Claude-Code-style auto-mode permission engine (see EPIC th-c89c2a).
//!
//! # Shape (borrowed from hermes + opencode)
//!
//! One always-on daemon owns the durable state and the agent runtime; every UI
//! (the `th code` ratatui TUI, the `smooth-web` React SPA, and — later —
//! messaging-platform adapters) is a thin event consumer over a durable
//! event stream + WebSocket token path.
//!
//! ```text
//! daemon (axum, loopback + tailnet)
//!   ├─ smooth-operator engine (Agent::run_with_channel per session)
//!   ├─ durable event log  → /api/event  (SSE, monotonic seq, cursor resume)
//!   ├─ token stream       → /ws         (TaskStart/Steer/Cancel)
//!   ├─ SessionRunCoordinator (one fiber/session, concurrent across keys)
//!   └─ Dolt-backed session/checkpoint/memory + approval/completion queues
//! ```
//!
//! # Build-out status
//!
//! - **Phase 0 (th-f30175, this crate's scaffold):** the durable-event core
//!   ([`event`]) — the monotonic-seq envelope + cursor-resume contract that the
//!   `/api/event` SSE endpoint and the Dolt event table are both built on.
//! - **Phase 1 (th-64fbe8):** axum server, session store, coordinator, frontend
//!   reconnect.
//! - Later phases: Dolt persistence, the auto-mode permission engine, the
//!   reimagined React control surface, scheduling + messaging surfaces.

pub mod approval;
pub mod config;
pub mod coordinator;
pub mod event;
pub mod hook;
pub mod messages;
pub mod permission;
pub mod runner;
pub mod schedule;
pub mod server;
pub mod session;
pub mod sqlite;
pub mod wire;

pub use approval::ApprovalCoordinator;
pub use coordinator::{SessionRunCoordinator, StartError};
pub use event::{DaemonEvent, EventKind, EventStore, InMemoryEventLog, Seq};
pub use hook::PermissionHook;
pub use messages::{InMemoryMessageStore, MessageStore, StoredMessage};
pub use permission::{Decision, PermissionEngine, PermissionMode};
pub use runner::{run_task, TaskSpec};
pub use schedule::{Schedule, ScheduleKind};
pub use server::{build_router, serve, serve_on, serve_with_shutdown, AppState};
pub use session::{InMemorySessionStore, Session, SessionStatus, SessionStore};
pub use wire::{map_agent_event, ClientEvent, PriorMessage, ServerEvent};

/// Build durable state, start the egress boundary if configured, and serve on
/// `addr` until shutdown.
///
/// This is the canonical daemon entry — used by **both** the standalone
/// `smooth-daemon` binary and `th daemon`, so the egress proxy starts the same
/// way regardless of how the daemon is launched (the bug this consolidates: the
/// `th daemon` path previously served without ever starting the proxy).
///
/// # Errors
/// Returns an error if the durable DB or the socket cannot be opened.
pub async fn serve_persistent(addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let mut state = AppState::persistent_default()?;
    tracing::info!(db = %AppState::default_db_path().display(), "durable state");
    start_egress_if_configured(&mut state);
    serve(state, addr).await
}

/// Start the goalie egress proxy on a background task and point the bash tool at
/// it, when `SMOOTH_EGRESS_ALLOWLIST` is configured. No-op otherwise.
fn start_egress_if_configured(state: &mut AppState) {
    let Some(setup) = config::resolve_egress() else { return };
    let audit = match smooth_goalie::AuditLogger::new(&config::egress_audit_path().to_string_lossy()) {
        Ok(audit) => audit,
        Err(e) => {
            tracing::error!(error = %e, "egress audit log could not be opened — egress boundary NOT started");
            return;
        }
    };
    if !setup.rejected.is_empty() {
        tracing::warn!(rejected = ?setup.rejected, "egress allowlist dropped invalid entries");
    }
    tracing::info!(proxy = %setup.proxy_addr, hosts = setup.allowlist.len(), "egress boundary ON");
    let proxy_addr = setup.proxy_addr.clone();
    let allowlist = setup.allowlist;
    tokio::spawn(async move {
        if let Err(e) = smooth_goalie::run_proxy_local(&proxy_addr, allowlist, audit).await {
            tracing::error!(error = %e, "egress proxy exited — sandboxed egress now fails closed");
        }
    });
    state.egress_proxy = Some(setup.proxy_addr);
}

/// The crate version, surfaced by the daemon's health/status endpoint so
/// frontends can detect a server upgrade across a reconnect.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty(), "crate version should be populated from Cargo");
    }
}

//! Big Smooth, reborn — the always-on, single-tenant personal-agent daemon.
//!
//! `smooth-daemon` runs **smooth-operator's local deployment flavor** as its
//! one and only agent runtime. It targets a **single trusted operator**
//! self-hosting their own instance (hermes-style) on a personal machine
//! reachable over SSH/Tailscale — NOT a multi-tenant service. The microsandbox
//! microVM substrate is gone; security is a kernel-enforced OS sandbox on tool
//! subprocesses + an egress allowlist proxy (EPIC th-c89c2a).
//!
//! # Shape — one operator, one protocol (the north star)
//!
//! `th daemon` *is* the operator. There is **no bespoke server, no bespoke WS
//! protocol, no second agent loop** — the daemon hosts the operator's
//! [`LocalServer`](smooth_operator_server::local::LocalServer), and every
//! surface (the `th code` TUI, the official widget / `smooth-web`, and — later —
//! messaging adapters) is a thin client on the **canonical operator protocol**.
//!
//! ```text
//! th daemon  →  smooth-operator LocalServer (:8787, canonical WS + widget)
//!   ├─ kernel-sandboxed tools (per-turn ToolProvider; egress via goalie)
//!   └─ durable local storage  (sqlite StorageAdapter — survives restart, no Postgres)
//! ```
//!
//! This module exposes [`serve_local_flavor`] (the entry the binary calls) and
//! [`start_egress_proxy`] (the shared egress boundary). The bespoke
//! `serve_persistent` agent loop + its server/wire/runner/coordinator/scheduler/
//! permission/sqlite modules were deleted once the operator path reached parity.

pub mod config;
pub mod operator;
mod operator_storage;
pub mod schedule;
pub mod scheduler;
pub mod tailscale;

pub use operator::{local_tool_provider, serve_local_flavor};
pub use schedule::{InMemoryScheduleStore, Schedule, ScheduleKind, ScheduleStore, SqliteScheduleStore};
pub use scheduler::{spawn_scheduler, tick, OperatorTurnDriver, TurnDriver};
pub use tailscale::TailscaleServe;

/// Resolve the egress config; if set, start the goalie proxy on a background
/// task and return its loopback addr (for routing the bash tool's egress
/// through it). Returns `None` when `SMOOTH_EGRESS_ALLOWLIST` is unset (egress
/// unrestricted) or the audit log can't be opened. Shared by the bespoke
/// daemon and the operator local flavor so both gate egress the same way.
pub(crate) fn start_egress_proxy() -> Option<String> {
    let setup = config::resolve_egress()?;
    let audit = match smooth_goalie::AuditLogger::new(&config::egress_audit_path().to_string_lossy()) {
        Ok(audit) => audit,
        Err(e) => {
            tracing::error!(error = %e, "egress audit log could not be opened — egress boundary NOT started");
            return None;
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
    Some(setup.proxy_addr)
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

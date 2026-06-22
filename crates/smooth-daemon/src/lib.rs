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

pub mod config;
pub mod coordinator;
pub mod event;
pub mod runner;
pub mod server;
pub mod session;
pub mod wire;

pub use coordinator::{SessionRunCoordinator, StartError};
pub use event::{DaemonEvent, EventKind, EventStore, InMemoryEventLog, Seq};
pub use runner::{run_task, TaskSpec};
pub use server::{build_router, serve, serve_on, serve_with_shutdown, AppState};
pub use session::{InMemorySessionStore, Session, SessionStatus, SessionStore};
pub use wire::{map_agent_event, ClientEvent, PriorMessage, ServerEvent};

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

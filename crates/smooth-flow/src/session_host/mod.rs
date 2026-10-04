//! The SmoothFlow session host (ADR-011, epic th-ce4f88).
//!
//! A per-session `smooth-daemon flow-host` process owns the PTY, is the
//! agent's parent, and runs every output byte through a headless
//! libghostty-vt. The daemon is its client and the host outlives it, so sessions survive a
//! daemon crash, restart or update. The wire protocol, record and adoption
//! rules are specified in docs/Architecture/SmoothFlow-Session-Host.md.
//!
//! - [`protocol`]: framing, message types, version negotiation.
//! - [`record`]: the host record and its private directory.
//! - [`server`]: the host process itself ([`server::run`]).
//! - [`client`]: spawn a host and drive it ([`client::HostClient`]).
//! - [`adopt`]: re-attach to the hosts a previous daemon left running.
//!
//! Unix only for now: on Windows `server::run` reports `ready:false` and
//! the client and adoption are not built (th-2b32a6, the named pipe).

pub mod protocol;
pub mod record;
pub mod server;

#[cfg(unix)]
pub mod adopt;
#[cfg(unix)]
pub mod client;

//! Smooth Goalie — in-VM HTTP forward proxy. Delegates every access decision
//! to Wonk and writes JSON-lines audit entries for every allowed or blocked
//! request.
//!
//! This library surface exists so integration tests (and, in the future, an
//! in-process use of Goalie by Big Smooth for local-only agents) can spin up
//! the proxy without going through the `smooth-goalie` binary.

pub mod audit;
pub mod proxy;
pub mod wonk;

pub use audit::{AuditEntry, AuditLogger};
pub use proxy::run_proxy;
pub use wonk::{NetworkCheckRequest, WonkClient, WonkDecision};

//! Smooth Goalie — HTTP forward proxy. Decides every request against an
//! allowlist and writes JSON-lines audit entries for every allowed or blocked
//! request.
//!
//! **How it is used today:** `smooth-daemon`'s `start_egress_proxy` runs
//! [`run_proxy_local`] in-process with an [`EgressAllowlist`] and an
//! [`AuditLogger`]. That loopback proxy is the daemon's **egress boundary** —
//! `smooth-tools`' kernel sandbox denies direct outbound network and points
//! `HTTP(S)_PROXY` at it, so off-box traffic must clear the allowlist here.
//!
//! The [`wonk`] delegation path and the `smooth-goalie` binary are leftovers
//! from the removed microVM stack (2026-07, pearl th-f4a801), where Goalie ran
//! inside the VM and asked Wonk for each decision. Nothing in the tree drives
//! that path now. New callers want [`run_proxy_local`] / [`run_proxy_with`].

pub mod allowlist;
pub mod audit;
pub mod proxy;
pub mod wonk;

pub use allowlist::{normalize_hostname, EgressAllowlist};
pub use audit::{AuditEntry, AuditLogger};
pub use proxy::{run_proxy, run_proxy_local, run_proxy_with, NetworkDecider};
pub use wonk::{NetworkCheckRequest, WonkClient, WonkDecision};

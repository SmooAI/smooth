//! # Smooth Diver
//!
//! The Pearl Diver — Boardroom cast member that owns the pearl lifecycle.
//! Creates pearls on dispatch, closes on completion, tracks sub-pearls,
//! manages the work model (parent-child, deps, labels, costs), and syncs
//! bidirectionally with Jira.

pub mod jira;
pub mod server;
pub mod store;

pub use jira::JiraClient;
pub use server::{build_router, build_router_with_state, AppState};
pub use store::{CompleteRequest, CostEntry, DispatchRequest, DispatchResult, DiverStore};

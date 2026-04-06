//! Smooth Pearls — built-in dependency-graph work-item tracker.
//!
//! Backed by embedded Dolt (via `smooth-dolt` Go binary) for
//! version-controlled, git-syncable pearl data.
//!
//! Per-project data lives in `.smooth/dolt/` (synced via Dolt remotes
//! or git refs). Global registry at `~/.smooth/` tracks all projects.

pub mod dolt;
pub mod query;
pub mod registry;
#[allow(clippy::missing_errors_doc)]
pub mod store;
pub mod tools;
pub mod types;

pub use dolt::SmoothDolt;
pub use query::PearlQuery;
pub use registry::Registry;
pub use store::PearlStore;
pub use tools::register_pearl_tools;
pub use types::{NewPearl, Pearl, PearlComment, PearlDepType, PearlDependency, PearlHistoryEntry, PearlStats, PearlStatus, PearlType, PearlUpdate, Priority};

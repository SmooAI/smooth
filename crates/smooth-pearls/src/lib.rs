//! Smooth Pearls — built-in dependency-graph work-item tracker.
//!
//! Inspired by the beads system. Replaces the previous `smooth-issues` crate.
//! Pearls have statuses, priorities, dependencies, labels, comments, and
//! history, all backed by a single SQLite file under `~/.smooth/smooth.db`.

pub mod query;
#[allow(clippy::missing_errors_doc)]
pub mod store;
pub mod tools;
pub mod types;

pub use query::PearlQuery;
pub use store::PearlStore;
pub use tools::register_pearl_tools;
pub use types::{NewPearl, Pearl, PearlComment, PearlDepType, PearlDependency, PearlHistoryEntry, PearlStats, PearlStatus, PearlType, PearlUpdate, Priority};

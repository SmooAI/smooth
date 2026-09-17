//! Smooth Pearls — built-in dependency-graph work-item tracker.
//!
//! One machine-global SQLite database (`~/.smooth/pearls.db`) holds every
//! project's pearls, keyed by the canonical project root (pearl
//! th-d3e842). `~/.smooth/registry.json` lists the known projects.

pub mod mail_store;
pub mod memory;
pub mod memory_tools;
pub mod query;
pub mod registry;
#[allow(clippy::missing_errors_doc)]
pub mod store;
pub mod tools;
pub mod types;

pub use mail_store::{AgentStatus, MailAgent, MailMessage, MailStore, MessageKind};
pub use memory::{Memory, MemoryStore};
pub use memory_tools::register_memory_tools;
pub use query::PearlQuery;
pub use registry::Registry;
pub use store::{default_db_path, resolve_project_root, ImportOutcome, PearlStore, SyncMapEntry};
pub use tools::register_pearl_tools;
pub use types::{NewPearl, Pearl, PearlComment, PearlDepType, PearlDependency, PearlHistoryEntry, PearlStats, PearlStatus, PearlType, PearlUpdate, Priority};

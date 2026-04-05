//! Smooth Issues — built-in issue tracker with SQLite backend.

pub mod query;
#[allow(clippy::missing_errors_doc)]
pub mod store;
pub mod tools;
pub mod types;

pub use query::IssueQuery;
pub use store::IssueStore;
pub use tools::register_issue_tools;
pub use types::{Comment, DepType, Dependency, HistoryEntry, Issue, IssueStats, IssueStatus, IssueType, IssueUpdate, NewIssue, Priority};

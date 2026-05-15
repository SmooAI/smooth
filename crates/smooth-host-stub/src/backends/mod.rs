//! Concrete `Backend` implementations.
//!
//! Each module wraps a single host CLI shellout. Pearl
//! th-893801 Phase 2.

pub mod aws;
pub mod github;

pub use aws::AwsStsBackend;
pub use github::GitHubBackend;

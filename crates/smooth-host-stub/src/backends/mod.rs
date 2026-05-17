//! Concrete `Backend` implementations.
//!
//! Each module wraps a single host CLI shellout. Pearl
//! th-893801 Phase 2.

pub mod aws;
pub mod gcloud;
pub mod github;

pub use aws::AwsStsBackend;
pub use gcloud::GcloudBackend;
pub use github::GitHubBackend;

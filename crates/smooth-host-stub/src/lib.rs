//! Smooth Host Stub — credential broker bridging the single sandbox
//! VM to host-resident CLIs.
//!
//! Pearl th-893801 Phase 2 iter-4a. Runs as `smooth-host-stub` on the
//! host. The sandbox sees a UDS bind-mounted at `/run/smooth/host.sock`
//! and dials this server when an in-VM tool needs a credential for a
//! known server (GitHub, AWS, GCR, ECR…).
//!
//! Trust rule: the host stub never trusts the sandbox. Every
//! `IssueCredential` call is gated by sandbox-side Narc through the
//! Ask flow AND re-validated here against the configured backend
//! allowlist. Unknown `server_url`s return `NOT_FOUND` regardless of
//! what the sandbox claims.
//!
//! Architecture:
//!
//! ```text
//!   sandbox tool (e.g. gh)
//!     │ runs git-credential-smooth shim
//!     ▼
//!   shim dials /run/smooth/host.sock
//!     │ gRPC IssueCredential(server_url)
//!     ▼
//!   smooth-host-stub server
//!     │ matches server_url against Backend::server_globs
//!     │ asks the matched Backend for a fresh credential
//!     ▼
//!   Backend impl shells out to `gh auth token` / `aws sts get-session-token` / …
//! ```

#![allow(clippy::expect_used)]

/// Tonic-generated proto types for the HostStub gRPC surface
/// (pearl th-893801).
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    unused_qualifications,
    missing_docs,
    clippy::derive_partial_eq_without_eq
)]
pub mod pb {
    tonic::include_proto!("smooth.host_stub.v1");
}

pub mod backend;
pub mod backends;
pub mod registry;
pub mod server;

pub use backend::{Backend, BackendError, CredentialRequest, IssuedCredential, ScopeHint};
pub use backends::{AwsStsBackend, GitHubBackend};
pub use registry::BackendRegistry;
pub use server::{serve_uds, HostStubServer};

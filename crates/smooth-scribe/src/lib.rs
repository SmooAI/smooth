pub mod forwarder;
pub mod hook;
pub mod log_entry;
pub mod server;
pub mod store;

pub use forwarder::{spawn as spawn_forwarder, ForwarderHandle};
pub use log_entry::{LogEntry, LogLevel};
pub use store::{LogStore, MemoryLogStore, Query};

/// Tonic-generated proto types for the Scribe gRPC surface
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
    tonic::include_proto!("smooth.scribe.v1");
}

/// gRPC server adapter — wraps a `Logger` trait implementation as
/// the proto-generated Scribe service. Production Logger uses the
/// existing MemoryLogStore; tests stub it.
pub mod grpc;

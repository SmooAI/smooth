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

/// gRPC server adapter.
///
/// Wraps a `Logger` trait implementation as the proto-generated
/// Scribe service. Production Logger uses the existing
/// `MemoryLogStore`; tests stub it.
pub mod grpc;

/// Production wiring for `MemoryLogStore`.
///
/// Adds proto<->domain conversion and implements `grpc::Logger`.
/// Pearl th-893801 iter-3c.
pub mod store_grpc;

pub use store_grpc::{adapter_for_memory_store, entry_from_pb, entry_to_pb, GrpcLogStoreAdapter};

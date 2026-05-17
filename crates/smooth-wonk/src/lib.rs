//! Smooth Wonk — per-VM access control authority.
//!
//! Library surface exposes the policy holder, negotiator, and HTTP server so
//! integration tests (and, in the future, embedded callers) can spin a Wonk
//! in-process without going through the binary.

pub mod hook;
pub mod narc_client;
pub mod narc_grpc_uds;
pub mod negotiate;
pub mod policy;
pub mod server;

/// Tonic-generated proto types for the Wonk gRPC surface (pearl
/// th-893801). build.rs compiles proto/wonk.proto via tonic-build.
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
    tonic::include_proto!("smooth.wonk.v1");
}

/// gRPC server adapter — implements the proto-generated Wonk trait
/// in terms of a small `Checker` trait that abstracts the
/// CheckNetwork/Tool/Cli/File logic. The production Checker is
/// `smooth_wonk::server::AppState`; tests stub it.
pub mod grpc;

/// Production Checker impl on AppState. Pearl th-893801 iter-3b.
pub mod checker;

pub use hook::WonkHook;
pub use narc_client::{NarcClient, NarcEscalator};
pub use narc_grpc_uds::NarcGrpcUds;
pub use negotiate::{AccessRequest, AccessResponse, Negotiator};
pub use policy::PolicyHolder;
pub use server::{build_router, run_server, AppState, CheckResponse};

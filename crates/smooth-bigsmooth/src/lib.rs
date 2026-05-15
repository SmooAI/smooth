//! Big Smooth — orchestrator, policy generation, sandbox management, API server.

/// Tonic-generated proto types for the BigSmooth gRPC surface
/// (pearl th-893801). build.rs compiles proto/bigsmooth.proto with
/// the narc.proto types routed through smooth-narc's `pb` module.
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
    tonic::include_proto!("smooth.bigsmooth.v1");
}

/// gRPC server adapter — wraps an `Orchestrator` trait
/// implementation as the proto-generated BigSmooth service.
/// Production wiring (linking the existing AppState into the trait)
/// lands in iter-3.
pub mod grpc;

/// Production Judge impl on BoardroomNarc + serve_uds helper.
/// Pearl th-893801 iter-3a.
pub mod narc_grpc;

/// Production Orchestrator impl over AccessStore. Pearl th-893801 iter-3d.
pub mod orchestrator_grpc;

/// Single-process gRPC cast bootstrap. Pearl th-893801 iter-3e.
pub mod single_process;

/// Tonic UDS client adapters for the in-VM cast. Pearl th-893801 iter-3f.
pub mod tonic_clients;

pub mod access;
pub mod audit;
pub mod boardroom;
pub mod boardroom_narc;
pub mod chat_tools;
pub mod creds;
pub mod host_tools;
pub mod teammates;

pub mod diver_client;
pub mod events;
pub mod jira;
pub mod operator_client;
pub mod orchestrator;
pub mod pearls;
pub mod policy;
pub mod pool;
pub mod port_cache;
pub mod sandbox;
pub mod search;
pub mod server;
pub mod session;
pub mod tailscale;
pub mod thoughts;
pub mod tool_api;
pub mod tools;
pub mod web_search;
pub mod wonk_grants;
pub mod ws;

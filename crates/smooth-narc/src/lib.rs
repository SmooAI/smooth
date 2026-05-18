pub mod access_wire;
pub mod alert;
pub mod detectors;
pub mod hook;
pub mod judge;

/// Tonic-generated protobuf types + service traits for the Narc
/// gRPC surface (pearl th-893801). The build.rs in this crate
/// compiles `proto/narc.proto` and `include!`s the result here.
///
/// To implement a server: `impl pb::narc_server::Narc for YourType`.
/// To call a server: `pb::narc_client::NarcClient::new(channel)`.
//
// Suppress clippy on tonic-generated code — we don't control the
// codegen output and several lints fire by design on it.
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
    tonic::include_proto!("smooth.narc.v1");
}

/// Conversion helpers between the existing in-crate types
/// (`smooth_narc::judge::*`) and the proto-generated equivalents.
/// Separating the wire types from the domain types lets us evolve
/// each independently and centralises the conversion logic.
pub mod convert;

/// gRPC server adapter — wraps a `Judge` trait implementation as a
/// tonic service. The production Judge is
/// `smooth_bigsmooth::safehouse_narc::SafehouseNarc`; tests can
/// implement Judge with a stub.
pub mod grpc;

pub use access_wire::{AccessEvent, AccessKind, AccessResolution, NewAccessRequest, PendingAccessRequest, ResolutionVerdict};
pub use alert::{Alert, Severity};
pub use detectors::{detect_dangerous_cli, CliGuard, DetectorResult, SecretDetector, WriteGuard};
pub use hook::NarcHook;
pub use judge::{
    rule_engine_decide, Decision, DecisionCache, JudgeDecision, JudgeKind, JudgeRequest, Scope, DANGEROUS_CLI_SUBSTRINGS, DANGEROUS_DOMAIN_SUFFIXES,
    OBVIOUSLY_SAFE_DOMAIN_SUFFIXES,
};

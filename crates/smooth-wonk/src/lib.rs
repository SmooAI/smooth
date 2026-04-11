//! Smooth Wonk — per-VM access control authority.
//!
//! Library surface exposes the policy holder, negotiator, and HTTP server so
//! integration tests (and, in the future, embedded callers) can spin a Wonk
//! in-process without going through the binary.

pub mod hook;
pub mod narc_client;
pub mod negotiate;
pub mod policy;
pub mod server;

pub use hook::WonkHook;
pub use narc_client::NarcClient;
pub use negotiate::{AccessRequest, AccessResponse, Negotiator};
pub use policy::PolicyHolder;
pub use server::{build_router, run_server, AppState, CheckResponse};

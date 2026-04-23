//! Bootstrap Bill — The Board's host-side broker.
//!
//! Bill is cursed to walk between worlds: he lives on the host because the
//! hypervisor lives there, but he serves **The Board** (the Boardroom cast)
//! and takes his orders from Big Smooth.
//!
//! # Why Bill exists
//!
//! Big Smooth runs inside a Boardroom microVM. HVF on Apple Silicon has no
//! nested virtualization, so Big Smooth cannot call `microsandbox` from
//! inside its own VM to spawn operator pods. Bill is the one process on the
//! host that holds `microsandbox::Sandbox` handles; everyone else calls him
//! over TCP loopback (via `host.containers.internal` when the caller lives
//! inside a VM).
//!
//! # Protocol
//!
//! Line-delimited JSON over TCP. One request per connection, one response
//! line (terminal), then close. Keep it dumb: no request IDs, no multiplexing.
//!
//! * [`protocol::BillRequest`] — the request types Bill accepts.
//! * [`protocol::BillResponse`] — the reply types, including terminal success
//!   and error variants.
//!
//! # Modules
//!
//! * [`protocol`] — wire types.
//! * [`server`] — TCP accept loop + dispatch to the microsandbox registry.
//! * [`client`] — [`client::BillClient`] used by Big Smooth (or any other
//!   Boardroom cast member) to talk to Bill.

pub mod client;
#[cfg(feature = "server")]
pub mod project_cache;
pub mod protocol;
#[cfg(feature = "server")]
pub mod server;

pub use client::BillClient;
pub use protocol::{BillRequest, BillResponse, BindMountSpec, PortMapping, SandboxSpec};

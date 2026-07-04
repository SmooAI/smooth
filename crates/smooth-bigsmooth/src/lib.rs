//! Big Smooth — orchestrator, policy generation, API server.

pub mod access;
pub mod audit;
pub mod auto_mode;
pub mod safehouse_narc;

/// `SafehouseNarc` keeps its legacy name on the type but new code
/// should prefer `Narc` — the central LLM-judge access arbiter.
/// Both names refer to the same struct.
pub use safehouse_narc::SafehouseNarc as Narc;

pub mod chat_tools;
pub mod host_tools;
pub mod teammates;

pub mod diver_client;
pub mod events;
pub mod jira;
pub mod orchestrator;
pub mod pearls;
pub mod policy;
pub mod search;
pub mod sep;
pub mod server;
pub mod session;
pub mod tailscale;
pub mod thoughts;
pub mod tool_api;
pub mod tools;
pub mod ui_relay;
pub mod web_search;
pub mod wonk_grants;
pub mod ws;

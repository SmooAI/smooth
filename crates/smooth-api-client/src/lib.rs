//! Smoo AI platform API client.
//!
//! The bulk of this crate is generated at build time by progenitor
//! from `crates/smooth-api-client/openapi.json` — see `build.rs`. We
//! re-export the generated `Client` plus typed models under `pb` and
//! layer a thin auth wrapper on top.
//!
//! # Typical usage
//!
//! ```no_run
//! # use smooth_api_client::SmoothApiClient;
//! # async fn ex() -> anyhow::Result<()> {
//! let client = SmoothApiClient::from_disk()?; // loads ~/.smooth/auth/smooai.json
//! let me = client.pb().get_profile().await?;
//! println!("logged in as {}", me.into_inner().email);
//! # Ok(()) }
//! ```
//!
//! For interactive login, see the `auth` module — `start_login()`
//! kicks off the device-flow handshake and `poll_until_complete()`
//! blocks until the user approves in the browser.

pub mod auth;
pub mod client;
pub mod credentials;

/// Generated progenitor code. Re-exported as `pb` (protobuf-style name
/// for "the wire types"). Consumers shouldn't usually need to dip in
/// here — use `SmoothApiClient` from the crate root instead.
pub mod pb {
    #![allow(clippy::all, clippy::pedantic, clippy::nursery, unused_imports, dead_code, missing_docs, unreachable_pub)]
    include!(concat!(env!("OUT_DIR"), "/codegen.rs"));
}

pub use client::SmoothApiClient;
pub use credentials::{Credentials, CredentialsStore};

/// Default production base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.smoo.ai";

/// Override base URL with `SMOOAI_API_URL` (matches the convention
/// `apps/web` uses for its own config — pearl th-config-api-url).
pub fn base_url() -> String {
    std::env::var("SMOOAI_API_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

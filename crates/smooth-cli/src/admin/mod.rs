//! `th admin` — Smoo AI superadmin operations.
//!
//! Calls the `/admin/*` endpoints on `api.smoo.ai` using the
//! Supabase user JWT persisted by `th auth login`. The backend
//! gates every `/admin/*` route with `requireSuperAdmin`, so this
//! command tree is a no-op unless your user has the admin role
//! (403 on every call otherwise — and the client prints a clear
//! "your user lacks the requireSuperAdmin role" message).
//!
//! Cardinal use case: stand up a new customer org end-to-end.
//! Today's surface covers org + user CRUD + product activation,
//! which is most of the ceremony. The remaining four steps
//! (parent-relationships, managed-websites, b2m-key mint, m2m-key
//! mint) need backend endpoints that don't exist yet — pearl
//! th-feebd2 + the four smooai-side backend pearls.

use anyhow::Result;
use clap::Subcommand;

pub mod client;
pub mod org;
pub mod render;
pub mod user;

#[derive(Debug, Subcommand)]
pub enum AdminCommands {
    /// User operations: list / search / roles / magic-link.
    User {
        #[command(subcommand)]
        cmd: user::UserCommands,
    },
    /// Organization operations: list / show / create / members /
    /// products.
    Org {
        #[command(subcommand)]
        cmd: org::OrgCommands,
    },
}

pub async fn dispatch(cmd: AdminCommands) -> Result<()> {
    match cmd {
        AdminCommands::User { cmd } => user::dispatch(cmd).await,
        AdminCommands::Org { cmd } => org::dispatch(cmd).await,
    }
}

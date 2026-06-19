//! `th admin` — Smoo AI platform-administration operations.
//!
//! Two kinds of operations live here:
//!
//! 1. **Super-admin** verbs that hit `/admin/*` on api.smoo.ai (gated
//!    server-side by `requireSuperAdmin`). `user`, `org` subtrees fall
//!    in this bucket. Without the role, every call returns 403 and the
//!    client prints "your user lacks the requireSuperAdmin role".
//!
//! 2. **Per-org platform-admin** verbs that hit `/organizations/{id}/…`
//!    but represent infrequent admin work (creating schemas, deleting
//!    environments, bulk value reconciliation). `config` falls in this
//!    bucket — pearl `th-9c0c34`. Auth is the same normal user/M2M
//!    auth as `th config`, but the naming captures "this is the rare
//!    admin path, not the daily path".
//!
//! Cardinal use case: stand up a new customer org end-to-end. Today's
//! surface covers org + user CRUD + product activation + config
//! schema/env management. The remaining four steps
//! (parent-relationships, managed-websites, b2m-key mint, m2m-key
//! mint) need backend endpoints that don't exist yet — pearl
//! `th-feebd2` + the four smooai-side backend pearls.

use anyhow::Result;
use clap::Subcommand;

pub mod client;
pub mod config;
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
    /// Organization operations: list / show / create / members / products /
    /// mint-client / scc-tier.
    Org {
        #[command(subcommand)]
        cmd: org::OrgCommands,
    },
    /// Platform-admin config operations: schemas + environments CRUD,
    /// bulk-set values, delete value records. Single-key reads/writes
    /// belong under `th config`; this is the infrequent admin
    /// surface. Pearl `th-9c0c34`.
    Config {
        #[command(subcommand)]
        cmd: config::ConfigCommands,
    },
}

pub async fn dispatch(cmd: AdminCommands) -> Result<()> {
    match cmd {
        AdminCommands::User { cmd } => user::dispatch(cmd).await,
        AdminCommands::Org { cmd } => org::dispatch(cmd).await,
        AdminCommands::Config { cmd } => config::dispatch(cmd).await,
    }
}

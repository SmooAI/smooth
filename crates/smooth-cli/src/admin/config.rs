//! `th admin config` — platform-administration config surface.
//!
//! Pearl `th-9c0c34`. Where `th config` (`crates/smooth-cli/src/config.rs`)
//! is the daily-developer ergonomic shortcut (`get` / `set` / `list` /
//! `push` / `pull` / `diff` / `init` / `feature-flag`), `th admin config`
//! holds the infrequent platform-admin verbs:
//!
//! - **Schemas CRUD** — create / update / delete schema entities, plus
//!   `push` (server-side version bump) and `values` (per-schema, per-env
//!   value dump). Manage the schemas themselves, not the values inside
//!   them.
//! - **Environments CRUD** — create / update / delete environments,
//!   plus per-env value dumps. Onboarding a new env or retiring a stale
//!   one happens here, not under `th config`.
//! - **Bulk values** — `bulk-set` for atomic multi-key writes (CI
//!   reconciliation jobs); `delete <id>` to remove an entire value
//!   record (not just clear it).
//!
//! Endpoints all live at `/organizations/{org_id}/config/*` — same
//! routes the old `th api config` hit. The naming captures "platform
//! admin" by convention, not by `requireSuperAdmin` gate (that's what
//! `/admin/*` is for). Org-scoped admin operations belong here so
//! `th config` stays narrow.
//!
//! Auth: uses the same `require_authed` + `require_active_org` shared
//! helpers as the smooai resource modules — user JWT preferred, M2M
//! via the platform login flow if the user wasn't `th auth login`'d.

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::smooai::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Debug, Subcommand)]
pub enum ConfigCommands {
    /// Schemas — the document that declares which keys exist + their
    /// tiers (public / secret / feature_flag).
    Schemas {
        #[command(subcommand)]
        cmd: SchemasCmd,
    },
    /// Environments — `development` / `staging` / `production` etc.
    /// Each value is set per (schema, environment) pair.
    Environments {
        #[command(subcommand)]
        cmd: EnvironmentsCmd,
    },
    /// Bulk value operations + record deletion. Single-key writes
    /// belong in `th config set`; this is the "manage values as
    /// records" surface.
    Values {
        #[command(subcommand)]
        cmd: ValuesCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum SchemasCmd {
    /// List all schemas for the active org.
    List {
        #[arg(long)]
        org: Option<String>,
    },
    /// Show a schema by id.
    Show {
        schema_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Create a schema. Body is a JSON document or `-` for stdin.
    Create {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Update a schema. Body is a JSON patch document.
    Update {
        schema_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Delete a schema. Irreversible.
    Delete {
        schema_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Push a new schema version. Body is the full new schema doc.
    Push {
        schema_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Dump every value under a (schema, environment) pair.
    Values {
        schema_id: String,
        env_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum EnvironmentsCmd {
    /// List all environments for the active org.
    List {
        #[arg(long)]
        org: Option<String>,
    },
    /// Create an environment. Body is a JSON document or `-` for stdin.
    Create {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Update an environment. Body is a JSON patch.
    Update {
        env_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Delete an environment. Irreversible — every value under it goes
    /// with it.
    Delete {
        env_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Dump every value in this environment, across all schemas.
    Values {
        env_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ValuesCmd {
    /// Bulk-set multiple values atomically. Body is a JSON document
    /// describing the writes (or `-` for stdin).
    BulkSet {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Delete a value record by id. Removes the row entirely — for
    /// "clear the key for one env" use `th config set` with a null
    /// value through the API.
    Delete {
        value_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

/// Dispatch `th admin config <sub>`.
///
/// # Errors
/// Bubbles auth + active-org resolution failures and non-2xx API
/// responses up.
pub async fn dispatch(cmd: ConfigCommands) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        ConfigCommands::Schemas { cmd } => dispatch_schemas(cmd, &client).await,
        ConfigCommands::Environments { cmd } => dispatch_environments(cmd, &client).await,
        ConfigCommands::Values { cmd } => dispatch_values(cmd, &client).await,
    }
}

async fn dispatch_schemas(cmd: SchemasCmd, client: &smooth_api_client::SmoothApiClient) -> Result<()> {
    match cmd {
        SchemasCmd::List { org } => {
            let o = require_active_org(client, org)?;
            print_list_envelope(
                &client.get(&format!("/organizations/{o}/config/schemas")).await.context("GET schemas")?,
                "schemas",
            );
        }
        SchemasCmd::Show { schema_id, org } => {
            let o = require_active_org(client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/config/schemas/{schema_id}"))
                    .await
                    .context("GET schema")?,
            );
        }
        SchemasCmd::Create { body, org } => {
            let o = require_active_org(client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/config/schemas"), Some(&b))
                    .await
                    .context("POST schema")?,
            );
        }
        SchemasCmd::Update { schema_id, body, org } => {
            let o = require_active_org(client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .patch(&format!("/organizations/{o}/config/schemas/{schema_id}"), &b)
                    .await
                    .context("PATCH schema")?,
            );
        }
        SchemasCmd::Delete { schema_id, org } => {
            let o = require_active_org(client, org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{o}/config/schemas/{schema_id}"))
                    .await
                    .context("DELETE schema")?,
            );
        }
        SchemasCmd::Push { schema_id, body, org } => {
            let o = require_active_org(client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/config/schemas/{schema_id}/push"), Some(&b))
                    .await
                    .context("POST schema push")?,
            );
        }
        SchemasCmd::Values { schema_id, env_id, org } => {
            let o = require_active_org(client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/config/schemas/{schema_id}/environments/{env_id}/values"))
                    .await
                    .context("GET schema/env values")?,
            );
        }
    }
    Ok(())
}

async fn dispatch_environments(cmd: EnvironmentsCmd, client: &smooth_api_client::SmoothApiClient) -> Result<()> {
    match cmd {
        EnvironmentsCmd::List { org } => {
            let o = require_active_org(client, org)?;
            print_list_envelope(
                &client
                    .get(&format!("/organizations/{o}/config/environments"))
                    .await
                    .context("GET environments")?,
                "environments",
            );
        }
        EnvironmentsCmd::Create { body, org } => {
            let o = require_active_org(client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/config/environments"), Some(&b))
                    .await
                    .context("POST environment")?,
            );
        }
        EnvironmentsCmd::Update { env_id, body, org } => {
            let o = require_active_org(client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .patch(&format!("/organizations/{o}/config/environments/{env_id}"), &b)
                    .await
                    .context("PATCH environment")?,
            );
        }
        EnvironmentsCmd::Delete { env_id, org } => {
            let o = require_active_org(client, org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{o}/config/environments/{env_id}"))
                    .await
                    .context("DELETE environment")?,
            );
        }
        EnvironmentsCmd::Values { env_id, org } => {
            let o = require_active_org(client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/config/environments/{env_id}/values"))
                    .await
                    .context("GET env values")?,
            );
        }
    }
    Ok(())
}

async fn dispatch_values(cmd: ValuesCmd, client: &smooth_api_client::SmoothApiClient) -> Result<()> {
    match cmd {
        ValuesCmd::BulkSet { body, org } => {
            let o = require_active_org(client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .put(&format!("/organizations/{o}/config/values/bulk"), &b)
                    .await
                    .context("PUT values bulk")?,
            );
        }
        ValuesCmd::Delete { value_id, org } => {
            let o = require_active_org(client, org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{o}/config/values/{value_id}"))
                    .await
                    .context("DELETE value")?,
            );
        }
    }
    Ok(())
}

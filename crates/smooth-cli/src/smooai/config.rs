//! `th config …` — Smoo AI platform configuration. Schemas,
//! environments, values, and feature-flag evaluation.

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    Schemas {
        #[command(subcommand)]
        cmd: SchemasCmd,
    },
    Environments {
        #[command(subcommand)]
        cmd: EnvironmentsCmd,
    },
    Values {
        #[command(subcommand)]
        cmd: ValuesCmd,
    },
    /// Evaluate a feature flag for the active org. Optional JSON
    /// context via `--context <path|->`.
    FeatureFlag {
        key: String,
        #[arg(long)]
        context: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum SchemasCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Show {
        schema_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Create {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        schema_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        schema_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Push {
        schema_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Per-schema, per-env values.
    Values {
        schema_id: String,
        env_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum EnvironmentsCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Create {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        env_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        env_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Values {
        env_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ValuesCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Show {
        value_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Set {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    BulkSet {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        value_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Schemas { cmd } => match cmd {
            SchemasCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(
                    &client.get(&format!("/organizations/{o}/config/schemas")).await.context("GET schemas")?,
                    "schemas",
                );
            }
            SchemasCmd::Show { schema_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .get(&format!("/organizations/{o}/config/schemas/{schema_id}"))
                        .await
                        .context("GET schema")?,
                );
            }
            SchemasCmd::Create { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = read_body(&body)?;
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/config/schemas"), Some(&b))
                        .await
                        .context("POST schema")?,
                );
            }
            SchemasCmd::Update { schema_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = read_body(&body)?;
                print_json(
                    &client
                        .patch(&format!("/organizations/{o}/config/schemas/{schema_id}"), &b)
                        .await
                        .context("PATCH schema")?,
                );
            }
            SchemasCmd::Delete { schema_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/config/schemas/{schema_id}"))
                        .await
                        .context("DELETE schema")?,
                );
            }
            SchemasCmd::Push { schema_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = read_body(&body)?;
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/config/schemas/{schema_id}/push"), Some(&b))
                        .await
                        .context("POST schema push")?,
                );
            }
            SchemasCmd::Values { schema_id, env_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .get(&format!("/organizations/{o}/config/schemas/{schema_id}/environments/{env_id}/values"))
                        .await
                        .context("GET schema/env values")?,
                );
            }
        },
        Cmd::Environments { cmd } => match cmd {
            EnvironmentsCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(
                    &client
                        .get(&format!("/organizations/{o}/config/environments"))
                        .await
                        .context("GET environments")?,
                    "environments",
                );
            }
            EnvironmentsCmd::Create { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = read_body(&body)?;
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/config/environments"), Some(&b))
                        .await
                        .context("POST environment")?,
                );
            }
            EnvironmentsCmd::Update { env_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = read_body(&body)?;
                print_json(
                    &client
                        .patch(&format!("/organizations/{o}/config/environments/{env_id}"), &b)
                        .await
                        .context("PATCH environment")?,
                );
            }
            EnvironmentsCmd::Delete { env_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/config/environments/{env_id}"))
                        .await
                        .context("DELETE environment")?,
                );
            }
            EnvironmentsCmd::Values { env_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .get(&format!("/organizations/{o}/config/environments/{env_id}/values"))
                        .await
                        .context("GET env values")?,
                );
            }
        },
        Cmd::Values { cmd } => match cmd {
            ValuesCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(&client.get(&format!("/organizations/{o}/config/values")).await.context("GET values")?, "values");
            }
            ValuesCmd::Show { value_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(&client.get(&format!("/organizations/{o}/config/values/{value_id}")).await.context("GET value")?);
            }
            ValuesCmd::Set { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = read_body(&body)?;
                print_json(&client.put(&format!("/organizations/{o}/config/values"), &b).await.context("PUT value")?);
            }
            ValuesCmd::BulkSet { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = read_body(&body)?;
                print_json(
                    &client
                        .put(&format!("/organizations/{o}/config/values/bulk"), &b)
                        .await
                        .context("PUT values bulk")?,
                );
            }
            ValuesCmd::Delete { value_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/config/values/{value_id}"))
                        .await
                        .context("DELETE value")?,
                );
            }
        },
        Cmd::FeatureFlag { key, context, org } => {
            let o = require_active_org(&client, org)?;
            let body = match context {
                Some(p) => read_body(&p)?,
                None => serde_json::json!({}),
            };
            print_json(
                &client
                    .post(&format!("/organizations/{o}/config/feature-flags/{key}/evaluate"), Some(&body))
                    .await
                    .context("POST evaluate flag")?,
            );
        }
    }
    Ok(())
}

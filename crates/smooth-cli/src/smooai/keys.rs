//! `th keys …` — M2M auth clients ("API keys").

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    /// Create an API key (JSON body). Response includes the secret
    /// exactly once — store it immediately.
    Create {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        client_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Revoke {
        client_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List { org } => {
            let org = require_active_org(&client, org)?;
            print_list_envelope(&client.get(&format!("/organizations/{org}/auth-clients")).await.context("GET auth-clients")?, "API keys");
        }
        Cmd::Create { body, org } => {
            let org = require_active_org(&client, org)?;
            let body = read_body(&body)?;
            print_json(&client.post(&format!("/organizations/{org}/auth-clients"), Some(&body)).await.context("POST auth-client")?);
        }
        Cmd::Update { client_id, body, org } => {
            let org = require_active_org(&client, org)?;
            let body = read_body(&body)?;
            print_json(&client.patch(&format!("/organizations/{org}/auth-clients/{client_id}"), &body).await.context("PATCH auth-client")?);
        }
        Cmd::Revoke { client_id, org } => {
            let org = require_active_org(&client, org)?;
            print_json(&client.delete(&format!("/organizations/{org}/auth-clients/{client_id}")).await.context("DELETE auth-client")?);
        }
    }
    Ok(())
}

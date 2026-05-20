//! `th products …` — billing products / plans.

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    /// Activate the free tier.
    Free {
        #[arg(long)]
        org: Option<String>,
    },
    /// Activate a bypass — admin only. Optional JSON body.
    Bypass {
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List { org } => {
            let o = require_active_org(&client, org)?;
            print_list_envelope(&client.get(&format!("/organizations/{o}/products")).await.context("GET products")?, "products");
        }
        Cmd::Free { org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/products/free"), None)
                    .await
                    .context("POST products free")?,
            );
        }
        Cmd::Bypass { body, org } => {
            let o = require_active_org(&client, org)?;
            let b = match body {
                Some(p) => Some(read_body(&p)?),
                None => None,
            };
            print_json(
                &client
                    .post(&format!("/organizations/{o}/products/bypass"), b.as_ref())
                    .await
                    .context("POST products bypass")?,
            );
        }
    }
    Ok(())
}

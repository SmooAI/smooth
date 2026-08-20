//! `th products …` — billing products / plans.

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// List the billing products / plans available to the org.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the list.
        #[arg(long)]
        json: bool,
    },
    /// Activate the free tier.
    Free {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Activate a bypass — admin only. Optional JSON body.
    Bypass {
        /// Optional JSON body (file path, or `-` for stdin) with bypass details.
        #[arg(long)]
        body: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List { org, json } => {
            let o = require_active_org(&client, org)?;
            let body = client.get(&format!("/organizations/{o}/products")).await.context("GET products")?;
            if json {
                print_json(&body);
            } else {
                print_list_envelope(&body, "products");
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// CLI-Spec §flags: every platform `list` verb offers `--json`.
    #[test]
    fn list_accepts_json_flag_and_defaults_to_off() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(subcommand)]
            cmd: Cmd,
        }
        let on = Wrap::try_parse_from(["t", "list", "--json"]).expect("--json must parse");
        assert!(matches!(on.cmd, Cmd::List { json: true, .. }));

        let off = Wrap::try_parse_from(["t", "list"]).expect("bare list must still parse");
        assert!(matches!(off.cmd, Cmd::List { json: false, .. }), "--json must default to off");
    }
}

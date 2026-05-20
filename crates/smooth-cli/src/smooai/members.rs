//! `th members …` — org members + invitations.

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Roles {
        #[arg(long)]
        org: Option<String>,
    },
    Invitations {
        #[arg(long)]
        org: Option<String>,
    },
    /// Invite a user (JSON body — typically `{"email": "...", "role": "..."}`).
    Invite {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Revoke {
        invitation_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Resend {
        invitation_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Accept {
        invitation_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Reject {
        invitation_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List { org } => {
            let org = require_active_org(&client, org)?;
            print_list_envelope(&client.get(&format!("/organizations/{org}/members")).await.context("GET members")?, "members");
        }
        Cmd::Roles { org } => {
            let org = require_active_org(&client, org)?;
            print_json(&client.get(&format!("/organizations/{org}/roles")).await.context("GET roles")?);
        }
        Cmd::Invitations { org } => {
            let org = require_active_org(&client, org)?;
            print_list_envelope(
                &client.get(&format!("/organizations/{org}/member-invitations")).await.context("GET invitations")?,
                "invitations",
            );
        }
        Cmd::Invite { body, org } => {
            let org = require_active_org(&client, org)?;
            let body = read_body(&body)?;
            print_json(&client.post(&format!("/organizations/{org}/member-invitations"), Some(&body)).await.context("POST invitation")?);
        }
        Cmd::Revoke { invitation_id, org } => {
            let org = require_active_org(&client, org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{org}/member-invitations/{invitation_id}"))
                    .await
                    .context("DELETE invitation")?,
            );
        }
        Cmd::Resend { invitation_id, org } => {
            let org = require_active_org(&client, org)?;
            print_json(
                &client
                    .post(&format!("/organizations/{org}/member-invitations/{invitation_id}/resend"), None)
                    .await
                    .context("POST resend")?,
            );
        }
        Cmd::Accept { invitation_id, org } => {
            let org = require_active_org(&client, org)?;
            print_json(
                &client
                    .post(&format!("/organizations/{org}/member-invitations/{invitation_id}/accept"), None)
                    .await
                    .context("POST accept")?,
            );
        }
        Cmd::Reject { invitation_id, org } => {
            let org = require_active_org(&client, org)?;
            print_json(
                &client
                    .post(&format!("/organizations/{org}/member-invitations/{invitation_id}/reject"), None)
                    .await
                    .context("POST reject")?,
            );
        }
    }
    Ok(())
}

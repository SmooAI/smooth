//! `th members …` — org members + invitations.

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// List the members of the active (or `--org-id`) organization.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// List the roles a member can be assigned.
    Roles {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// List pending member invitations.
    Invitations {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Invite a user (JSON body — typically `{"email": "...", "role": "..."}`).
    Invite {
        /// JSON invitation body, or `-` to read from stdin.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Revoke (delete) a pending invitation.
    Revoke {
        /// The invitation id from `th api members invitations`.
        invitation_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Resend the email for a pending invitation.
    Resend {
        /// The invitation id from `th api members invitations`.
        invitation_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Accept an invitation on the invitee's behalf.
    Accept {
        /// The invitation id from `th api members invitations`.
        invitation_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Reject an invitation on the invitee's behalf.
    Reject {
        /// The invitation id from `th api members invitations`.
        invitation_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
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
                &client
                    .get(&format!("/organizations/{org}/member-invitations"))
                    .await
                    .context("GET invitations")?,
                "invitations",
            );
        }
        Cmd::Invite { body, org } => {
            let org = require_active_org(&client, org)?;
            let body = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{org}/member-invitations"), Some(&body))
                    .await
                    .context("POST invitation")?,
            );
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

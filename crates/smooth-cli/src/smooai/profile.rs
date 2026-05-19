//! `th profile …` — current user profile + the user's pending
//! org-member invitations.

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, read_body, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// Show the logged-in user's profile.
    Show,
    Update {
        body: String,
    },
    /// List pending org-member invitations addressed to this user.
    Invitations,
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed()?;
    match cmd {
        Cmd::Show => print_json(&client.get("/profile").await.context("GET profile")?),
        Cmd::Update { body } => {
            let b = read_body(&body)?;
            print_json(&client.patch("/profile", &b).await.context("PATCH profile")?);
        }
        Cmd::Invitations => print_json(&client.get("/profile/organization-member-invitations").await.context("GET profile invitations")?),
    }
    Ok(())
}

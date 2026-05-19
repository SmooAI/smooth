//! `th knowledge …` — knowledge documents (text, websites, files).

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Show {
        doc_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Content {
        doc_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Upload a text knowledge document (JSON body — file uploads
    /// use a separate multipart endpoint the CLI doesn't wrap yet).
    Upload {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Register a website as a knowledge source.
    Website {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Process {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        doc_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    UpdateContent {
        doc_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        doc_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed()?;
    match cmd {
        Cmd::List { org } => {
            let o = require_active_org(&client, org)?;
            print_list_envelope(&client.get(&format!("/organizations/{o}/knowledge")).await.context("GET knowledge")?, "knowledge docs");
        }
        Cmd::Show { doc_id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(&client.get(&format!("/organizations/{o}/knowledge/{doc_id}")).await.context("GET knowledge doc")?);
        }
        Cmd::Content { doc_id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(&client.get(&format!("/organizations/{o}/knowledge/{doc_id}/content")).await.context("GET knowledge content")?);
        }
        Cmd::Upload { body, org } => {
            let o = require_active_org(&client, org)?;
            let b = read_body(&body)?;
            print_json(&client.post(&format!("/organizations/{o}/knowledge/upload"), Some(&b)).await.context("POST knowledge upload")?);
        }
        Cmd::Website { body, org } => {
            let o = require_active_org(&client, org)?;
            let b = read_body(&body)?;
            print_json(&client.post(&format!("/organizations/{o}/knowledge/websites"), Some(&b)).await.context("POST knowledge website")?);
        }
        Cmd::Process { body, org } => {
            let o = require_active_org(&client, org)?;
            let b = read_body(&body)?;
            print_json(&client.post(&format!("/organizations/{o}/knowledge/process"), Some(&b)).await.context("POST knowledge process")?);
        }
        Cmd::Update { doc_id, body, org } => {
            let o = require_active_org(&client, org)?;
            let b = read_body(&body)?;
            print_json(&client.patch(&format!("/organizations/{o}/knowledge/{doc_id}"), &b).await.context("PATCH knowledge doc")?);
        }
        Cmd::UpdateContent { doc_id, body, org } => {
            let o = require_active_org(&client, org)?;
            let b = read_body(&body)?;
            print_json(&client.patch(&format!("/organizations/{o}/knowledge/{doc_id}/content"), &b).await.context("PATCH knowledge content")?);
        }
        Cmd::Delete { doc_id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(&client.delete(&format!("/organizations/{o}/knowledge/{doc_id}")).await.context("DELETE knowledge doc")?);
        }
    }
    Ok(())
}

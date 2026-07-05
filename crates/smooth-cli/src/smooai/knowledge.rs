//! `th knowledge …` — knowledge documents (text, websites, files).

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// List the knowledge documents in the active (or `--org-id`) organization.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show one knowledge document's metadata.
    Show {
        /// The document id from `th api knowledge list`.
        doc_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Fetch the document's stored content.
    Content {
        /// The document id from `th api knowledge list`.
        doc_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Upload a text knowledge document (JSON body — file uploads
    /// use a separate multipart endpoint the CLI doesn't wrap yet).
    Upload {
        /// JSON document body, or `-` to read from stdin.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Register a website as a knowledge source.
    Website {
        /// JSON body describing the website to crawl, or `-` for stdin.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Kick off (re)processing of a knowledge source (JSON body).
    Process {
        /// JSON processing request body, or `-` to read from stdin.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Patch a knowledge document's metadata (JSON body).
    Update {
        /// The document id from `th api knowledge list`.
        doc_id: String,
        /// JSON patch body, or `-` to read from stdin.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Replace a knowledge document's content (JSON body).
    UpdateContent {
        /// The document id from `th api knowledge list`.
        doc_id: String,
        /// JSON content body, or `-` to read from stdin.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a knowledge document permanently.
    Delete {
        /// The document id from `th api knowledge list`.
        doc_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List { org } => {
            let o = require_active_org(&client, org)?;
            print_list_envelope(
                &client.get(&format!("/organizations/{o}/knowledge")).await.context("GET knowledge")?,
                "knowledge docs",
            );
        }
        Cmd::Show { doc_id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/knowledge/{doc_id}"))
                    .await
                    .context("GET knowledge doc")?,
            );
        }
        Cmd::Content { doc_id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/knowledge/{doc_id}/content"))
                    .await
                    .context("GET knowledge content")?,
            );
        }
        Cmd::Upload { body, org } => {
            let o = require_active_org(&client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/knowledge/upload"), Some(&b))
                    .await
                    .context("POST knowledge upload")?,
            );
        }
        Cmd::Website { body, org } => {
            let o = require_active_org(&client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/knowledge/websites"), Some(&b))
                    .await
                    .context("POST knowledge website")?,
            );
        }
        Cmd::Process { body, org } => {
            let o = require_active_org(&client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/knowledge/process"), Some(&b))
                    .await
                    .context("POST knowledge process")?,
            );
        }
        Cmd::Update { doc_id, body, org } => {
            let o = require_active_org(&client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .patch(&format!("/organizations/{o}/knowledge/{doc_id}"), &b)
                    .await
                    .context("PATCH knowledge doc")?,
            );
        }
        Cmd::UpdateContent { doc_id, body, org } => {
            let o = require_active_org(&client, org)?;
            let b = read_body(&body)?;
            print_json(
                &client
                    .patch(&format!("/organizations/{o}/knowledge/{doc_id}/content"), &b)
                    .await
                    .context("PATCH knowledge content")?,
            );
        }
        Cmd::Delete { doc_id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{o}/knowledge/{doc_id}"))
                    .await
                    .context("DELETE knowledge doc")?,
            );
        }
    }
    Ok(())
}

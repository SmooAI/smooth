//! `th knowledge …` — knowledge documents (text, websites, files).

use anyhow::{Context, Result};
use clap::Subcommand;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// Semantic search over the org's OWN knowledge base — the same retrieval an
    /// agent runs. Returns the most relevant passages (name + content + score).
    /// Give it a question or a few keywords; scope to one document with `--doc`.
    Search {
        /// What to look for in the org's knowledge (a question or keywords).
        query: String,
        /// Max passages to return (1-20).
        #[arg(long, value_name = "N")]
        max: Option<u64>,
        /// Scope the search to a single knowledge document (id from `th knowledge list`).
        #[arg(long = "doc", visible_alias = "document-id", value_name = "DOC_ID")]
        doc: Option<String>,
        /// Print the full JSON response instead of the compact passage list.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// List the knowledge documents in the active (or `--org-id`) organization.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the list.
        #[arg(long)]
        json: bool,
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
    /// Crawl a website into the org's knowledge base (async ingestion job).
    AddUrl {
        /// The website URL to crawl and ingest.
        url: String,
        /// Display name for the knowledge source (defaults to the URL).
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
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
    /// Delete a knowledge document permanently. Prints the target (org +
    /// host) and requires typing the document id back; refuses when not
    /// attached to a terminal.
    Delete {
        /// The document id from `th api knowledge list`.
        doc_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the target and exit without deleting.
        #[arg(long)]
        dry_run: bool,
        /// Skip the interactive confirmation. Required in scripts/CI.
        #[arg(long)]
        yes: bool,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Search {
            query,
            max,
            doc,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let mut body = serde_json::json!({ "query": query });
            if let Some(m) = max {
                body["maxResults"] = serde_json::json!(m);
            }
            if let Some(d) = doc {
                body["documentId"] = serde_json::json!(d);
            }
            let resp = client
                .post(&format!("/organizations/{o}/knowledge/search"), Some(&body))
                .await
                .context("POST knowledge search")?;
            if as_json {
                print_json(&resp);
            } else {
                print_knowledge_results(&resp);
            }
        }
        Cmd::List { org, json } => {
            let o = require_active_org(&client, org)?;
            let body = client.get(&format!("/organizations/{o}/knowledge")).await.context("GET knowledge")?;
            if json {
                print_json(&body);
            } else {
                print_list_envelope(&body, "knowledge docs");
            }
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
        Cmd::AddUrl { url, name, org } => {
            let o = require_active_org(&client, org)?;
            let name = name.unwrap_or_else(|| url.clone());
            let body = serde_json::json!({ "urls": [url], "name": name });
            print_json(
                &client
                    .post(&format!("/organizations/{o}/knowledge/websites"), Some(&body))
                    .await
                    .context("POST knowledge add-url")?,
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
        Cmd::Delete { doc_id, org, dry_run, yes } => {
            let o = require_active_org(&client, org)?;
            let proceed = crate::destructive::gate(
                &crate::destructive::Target {
                    verb: "delete",
                    noun: "knowledge document",
                    id: &doc_id,
                    org: &o,
                    severity: crate::destructive::Severity::Irreversible,
                },
                dry_run,
                yes,
            )?;
            if proceed {
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/knowledge/{doc_id}"))
                        .await
                        .context("DELETE knowledge doc")?,
                );
            }
        }
    }
    Ok(())
}

/// Compact rendering of `POST /knowledge/search`: a numbered list of
/// `name` with the passage below each, most-relevant first. Falls back to JSON on
/// an unexpected shape.
fn print_knowledge_results(resp: &serde_json::Value) {
    match resp.get("results").and_then(|r| r.as_array()) {
        Some(results) if !results.is_empty() => {
            for (i, r) in results.iter().enumerate() {
                let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("(untitled)");
                println!("{}. {name}", i + 1);
                if let Some(content) = r.get("content").and_then(|v| v.as_str()) {
                    // Indent the passage under its heading; keep it whole (unlike
                    // web search, knowledge passages are the answer, not a teaser).
                    for line in content.lines() {
                        println!("   {line}");
                    }
                }
                println!();
            }
        }
        Some(_) => println!("No matching knowledge found for that query."),
        None => print_json(resp),
    }
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

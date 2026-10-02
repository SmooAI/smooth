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
    /// Write a NEW markdown document into the knowledge base: stored as a file
    /// in the Files tree and indexed for search. Mirrors the Smooth Operator's
    /// `knowledge.create` and the hosted MCP's `knowledge_create`.
    Create {
        /// Document name, e.g. "Refund Policy".
        #[arg(long)]
        name: String,
        /// Markdown file to read the content from, or `-` for stdin.
        #[arg(long = "file", short = 'f', value_name = "PATH")]
        file: String,
        /// File it into this folder (id from `th smoo files ls`). Root when omitted.
        #[arg(long = "folder-id", value_name = "FOLDER_ID")]
        folder_id: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Replace a text document's content with new markdown, by FILE id (from
    /// `th smoo files ls` / `search`). Re-indexes it if it is in the knowledge
    /// base. Mirrors `knowledge.update` / the MCP's `knowledge_update`.
    Edit {
        /// The file id (Files tree), not a knowledge document id.
        file_id: String,
        /// Markdown file to read the new content from, or `-` for stdin.
        #[arg(long = "file", short = 'f', value_name = "PATH")]
        file: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Ingest a website into the org's knowledge base (async ingestion job).
    /// Crawls the site from the URL by default; `--page` ingests just that page.
    AddUrl {
        /// The website URL to crawl and ingest.
        url: String,
        /// Display name for the knowledge source (defaults to the URL).
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Ingest only this one page instead of crawling the site.
        #[arg(long)]
        page: bool,
        /// Crawl only: max pages to fetch (1-500).
        #[arg(long = "max-pages", value_name = "N", conflicts_with = "page")]
        max_pages: Option<u32>,
        /// Crawl only: max link depth from the URL (0-10).
        #[arg(long = "max-depth", value_name = "N", conflicts_with = "page")]
        max_depth: Option<u32>,
        /// With `--page`: make the document private to you instead of shared with the org.
        #[arg(long, requires = "page")]
        personal: bool,
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
        Cmd::Create { name, file, folder_id, org } => {
            let o = require_active_org(&client, org)?;
            let body = create_body(&name, &read_markdown(&file)?, folder_id.as_deref())?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/files/documents"), Some(&body))
                    .await
                    .context("POST files/documents (needs knowledge.write)")?,
            );
        }
        Cmd::Edit { file_id, file, org } => {
            let o = require_active_org(&client, org)?;
            let body = serde_json::json!({ "content": nonempty(read_markdown(&file)?)? });
            print_json(
                &client
                    .put(&format!("/organizations/{o}/files/{file_id}/content"), &body)
                    .await
                    .context("PUT files content (needs knowledge.write)")?,
            );
        }
        Cmd::AddUrl {
            url,
            name,
            page,
            max_pages,
            max_depth,
            personal,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let (path, body) = add_url_request(&url, name.as_deref(), page, max_pages, max_depth, personal);
            print_json(
                &client
                    .post(&format!("/organizations/{o}/knowledge/{path}"), Some(&body))
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

/// Markdown content from a file path, or stdin for `-`.
fn read_markdown(path: &str) -> Result<String> {
    if path == "-" {
        use std::io::Read as _;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("read stdin")?;
        Ok(s)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {path}"))
    }
}

fn nonempty(content: String) -> Result<String> {
    if content.trim().is_empty() {
        anyhow::bail!("the document content is empty");
    }
    Ok(content)
}

/// `POST /files/documents` body.
fn create_body(name: &str, content: &str, folder_id: Option<&str>) -> Result<serde_json::Value> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 512 {
        anyhow::bail!("--name must be 1-512 characters");
    }
    let mut body = serde_json::json!({ "name": name, "content": nonempty(content.to_string())? });
    if let Some(f) = folder_id.map(str::trim).filter(|f| !f.is_empty()) {
        body["folderId"] = serde_json::json!(f);
    }
    Ok(body)
}

/// The route + body for `add-url`: a single page goes to `/knowledge/process`
/// (`websiteUrl`), a crawl to `/knowledge/websites` (`urls`).
fn add_url_request(
    url: &str,
    name: Option<&str>,
    page: bool,
    max_pages: Option<u32>,
    max_depth: Option<u32>,
    personal: bool,
) -> (&'static str, serde_json::Value) {
    let name = name.map_or_else(|| url.to_string(), str::to_string);
    if page {
        return ("process", serde_json::json!({ "name": name, "websiteUrl": url, "personal": personal }));
    }
    let mut body = serde_json::json!({ "urls": [url], "name": name });
    if let Some(n) = max_pages {
        body["crawlLimit"] = serde_json::json!(n.clamp(1, 500));
    }
    if let Some(d) = max_depth {
        body["crawlMaxDepth"] = serde_json::json!(d.min(10));
    }
    ("websites", body)
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

    #[test]
    fn add_url_crawls_by_default_and_ingests_one_page_with_page() {
        let (path, body) = add_url_request("https://smoo.ai", None, false, Some(9999), Some(3), false);
        assert_eq!(path, "websites");
        assert_eq!(body["urls"], serde_json::json!(["https://smoo.ai"]));
        assert_eq!(body["crawlLimit"], serde_json::json!(500));
        assert_eq!(body["crawlMaxDepth"], serde_json::json!(3));
        let (path, body) = add_url_request("https://smoo.ai/p", Some("Pricing"), true, None, None, true);
        assert_eq!(path, "process");
        assert_eq!(body["websiteUrl"], serde_json::json!("https://smoo.ai/p"));
        assert_eq!(body["personal"], serde_json::json!(true));
        assert_eq!(body["name"], serde_json::json!("Pricing"));
    }

    #[test]
    fn create_body_validates_and_carries_the_folder() {
        let b = create_body(" Refunds ", "# R", Some("fold-1")).unwrap();
        assert_eq!(b["name"], serde_json::json!("Refunds"));
        assert_eq!(b["folderId"], serde_json::json!("fold-1"));
        assert!(create_body("", "# R", None).is_err());
        assert!(create_body("x", "  ", None).is_err());
        assert!(create_body("x", "y", None).unwrap().get("folderId").is_none());
    }

    #[test]
    fn personal_requires_page() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrap {
            #[command(subcommand)]
            cmd: Cmd,
        }
        assert!(Wrap::try_parse_from(["t", "add-url", "https://smoo.ai", "--personal"]).is_err());
        assert!(Wrap::try_parse_from(["t", "add-url", "https://smoo.ai", "--page", "--personal"]).is_ok());
        assert!(Wrap::try_parse_from(["t", "create", "--name", "x", "--file", "-"]).is_ok());
        assert!(Wrap::try_parse_from(["t", "edit", "f-1", "--file", "doc.md"]).is_ok());
    }

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

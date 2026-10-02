//! `th smoo blog …` — the CLI twin of the hosted MCP's `blog_*` tools
//! (SMOODEV-3537).
//!
//! A HeyPage blog post is its own record, not a site page: drafts are free,
//! nothing is public until `publish`, and the live page keeps serving the
//! revision that was published until you publish again. Every verb here is a
//! thin wrapper over the same `api.smoo.ai` route the MCP tool calls
//! (`/organizations/{org}/heypage/blog/…`), with the MCP's client-side guards
//! mirrored so a bad request fails here with a sentence instead of a schema
//! error from upstream.
//!
//! `image generate` COSTS money (a paid image model). It is a single POST —
//! the api client never retries a non-401 failure — so a timeout means
//! "unknown", not "failed": check `blog image search`/the post before
//! re-running, or you pay twice.

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};

use super::{print_json, read_body, require_active_org, require_authed};
use crate::destructive::{Confirm, Severity, Target};

/// Statuses the list route filters on.
const POST_STATUSES: &[&str] = &["draft", "published", "archived"];

#[derive(Subcommand)]
pub enum Cmd {
    /// List the website's blog posts, newest first, drafts included.
    List {
        /// Only posts with this status: `draft`, `published` or `archived`.
        #[arg(long)]
        status: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the list.
        #[arg(long)]
        json: bool,
    },
    /// Read one post: its current DRAFT body, metadata and revision history.
    Get {
        /// Post id (from `blog list`).
        post_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Write a new post as a DRAFT. Nothing is public until `blog publish`.
    ///
    /// Body: exactly one of `--markdown <file|->` or `--blocks <json-file|->`.
    /// Images (body + `--hero-image`) must be images.unsplash.com or
    /// Smoo-hosted URLs (see `blog image import`), each with alt text.
    Create {
        /// The post's headline.
        #[arg(long)]
        title: String,
        /// URL segment, e.g. `preparing-for-an-iep-meeting`. Permanent once published.
        #[arg(long)]
        slug: String,
        #[command(flatten)]
        body: BodyArgs,
        #[command(flatten)]
        meta: MetaArgs,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Revise a post's DRAFT. The live page is unchanged until `blog publish`.
    ///
    /// `--markdown`/`--blocks` REPLACE the whole body — send the complete post.
    /// Metadata is merged server-side, so setting one field keeps the others.
    Update {
        /// Post id (from `blog list`).
        post_id: String,
        /// New headline.
        #[arg(long)]
        title: Option<String>,
        /// New slug. Changing a published post's slug breaks every link to it.
        #[arg(long)]
        slug: Option<String>,
        #[command(flatten)]
        body: BodyArgs,
        #[command(flatten)]
        meta: MetaArgs,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Put a post's current draft on the public site.
    Publish {
        /// Post id (from `blog list`).
        post_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Changes the public site immediately — `--dry-run` / `--yes`.
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Take a post off the public site. It stays as a draft with its history.
    Unpublish {
        /// Post id (from `blog list`).
        post_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Changes the public site immediately — `--dry-run` / `--yes`.
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Permanently delete a DRAFT post and its revision history. Irreversible;
    /// a published post is refused upstream — unpublish it first.
    Delete {
        /// Post id (from `blog list`).
        post_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Images for posts: stock search (free), generate (paid), import (re-host your own).
    Image {
        #[command(subcommand)]
        cmd: ImageCmd,
    },
}

#[derive(Subcommand)]
pub enum ImageCmd {
    /// Search stock photography. Free — try this before `generate`.
    Search {
        /// What the picture should SHOW, e.g. "parent and child reading together".
        query: String,
        /// How many candidates (1–24, default 6).
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=24))]
        count: Option<u32>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Generate an original image and host it on the Smoo CDN. COSTS MONEY
    /// (paid image model) and takes several seconds; never auto-retried.
    Generate {
        /// Subject, setting and mood. Avoid asking for text inside the image.
        prompt: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Copy an image from an https URL onto the Smoo CDN so a post can use it (≤10MB).
    Import {
        /// Public https URL of the image.
        source_url: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

/// The two mutually-exclusive body forms.
#[derive(clap::Args, Debug, Default)]
pub struct BodyArgs {
    /// Post body as markdown (file path, or `-` for stdin).
    #[arg(long, conflicts_with = "blocks")]
    pub markdown: Option<String>,
    /// Post body as structured blocks JSON (file path, or `-` for stdin).
    #[arg(long)]
    pub blocks: Option<String>,
}

/// Display metadata — flat flags, plus `--metadata` for a whole object.
/// Flat flags win over the same key in `--metadata`.
#[derive(clap::Args, Debug, Default)]
pub struct MetaArgs {
    /// Metadata JSON object (file path, or `-` for stdin): excerpt, heroImage, heroImageAlt, category, author.
    #[arg(long)]
    pub metadata: Option<String>,
    /// One-paragraph summary for the index card, social preview and RSS.
    #[arg(long)]
    pub excerpt: Option<String>,
    /// Hero image URL (images.unsplash.com or Smoo-hosted). Needs `--hero-image-alt`.
    #[arg(long = "hero-image")]
    pub hero_image: Option<String>,
    /// Alt text for the hero image.
    #[arg(long = "hero-image-alt")]
    pub hero_image_alt: Option<String>,
    /// Short category shown above the title.
    #[arg(long)]
    pub category: Option<String>,
    /// Display author (defaults to the site's business name upstream).
    #[arg(long)]
    pub author: Option<String>,
}

/// Read a text file (or stdin for `-`).
/// Publish and unpublish change what the public sees on the org's site the
/// moment they run, so they print the org + post and confirm (SMOODEV-3606).
fn gate_public(verb: &str, post_id: &str, org: &str, confirm: Confirm) -> Result<bool> {
    crate::destructive::gate_with(
        &Target {
            verb,
            noun: "blog post",
            id: post_id,
            org,
            severity: Severity::Standard,
        },
        confirm,
    )
}

fn read_text(path: &str) -> Result<String> {
    if path == "-" {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("read stdin")?;
        Ok(s)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {path}"))
    }
}

/// Resolve the body flags into `{markdown}` / `{blocks}` / nothing.
/// Clap already rejects both at once; this reads the file(s).
fn body_fields(b: &BodyArgs) -> Result<Option<(&'static str, Value)>> {
    if let Some(p) = &b.markdown {
        return Ok(Some(("markdown", Value::String(read_text(p)?))));
    }
    if let Some(p) = &b.blocks {
        return Ok(Some(("blocks", read_body(p)?)));
    }
    Ok(None)
}

/// Mirror the MCP's `merge_blog_metadata`: flat flags win over the nested
/// object, `None` when nothing was supplied, and a hero image without alt
/// text is refused (the post goes on the public web).
fn merge_metadata(nested: Option<Value>, m: &MetaArgs) -> Result<Option<Value>> {
    let mut map = match nested {
        Some(Value::Object(o)) => o,
        Some(_) => bail!("--metadata must be a JSON object"),
        None => Map::new(),
    };
    for (key, value) in [
        ("excerpt", &m.excerpt),
        ("heroImage", &m.hero_image),
        ("heroImageAlt", &m.hero_image_alt),
        ("category", &m.category),
        ("author", &m.author),
    ] {
        if let Some(v) = value {
            map.insert(key.to_string(), Value::String(v.clone()));
        }
    }
    if map.contains_key("heroImage") && !map.contains_key("heroImageAlt") {
        bail!("--hero-image needs --hero-image-alt — describe the image for anyone who cannot see it");
    }
    Ok(if map.is_empty() { None } else { Some(Value::Object(map)) })
}

/// Build the `POST heypage/blog/posts` body. A body is required.
fn create_payload(title: &str, slug: &str, body: Option<(&'static str, Value)>, metadata: Option<Value>) -> Result<Value> {
    let Some((k, v)) = body else {
        bail!("the post needs a body: pass --markdown <file|-> (the usual choice) or --blocks <json-file|->");
    };
    let mut payload = json!({ "title": title, "slug": slug });
    payload[k] = v;
    if let Some(m) = metadata {
        payload["metadata"] = m;
    }
    Ok(payload)
}

/// Build the `PATCH heypage/blog/posts/{id}` body. Refuses an empty change.
fn update_payload(title: Option<&str>, slug: Option<&str>, body: Option<(&'static str, Value)>, metadata: Option<Value>) -> Result<Value> {
    let mut payload = json!({});
    if let Some(t) = title {
        payload["title"] = json!(t);
    }
    if let Some(s) = slug {
        payload["slug"] = json!(s);
    }
    if let Some((k, v)) = body {
        payload[k] = v;
    }
    if let Some(m) = metadata {
        payload["metadata"] = m;
    }
    if payload.as_object().is_some_and(Map::is_empty) {
        bail!("nothing to change — pass at least one of --title, --slug, --markdown, --blocks or metadata flags");
    }
    Ok(payload)
}

/// `GET heypage/blog/posts[?status=…]`, validating the status locally.
fn list_path(org: &str, status: Option<&str>) -> Result<String> {
    let base = format!("/organizations/{org}/heypage/blog/posts");
    match status {
        None => Ok(base),
        Some(s) if POST_STATUSES.contains(&s) => Ok(format!("{base}?status={s}")),
        Some(s) => bail!("unknown status {s:?} — use one of: {}", POST_STATUSES.join(", ")),
    }
}

/// `GET heypage/blog/images/search?q=…[&count=N]`.
fn image_search_path(org: &str, query: &str, count: Option<u32>) -> String {
    let mut p = format!("/organizations/{org}/heypage/blog/images/search?q={}", urlencoding::encode(query));
    if let Some(c) = count {
        p.push_str(&format!("&count={c}"));
    }
    p
}

/// `{post: {...}}` or the bare object.
fn unwrap<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(v)
}

fn render_list(body: &Value) {
    let posts = body.get("posts").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    println!();
    if posts.is_empty() {
        println!("  {} {}", "●".dimmed(), "no blog posts yet".dimmed());
        println!();
        return;
    }
    for p in posts {
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let title = if s("title").is_empty() { "(untitled)".to_string() } else { s("title") };
        let published = if s("publishedAt").is_empty() {
            String::new()
        } else {
            format!("  published {}", s("publishedAt"))
        };
        println!(
            "  {} {} {} {}{}",
            "○".dimmed(),
            s("id").cyan(),
            title.bold(),
            format!("[{}] /blog/{}", s("status"), s("slug")).dimmed(),
            published.dimmed()
        );
    }
    println!();
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List { status, org, json } => {
            let o = require_active_org(&client, org)?;
            let body = client.get(&list_path(&o, status.as_deref())?).await.context("GET heypage/blog/posts")?;
            if json {
                print_json(&body);
            } else {
                render_list(&body);
            }
        }
        Cmd::Get { post_id, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/heypage/blog/posts/{post_id}"))
                    .await
                    .context("GET heypage/blog/posts/{id}")?,
            );
        }
        Cmd::Create { title, slug, body, meta, org } => {
            let o = require_active_org(&client, org)?;
            let nested = meta.metadata.as_deref().map(read_body).transpose()?;
            let payload = create_payload(&title, &slug, body_fields(&body)?, merge_metadata(nested, &meta)?)?;
            let resp = client
                .post(&format!("/organizations/{o}/heypage/blog/posts"), Some(&payload))
                .await
                .context("POST heypage/blog/posts")?;
            eprintln_note("drafted — NOT public until `th smoo blog publish <id>`");
            print_json(unwrap(&resp, "post"));
        }
        Cmd::Update {
            post_id,
            title,
            slug,
            body,
            meta,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let nested = meta.metadata.as_deref().map(read_body).transpose()?;
            let payload = update_payload(title.as_deref(), slug.as_deref(), body_fields(&body)?, merge_metadata(nested, &meta)?)?;
            let resp = client
                .patch(&format!("/organizations/{o}/heypage/blog/posts/{post_id}"), &payload)
                .await
                .context("PATCH heypage/blog/posts/{id}")?;
            eprintln_note("draft updated — the live page is unchanged until `th smoo blog publish`");
            print_json(unwrap(&resp, "post"));
        }
        Cmd::Publish { post_id, org, confirm } => {
            let o = require_active_org(&client, org)?;
            if !gate_public("publish", &post_id, &o, confirm)? {
                return Ok(());
            }
            let resp = client
                .post(&format!("/organizations/{o}/heypage/blog/posts/{post_id}/publish"), Some(&json!({})))
                .await
                .context("POST heypage/blog/posts/{id}/publish")?;
            print_json(unwrap(&resp, "post"));
        }
        Cmd::Unpublish { post_id, org, confirm } => {
            let o = require_active_org(&client, org)?;
            if !gate_public("unpublish", &post_id, &o, confirm)? {
                return Ok(());
            }
            let resp = client
                .post(&format!("/organizations/{o}/heypage/blog/posts/{post_id}/unpublish"), Some(&json!({})))
                .await
                .context("POST heypage/blog/posts/{id}/unpublish")?;
            print_json(unwrap(&resp, "post"));
        }
        Cmd::Delete { post_id, org, confirm } => {
            let o = require_active_org(&client, org)?;
            let proceed = crate::destructive::gate_with(
                &Target {
                    verb: "delete",
                    noun: "blog post",
                    id: &post_id,
                    org: &o,
                    severity: Severity::Irreversible,
                },
                confirm,
            )?;
            if proceed {
                let resp = client
                    .delete(&format!("/organizations/{o}/heypage/blog/posts/{post_id}"))
                    .await
                    .context("DELETE heypage/blog/posts/{id} (a published post is refused — unpublish it first)")?;
                println!("  {} deleted blog post {}", "✗".red(), post_id.dimmed());
                print_json(unwrap(&resp, "deleted"));
            }
        }
        Cmd::Image { cmd } => match cmd {
            ImageCmd::Search { query, count, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .get(&image_search_path(&o, &query, count))
                        .await
                        .context("GET heypage/blog/images/search")?,
                );
            }
            ImageCmd::Generate { prompt, org } => {
                let o = require_active_org(&client, org)?;
                let resp = client
                    .post(&format!("/organizations/{o}/heypage/blog/images/generate"), Some(&json!({ "prompt": prompt })))
                    .await
                    .context(
                        "POST heypage/blog/images/generate (paid; not retried — on a timeout the image may still have been made, \
                         so do not blindly re-run)",
                    )?;
                print_json(unwrap(&resp, "image"));
            }
            ImageCmd::Import { source_url, org } => {
                let o = require_active_org(&client, org)?;
                let resp = client
                    .post(
                        &format!("/organizations/{o}/heypage/blog/images/import"),
                        Some(&json!({ "sourceUrl": source_url })),
                    )
                    .await
                    .context("POST heypage/blog/images/import")?;
                print_json(unwrap(&resp, "image"));
            }
        },
    }
    Ok(())
}

/// A one-line status note on stderr, so `--json`-style stdout stays parseable.
fn eprintln_note(msg: &str) {
    anstream::eprintln!("  {} {}", "●".dimmed(), msg.dimmed());
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    fn parse(args: &[&str]) -> std::result::Result<Cmd, clap::Error> {
        let mut v = vec!["t"];
        v.extend_from_slice(args);
        Wrap::try_parse_from(v).map(|w| w.cmd)
    }

    #[test]
    fn every_verb_parses() {
        assert!(matches!(parse(&["list", "--json", "--status", "draft"]).unwrap(), Cmd::List { json: true, .. }));
        assert!(matches!(parse(&["list"]).unwrap(), Cmd::List { json: false, status: None, .. }));
        assert!(matches!(parse(&["get", "p1"]).unwrap(), Cmd::Get { ref post_id, .. } if post_id == "p1"));
        assert!(matches!(
            parse(&["create", "--title", "T", "--slug", "t", "--markdown", "-"]).unwrap(),
            Cmd::Create { .. }
        ));
        assert!(matches!(parse(&["update", "p1", "--title", "New"]).unwrap(), Cmd::Update { .. }));
        assert!(matches!(parse(&["publish", "p1"]).unwrap(), Cmd::Publish { .. }));
        assert!(matches!(parse(&["unpublish", "p1"]).unwrap(), Cmd::Unpublish { .. }));
        assert!(matches!(
            parse(&["image", "search", "a dog", "--count", "3"]).unwrap(),
            Cmd::Image {
                cmd: ImageCmd::Search { count: Some(3), .. }
            }
        ));
        assert!(matches!(
            parse(&["image", "generate", "a dog"]).unwrap(),
            Cmd::Image {
                cmd: ImageCmd::Generate { .. }
            }
        ));
        assert!(matches!(
            parse(&["image", "import", "https://x.test/a.png"]).unwrap(),
            Cmd::Image { cmd: ImageCmd::Import { .. } }
        ));
    }

    #[test]
    fn create_requires_title_and_slug_and_rejects_two_bodies() {
        assert!(parse(&["create", "--slug", "t", "--markdown", "-"]).is_err(), "title is required");
        assert!(parse(&["create", "--title", "T", "--markdown", "-"]).is_err(), "slug is required");
        assert!(
            parse(&["create", "--title", "T", "--slug", "t", "--markdown", "a.md", "--blocks", "b.json"]).is_err(),
            "markdown and blocks are mutually exclusive"
        );
    }

    #[test]
    fn delete_carries_the_confirm_gate_flags() {
        match parse(&["delete", "p1"]).unwrap() {
            Cmd::Delete { confirm, .. } => assert!(!confirm.yes && !confirm.dry_run, "confirmation is on by default"),
            _ => panic!("wrong variant"),
        }
        match parse(&["delete", "p1", "--yes"]).unwrap() {
            Cmd::Delete { confirm, .. } => assert!(confirm.yes),
            _ => panic!("wrong variant"),
        }
        assert!(matches!(
            parse(&["delete", "p1", "--dry-run"]).unwrap(),
            Cmd::Delete {
                confirm: Confirm { dry_run: true, .. },
                ..
            }
        ));
    }

    #[test]
    fn image_search_count_is_bounded() {
        assert!(parse(&["image", "search", "q", "--count", "0"]).is_err());
        assert!(parse(&["image", "search", "q", "--count", "25"]).is_err());
        assert!(parse(&["image", "search", "q", "--count", "24"]).is_ok());
    }

    #[test]
    fn create_payload_requires_a_body_and_carries_metadata() {
        assert!(create_payload("T", "t", None, None).is_err(), "a post needs a body");
        let p = create_payload("T", "t", Some(("markdown", json!("# Hi"))), Some(json!({ "excerpt": "e" }))).unwrap();
        assert_eq!(p, json!({ "title": "T", "slug": "t", "markdown": "# Hi", "metadata": { "excerpt": "e" } }));
        let b = create_payload("T", "t", Some(("blocks", json!([{ "type": "RichText" }]))), None).unwrap();
        assert_eq!(b["blocks"][0]["type"], "RichText");
        assert!(b.get("metadata").is_none(), "no metadata key when nothing was set");
    }

    #[test]
    fn update_payload_refuses_an_empty_change() {
        assert!(update_payload(None, None, None, None).is_err());
        let p = update_payload(Some("New"), None, None, None).unwrap();
        assert_eq!(p, json!({ "title": "New" }));
        let p = update_payload(None, Some("s"), Some(("markdown", json!("x"))), Some(json!({ "author": "A" }))).unwrap();
        assert_eq!(p, json!({ "slug": "s", "markdown": "x", "metadata": { "author": "A" } }));
    }

    #[test]
    fn metadata_merge_flat_wins_and_hero_needs_alt() {
        let m = MetaArgs {
            excerpt: Some("flat".into()),
            ..Default::default()
        };
        let merged = merge_metadata(Some(json!({ "excerpt": "nested", "category": "c" })), &m).unwrap().unwrap();
        assert_eq!(merged, json!({ "excerpt": "flat", "category": "c" }));

        assert_eq!(merge_metadata(None, &MetaArgs::default()).unwrap(), None, "nothing set → key omitted");

        let hero = MetaArgs {
            hero_image: Some("https://images.unsplash.com/x".into()),
            ..Default::default()
        };
        assert!(merge_metadata(None, &hero).is_err(), "hero image without alt is refused");
        assert!(
            merge_metadata(Some(json!({ "heroImageAlt": "a dog" })), &hero).is_ok(),
            "alt may come from --metadata"
        );
        assert!(merge_metadata(Some(json!("str")), &MetaArgs::default()).is_err(), "non-object metadata refused");
    }

    #[test]
    fn body_fields_reads_markdown_text_and_blocks_json() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("p.md");
        std::fs::write(&md, "# Title\n\nBody").unwrap();
        let (k, v) = body_fields(&BodyArgs {
            markdown: Some(md.to_str().unwrap().into()),
            blocks: None,
        })
        .unwrap()
        .unwrap();
        assert_eq!((k, v), ("markdown", json!("# Title\n\nBody")));

        let bl = dir.path().join("b.json");
        std::fs::write(&bl, r#"[{"type":"RichText"}]"#).unwrap();
        let (k, v) = body_fields(&BodyArgs {
            markdown: None,
            blocks: Some(bl.to_str().unwrap().into()),
        })
        .unwrap()
        .unwrap();
        assert_eq!(k, "blocks");
        assert_eq!(v[0]["type"], "RichText");

        assert!(body_fields(&BodyArgs::default()).unwrap().is_none());
    }

    #[test]
    fn paths_match_the_mcp_routes() {
        assert_eq!(list_path("o1", None).unwrap(), "/organizations/o1/heypage/blog/posts");
        assert_eq!(list_path("o1", Some("draft")).unwrap(), "/organizations/o1/heypage/blog/posts?status=draft");
        assert!(list_path("o1", Some("live")).is_err(), "unknown status names the allowed set");
        assert_eq!(
            image_search_path("o1", "a parent & child", Some(6)),
            "/organizations/o1/heypage/blog/images/search?q=a%20parent%20%26%20child&count=6"
        );
        assert_eq!(image_search_path("o1", "dog", None), "/organizations/o1/heypage/blog/images/search?q=dog");
    }

    #[test]
    fn unwrap_handles_envelope_and_bare() {
        assert_eq!(unwrap(&json!({ "post": { "id": "p" } }), "post")["id"], "p");
        assert_eq!(unwrap(&json!({ "id": "p" }), "post")["id"], "p");
    }
}

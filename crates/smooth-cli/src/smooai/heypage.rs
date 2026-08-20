//! `th heypage …` — dogfood HeyPage through the REAL product API.
//!
//! Drives the same endpoints a customer (or the web builder) hits:
//! generate a SiteSpec → create a site → publish it → live at
//! `heypage.ai/p/<slug>`. No backdoor: server-side `@smooai/config`
//! owns every secret, so this needs nothing but an authed `th` session
//! and an org with the `heypage` custom app enabled.
//!
//! The generate step reads the `heypageStudioPipeline` feature flag
//! server-side (ADR-065) — there is NO per-request studio toggle, so
//! there's deliberately no `--studio` flag here (flip the flag with
//! `th config set heypageStudioPipeline true --tier feature_flag`).
//!
//! There is also no URL-crawl → brand seam: the generate endpoint takes
//! `brand` (palette hexes + logo) and `assetUrls` directly, so `--url`
//! is a follow-up, not wired.

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::json;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};
use smooth_api_client::SmoothApiClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// Generate a SiteSpec from a brief (no site created). Prints spec + reasoning.
    Generate {
        /// Brief JSON (file path, or `-` for stdin). Either the SiteBrief
        /// object itself (`{businessName, industry, description, goals, pages?}`)
        /// or a wrapper `{brief, brand?, assetUrls?}` mirroring the endpoint.
        #[arg(long)]
        brief: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// The full dogfood: generate → create site → (optionally) publish. Prints the live /p/<slug> URL.
    Build {
        /// Brief JSON (file path, or `-` for stdin). See `generate --brief`.
        #[arg(long)]
        brief: String,
        /// Site name — seeds the globally-unique slug.
        #[arg(long)]
        name: String,
        /// Publish after creating (moderates, then goes live).
        #[arg(long)]
        publish: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Regenerate an EXISTING site in place: fresh spec → new revision on each
    /// page it already has → publish. Idempotent — updates the live site (its
    /// slug/URL are unchanged), never spawns a duplicate. This is the path that
    /// refreshes the live showcase examples.
    Regen {
        /// Site id to regenerate. Or use `--slug` to resolve the id via `list`.
        #[arg(long)]
        site: Option<String>,
        /// Site slug (resolved to an id via `list`) — the loop-friendly form,
        /// e.g. `--slug claysmith-studio`. Mutually exclusive with `--site`.
        #[arg(long, conflicts_with = "site")]
        slug: Option<String>,
        /// Brief JSON (file path, or `-` for stdin). See `generate --brief`.
        #[arg(long)]
        brief: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// List the org's sites (id + slug + name). Slug resolution for `regen --slug`.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Publish (moderate, then go live) an existing site.
    Publish {
        /// Site id.
        #[arg(long)]
        site: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Editor view of a site — pages + latest revisions + publish state.
    Get {
        /// Site id.
        #[arg(long)]
        site: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Publish history, newest first — publishIds (for `rollback`) + view links.
    Versions {
        /// Site id. Or use `--slug`.
        #[arg(long, conflicts_with = "slug")]
        site: Option<String>,
        /// Site slug (resolved to an id via `list`).
        #[arg(long)]
        slug: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Put the LIVE site back to a previous publish. Appends a new publish
    /// rather than erasing history, so a rollback can itself be rolled back.
    Rollback {
        /// Site id. Or use `--slug`.
        #[arg(long, conflicts_with = "slug")]
        site: Option<String>,
        /// Site slug (resolved to an id via `list`).
        #[arg(long)]
        slug: Option<String>,
        /// A specific publishId from `versions`. Omit for the one before the current live publish.
        #[arg(long = "publish-id")]
        publish_id: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Read/write a site's SOURCE: component HTML/CSS/JS, stylesheet rules, page SEO text.
    Source {
        #[command(subcommand)]
        cmd: SourceCmd,
    },
    /// Read/write a site's editable slot CONTENT (text, images, links) — no regeneration.
    Content {
        #[command(subcommand)]
        cmd: ContentCmd,
    },
}

#[derive(Subcommand)]
pub enum SourceCmd {
    /// Every component's HTML/CSS/JS + slots + the site's design brief.
    Get {
        /// Site id. Or use `--slug`.
        #[arg(long, conflicts_with = "slug")]
        site: Option<String>,
        /// Site slug (resolved to an id via `list`).
        #[arg(long)]
        slug: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Write source back: body JSON `{components?, siteCssEdits?, pages?}`.
    /// Components are replaced WHOLE — never a fragment. Creates a DRAFT;
    /// nothing is public until `publish`.
    Set {
        /// Site id. Or use `--slug`.
        #[arg(long, conflicts_with = "slug")]
        site: Option<String>,
        /// Site slug (resolved to an id via `list`).
        #[arg(long)]
        slug: Option<String>,
        /// Body JSON (file path, or `-` for stdin).
        #[arg(long)]
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ContentCmd {
    /// Every editable slot, addressed by page path + instanceIndex + slot name.
    /// Read this before `content set` — its slot names are the only valid targets.
    Get {
        /// Site id. Or use `--slug`.
        #[arg(long, conflicts_with = "slug")]
        site: Option<String>,
        /// Site slug (resolved to an id via `list`).
        #[arg(long)]
        slug: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Edit slot values: body JSON `{path, updates: [{instanceIndex, content: {slot: value}}]}`.
    /// A direct edit, not a regeneration. Creates a DRAFT; nothing is public until `publish`.
    Set {
        /// Site id. Or use `--slug`.
        #[arg(long, conflicts_with = "slug")]
        site: Option<String>,
        /// Site slug (resolved to an id via `list`).
        #[arg(long)]
        slug: Option<String>,
        /// Body JSON (file path, or `-` for stdin).
        #[arg(long)]
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

/// Read the `--brief` file and shape it into the generate request body.
/// Accepts either the bare SiteBrief object or a `{brief, brand?, assetUrls?}`
/// wrapper; injects `contentType: "website"` either way.
fn generate_body(brief_path: &str) -> Result<serde_json::Value> {
    let mut body = read_body(brief_path)?;
    if body.get("brief").is_none() {
        // Bare SiteBrief — wrap it so the endpoint's `body.brief` read finds it.
        body = json!({ "brief": body });
    }
    body["contentType"] = json!("website");
    Ok(body)
}

/// Pull the live `heypage.ai/p/<slug>` URL out of a response carrying a site
/// (`{ site: { slug, .. } }` — create-site or site-detail) or its bare fallback.
fn live_url(resp: &serde_json::Value) -> Option<String> {
    let site = resp.get("site").unwrap_or(resp);
    site.get("slug").and_then(|v| v.as_str()).map(|s| format!("https://heypage.ai/p/{s}"))
}

/// Page paths present in BOTH a freshly-generated spec (`spec.pages` object,
/// keyed by path) and the existing site detail (`detail.pages[].page.path`),
/// in the spec's order. The PUT-revision endpoint 404s on a path the site
/// doesn't have, so regen only touches paths that already exist.
fn revisable_paths(spec: &serde_json::Value, detail: &serde_json::Value) -> Vec<String> {
    let existing: std::collections::HashSet<&str> = detail
        .get("pages")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p.get("page").and_then(|pg| pg.get("path")).and_then(|v| v.as_str()))
                .collect()
        })
        .unwrap_or_default();
    spec.get("pages")
        .and_then(|v| v.as_object())
        .map(|m| m.keys().filter(|k| existing.contains(k.as_str())).cloned().collect())
        .unwrap_or_default()
}

/// Resolve a regen target to a site id: `--site` wins; otherwise `--slug` is
/// looked up against the org's site list (`GET heypage/sites`). Errors if
/// neither is given or the slug matches no site.
async fn resolve_site_id(client: &SmoothApiClient, org: &str, site: Option<String>, slug: Option<String>) -> Result<String> {
    if let Some(id) = site {
        return Ok(id);
    }
    let slug = slug.context("pass --site <id> or --slug <slug>")?;
    let list = client.get(&format!("/organizations/{org}/heypage/sites")).await.context("GET heypage/sites")?;
    find_slug_id(&list, &slug).with_context(|| format!("no site with slug {slug:?} in org {org}"))
}

/// Find the `id` of the site whose `slug` matches, in a `{data:[…]}` envelope
/// or a bare array.
fn find_slug_id(list: &serde_json::Value, slug: &str) -> Option<String> {
    list.get("data")
        .and_then(|v| v.as_array())
        .or_else(|| list.as_array())
        .into_iter()
        .flatten()
        .find(|s| s.get("slug").and_then(|v| v.as_str()) == Some(slug))
        .and_then(|s| s.get("id").and_then(|v| v.as_str()).map(str::to_string))
}

/// Mirror the MCP guard: refuse an empty source-edit body before the roundtrip.
fn require_source_edits(body: &serde_json::Value) -> Result<()> {
    let has = |k: &str| body.get(k).and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty());
    if has("components") || has("siteCssEdits") || has("pages") {
        Ok(())
    } else {
        anyhow::bail!("no edits — the body needs a non-empty `components`, `siteCssEdits`, or `pages` array")
    }
}

/// Mirror the MCP guard: a content edit needs a `path` (`""` = home) and at
/// least one entry in `updates`.
fn require_content_edits(body: &serde_json::Value) -> Result<()> {
    if body.get("path").and_then(|v| v.as_str()).is_none() {
        anyhow::bail!("body needs a `path` string (\"\" = home page)");
    }
    if body.get("updates").and_then(|v| v.as_array()).is_none_or(Vec::is_empty) {
        anyhow::bail!("no edits — the body needs a non-empty `updates` array");
    }
    Ok(())
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Generate { brief, org } => {
            let o = require_active_org(&client, org)?;
            let body = generate_body(&brief)?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/content-items/generate"), Some(&body))
                    .await
                    .context("POST content-items/generate")?,
            );
        }
        Cmd::Build { brief, name, publish, org } => {
            let o = require_active_org(&client, org)?;

            // 1. Generate the SiteSpec.
            let gen = client
                .post(&format!("/organizations/{o}/content-items/generate"), Some(&generate_body(&brief)?))
                .await
                .context("POST content-items/generate")?;
            let spec = gen.get("spec").context("generate response had no `spec`")?;

            // 2. Create the site from that spec.
            let site_resp = client
                .post(&format!("/organizations/{o}/heypage/sites"), Some(&json!({ "name": name, "spec": spec })))
                .await
                .context("POST heypage/sites")?;
            let site = site_resp.get("site").unwrap_or(&site_resp);
            let site_id = site
                .get("id")
                .and_then(|v| v.as_str())
                .context("create-site response had no site.id")?
                .to_string();

            // 3. Publish, if asked.
            if publish {
                client
                    .post(&format!("/organizations/{o}/heypage/sites/{site_id}/publish"), None)
                    .await
                    .context("POST heypage/sites/{id}/publish (422 = moderation-flagged)")?;
            }

            print_json(&json!({
                "siteId": site_id,
                "published": publish,
                "url": live_url(&site_resp),
            }));
        }
        Cmd::Regen { site, slug, brief, org } => {
            let o = require_active_org(&client, org)?;
            let site = resolve_site_id(&client, &o, site, slug).await?;

            // 1. Fresh spec from the brief.
            let gen = client
                .post(&format!("/organizations/{o}/content-items/generate"), Some(&generate_body(&brief)?))
                .await
                .context("POST content-items/generate")?;
            let spec = gen.get("spec").context("generate response had no `spec`")?;

            // 2. Existing pages on the site — revisions only append to paths it has.
            let detail = client
                .get(&format!("/organizations/{o}/heypage/sites/{site}"))
                .await
                .context("GET heypage/sites/{id}")?;

            // 3. New revision per shared page path.
            let paths = revisable_paths(spec, &detail);
            for path in &paths {
                let page_spec = &spec["pages"][path];
                client
                    .put(
                        &format!("/organizations/{o}/heypage/sites/{site}/pages/revision"),
                        &json!({ "path": path, "spec": page_spec }),
                    )
                    .await
                    .with_context(|| format!("PUT heypage/sites/{site}/pages/revision (path {path:?})"))?;
            }

            // 4. Publish.
            client
                .post(&format!("/organizations/{o}/heypage/sites/{site}/publish"), None)
                .await
                .context("POST heypage/sites/{id}/publish (422 = moderation-flagged)")?;

            print_json(&json!({
                "siteId": site,
                "updatedPaths": paths,
                "published": true,
                "url": live_url(&detail),
            }));
        }
        Cmd::List { org } => {
            let o = require_active_org(&client, org)?;
            print_list_envelope(
                &client.get(&format!("/organizations/{o}/heypage/sites")).await.context("GET heypage/sites")?,
                "sites",
            );
        }
        Cmd::Publish { site, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .post(&format!("/organizations/{o}/heypage/sites/{site}/publish"), None)
                    .await
                    .context("POST heypage/sites/{id}/publish")?,
            );
        }
        Cmd::Get { site, org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/heypage/sites/{site}"))
                    .await
                    .context("GET heypage/sites/{id}")?,
            );
        }
        Cmd::Versions { site, slug, org } => {
            let o = require_active_org(&client, org)?;
            let site = resolve_site_id(&client, &o, site, slug).await?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/heypage/sites/{site}/versions"))
                    .await
                    .context("GET heypage/sites/{id}/versions")?,
            );
        }
        Cmd::Rollback { site, slug, publish_id, org } => {
            let o = require_active_org(&client, org)?;
            let site = resolve_site_id(&client, &o, site, slug).await?;
            let body = publish_id.map_or_else(|| json!({}), |id| json!({ "publishId": id }));
            print_json(
                &client
                    .post(&format!("/organizations/{o}/heypage/sites/{site}/rollback"), Some(&body))
                    .await
                    .context("POST heypage/sites/{id}/rollback")?,
            );
        }
        Cmd::Source { cmd } => match cmd {
            SourceCmd::Get { site, slug, org } => {
                let o = require_active_org(&client, org)?;
                let site = resolve_site_id(&client, &o, site, slug).await?;
                print_json(
                    &client
                        .get(&format!("/organizations/{o}/heypage/sites/{site}/source"))
                        .await
                        .context("GET heypage/sites/{id}/source")?,
                );
            }
            SourceCmd::Set { site, slug, body, org } => {
                let o = require_active_org(&client, org)?;
                let site = resolve_site_id(&client, &o, site, slug).await?;
                let payload = read_body(&body)?;
                require_source_edits(&payload)?;
                print_json(
                    &client
                        .put(&format!("/organizations/{o}/heypage/sites/{site}/source"), &payload)
                        .await
                        .context("PUT heypage/sites/{id}/source")?,
                );
            }
        },
        Cmd::Content { cmd } => match cmd {
            ContentCmd::Get { site, slug, org } => {
                let o = require_active_org(&client, org)?;
                let site = resolve_site_id(&client, &o, site, slug).await?;
                print_json(
                    &client
                        .get(&format!("/organizations/{o}/heypage/sites/{site}/editable-content"))
                        .await
                        .context("GET heypage/sites/{id}/editable-content")?,
                );
            }
            ContentCmd::Set { site, slug, body, org } => {
                let o = require_active_org(&client, org)?;
                let site = resolve_site_id(&client, &o, site, slug).await?;
                let payload = read_body(&body)?;
                require_content_edits(&payload)?;
                print_json(
                    &client
                        .patch(&format!("/organizations/{o}/heypage/sites/{site}/content"), &payload)
                        .await
                        .context("PATCH heypage/sites/{id}/content")?,
                );
            }
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_brief_gets_wrapped_and_typed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("b.json");
        std::fs::write(&p, r#"{"businessName":"Acme","industry":"x","description":"y","goals":"z"}"#).unwrap();
        let body = generate_body(p.to_str().unwrap()).unwrap();
        assert_eq!(body["contentType"], "website");
        assert_eq!(body["brief"]["businessName"], "Acme");
    }

    #[test]
    fn wrapper_brief_is_left_intact() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("b.json");
        std::fs::write(&p, r#"{"brief":{"businessName":"Acme"},"assetUrls":["u"]}"#).unwrap();
        let body = generate_body(p.to_str().unwrap()).unwrap();
        assert_eq!(body["contentType"], "website");
        assert_eq!(body["brief"]["businessName"], "Acme");
        assert_eq!(body["assetUrls"][0], "u");
    }

    #[test]
    fn revisable_paths_intersects_spec_and_site() {
        let spec = json!({ "pages": { "": {}, "classes": {}, "new-page": {} } });
        let detail = json!({ "pages": [{ "page": { "path": "" } }, { "page": { "path": "classes" } }, { "page": { "path": "gallery" } }] });
        let got = revisable_paths(&spec, &detail);
        // "new-page" (not on site) and "gallery" (not in spec) are excluded.
        assert_eq!(got, vec!["".to_string(), "classes".to_string()]);
    }

    #[test]
    fn find_slug_id_matches_in_envelope_and_bare() {
        let env = json!({ "data": [{ "id": "a", "slug": "claysmith-studio" }, { "id": "b", "slug": "north-fork-caf" }] });
        assert_eq!(find_slug_id(&env, "north-fork-caf"), Some("b".to_string()));
        assert_eq!(find_slug_id(&env, "nope"), None);
        let bare = json!([{ "id": "c", "slug": "jesse-builds" }]);
        assert_eq!(find_slug_id(&bare, "jesse-builds"), Some("c".to_string()));
    }

    #[test]
    fn live_url_from_wrapped_and_bare() {
        let wrapped = json!({ "site": { "slug": "acme" } });
        assert_eq!(live_url(&wrapped).unwrap(), "https://heypage.ai/p/acme");
        let bare = json!({ "slug": "beta" });
        assert_eq!(live_url(&bare).unwrap(), "https://heypage.ai/p/beta");
    }

    /// MCP parity (pearl th-088c93): `versions`/`rollback`/`source`/`content`
    /// mirror the hosted `site_*` tools.
    #[test]
    fn parity_verbs_parse() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(subcommand)]
            cmd: Cmd,
        }
        let v = Wrap::try_parse_from(["t", "versions", "--site", "s1"]).expect("versions must parse");
        assert!(matches!(v.cmd, Cmd::Versions { site: Some(ref s), .. } if s == "s1"));

        let r = Wrap::try_parse_from(["t", "rollback", "--slug", "acme", "--publish-id", "p9"]).expect("rollback must parse");
        match r.cmd {
            Cmd::Rollback { site, slug, publish_id, .. } => {
                assert_eq!(site, None);
                assert_eq!(slug.as_deref(), Some("acme"));
                assert_eq!(publish_id.as_deref(), Some("p9"));
            }
            _ => panic!("parsed the wrong variant"),
        }
        // publish-id is optional: bare rollback restores the previous publish.
        let bare = Wrap::try_parse_from(["t", "rollback", "--site", "s1"]).expect("rollback without --publish-id must parse");
        assert!(matches!(bare.cmd, Cmd::Rollback { publish_id: None, .. }));
        // --site and --slug are mutually exclusive.
        assert!(Wrap::try_parse_from(["t", "rollback", "--site", "s1", "--slug", "acme"]).is_err());

        let sg = Wrap::try_parse_from(["t", "source", "get", "--site", "s1"]).expect("source get must parse");
        assert!(matches!(sg.cmd, Cmd::Source { cmd: SourceCmd::Get { .. } }));
        let ss = Wrap::try_parse_from(["t", "source", "set", "--site", "s1", "--body", "-"]).expect("source set must parse");
        assert!(matches!(ss.cmd, Cmd::Source { cmd: SourceCmd::Set { ref body, .. } } if body == "-"));
        assert!(
            Wrap::try_parse_from(["t", "source", "set", "--site", "s1"]).is_err(),
            "source set requires --body"
        );

        let cg = Wrap::try_parse_from(["t", "content", "get", "--slug", "acme"]).expect("content get must parse");
        assert!(matches!(cg.cmd, Cmd::Content { cmd: ContentCmd::Get { .. } }));
        let cs = Wrap::try_parse_from(["t", "content", "set", "--site", "s1", "--body", "edits.json"]).expect("content set must parse");
        assert!(matches!(cs.cmd, Cmd::Content { cmd: ContentCmd::Set { .. } }));
    }

    #[test]
    fn source_edit_guard_requires_a_nonempty_edit_array() {
        assert!(require_source_edits(&json!({ "components": [{ "id": "hero", "html": "<div/>" }] })).is_ok());
        assert!(require_source_edits(&json!({ "siteCssEdits": [{ "op": "upsert", "selector": ".x" }] })).is_ok());
        assert!(require_source_edits(&json!({ "pages": [{ "path": "", "title": "T" }] })).is_ok());
        assert!(require_source_edits(&json!({})).is_err());
        assert!(require_source_edits(&json!({ "components": [] })).is_err(), "empty arrays are not edits");
        assert!(require_source_edits(&json!({ "components": "hero" })).is_err(), "non-array shapes are refused");
    }

    #[test]
    fn content_edit_guard_requires_path_and_updates() {
        let ok = json!({ "path": "", "updates": [{ "instanceIndex": 0, "content": { "heading": "Hi" } }] });
        assert!(require_content_edits(&ok).is_ok(), "\"\" is the home page, a valid path");
        assert!(require_content_edits(&json!({ "updates": [{}] })).is_err(), "path is required");
        assert!(require_content_edits(&json!({ "path": "about" })).is_err(), "updates is required");
        assert!(
            require_content_edits(&json!({ "path": "about", "updates": [] })).is_err(),
            "empty updates are not edits"
        );
    }
}

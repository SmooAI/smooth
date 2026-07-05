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

use super::{print_json, read_body, require_active_org, require_authed};

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

/// Pull the live `heypage.ai/p/<slug>` URL out of a create-site response
/// (`{ site: { slug, .. } }`) or its bare-site fallback.
fn live_url(site_resp: &serde_json::Value) -> Option<String> {
    let site = site_resp.get("site").unwrap_or(site_resp);
    site.get("slug").and_then(|v| v.as_str()).map(|s| format!("https://heypage.ai/p/{s}"))
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
    fn live_url_from_wrapped_and_bare() {
        let wrapped = json!({ "site": { "slug": "acme" } });
        assert_eq!(live_url(&wrapped).unwrap(), "https://heypage.ai/p/acme");
        let bare = json!({ "slug": "beta" });
        assert_eq!(live_url(&bare).unwrap(), "https://heypage.ai/p/beta");
    }
}

//! `th smoo templates generate` — generate Content Builder content (SMOODEV-3609).
//!
//! Posts a brief to `POST /organizations/{org}/content-items/generate` and prints
//! the generated spec (a landing page, an email template or a form). Nothing is
//! published or sent; `--save-draft` also stores it as a DRAFT content item. CLI
//! twin of the Smooth Operator's `templates.generate` and the hosted MCP's
//! `templates_generate`. Needs the `contentbuilder` product and the "Generate
//! content templates" (`content.templates.write`) permission.

use anstream::println;
use anyhow::{Context, Result};
use clap::{Subcommand, ValueEnum};
use serde_json::json;

use super::{print_json, require_active_org, require_authed};

/// What to generate — the route's own list.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ContentType {
    LandingPage,
    EmailTemplate,
    Form,
}

impl ContentType {
    fn wire(self) -> &'static str {
        match self {
            Self::LandingPage => "landing_page",
            Self::EmailTemplate => "email_template",
            Self::Form => "form",
        }
    }
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Generate a spec from a natural-language brief.
    Generate {
        /// What the content should be about / do.
        prompt: String,
        /// The kind of content.
        #[arg(long = "type", value_enum)]
        content_type: ContentType,
        /// Also save it as a DRAFT content item (signed-in user only).
        #[arg(long = "save-draft")]
        save_draft: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

fn body(prompt: &str, content_type: ContentType, save_draft: bool) -> Result<serde_json::Value> {
    let prompt = prompt.trim();
    if prompt.chars().count() < 4 {
        anyhow::bail!("the brief is too short — describe what the content should say or do");
    }
    Ok(json!({ "prompt": prompt, "contentType": content_type.wire(), "autoSave": save_draft }))
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Generate {
            prompt,
            content_type,
            save_draft,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let resp = client
                .post(
                    &format!("/organizations/{o}/content-items/generate"),
                    Some(&body(&prompt, content_type, save_draft)?),
                )
                .await
                .context("POST content-items/generate (needs contentbuilder + content.templates.write)")?;
            if let Some(reasoning) = resp.get("reasoning").and_then(|v| v.as_str()) {
                println!("{reasoning}");
            }
            if let Some(id) = resp.get("contentItemId").and_then(|v| v.as_str()) {
                println!("Saved as draft content item {id}.");
            }
            print_json(resp.get("spec").unwrap_or(&resp));
        }
    }
    Ok(())
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

    #[test]
    fn generate_parses_and_maps_the_type() {
        let w = Wrap::try_parse_from(["t", "generate", "Spring sale", "--type", "email-template", "--save-draft"]).unwrap();
        let Cmd::Generate {
            prompt,
            content_type,
            save_draft,
            ..
        } = w.cmd;
        let b = body(&prompt, content_type, save_draft).unwrap();
        assert_eq!(b["contentType"], "email_template");
        assert_eq!(b["autoSave"], true);
        assert!(Wrap::try_parse_from(["t", "generate", "x", "--type", "sms-template"]).is_err());
        assert!(body("hi", ContentType::Form, false).is_err());
    }
}

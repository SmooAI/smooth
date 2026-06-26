//! `th api keys …` — Smoo AI auth clients.
//!
//! Two client types, both minted here:
//!
//! - **M2M** (`machine-to-machine`): a server/CI credential — returns a
//!   `client_id` + **secret key** (shown once). Used for
//!   `client_credentials` grants against `auth.smoo.ai`.
//! - **B2M** (`browser-to-machine`): a browser/frontend credential —
//!   returns a **publishable key** (shown once) restricted to an
//!   allowlist of origins. Used by embeddable widgets where the key is
//!   exposed to the page, so origin-pinning is the security boundary.
//!
//! The backend (`/organizations/{org_id}/auth-clients`) requires a
//! dashboard **user** session (`auth.provider === 'supabase'`) and 403s
//! under M2M — so this surface uses [`UserClient`], the user-JWT client.
//! A master admin can target a child org with `--org-id` (user JWT acts
//! cross-org). There is no in-place rotate endpoint: `rotate` mints a
//! fresh client of the same type/origins then revokes the old one.

use anyhow::{bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::print_json;
use crate::smooai::user_client::UserClient;

/// Auth-client kind. The CLI uses the short `m2m`/`b2m` spellings; the
/// API uses the long `machine-to-machine`/`browser-to-machine` forms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ClientType {
    /// machine-to-machine — server/CI secret (client_id + secret key).
    #[value(name = "m2m", alias = "machine-to-machine")]
    M2m,
    /// browser-to-machine — origin-restricted publishable key for browsers.
    #[value(name = "b2m", alias = "browser-to-machine")]
    B2m,
}

impl ClientType {
    fn api_value(self) -> &'static str {
        match self {
            ClientType::M2m => "machine-to-machine",
            ClientType::B2m => "browser-to-machine",
        }
    }
}

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's auth clients — both M2M and B2M — with their type,
    /// created/expiry timestamps, and (for B2M) the allowed origins.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then
        /// the credentials file's `active_org_id`.
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the pretty list.
        #[arg(long)]
        json: bool,
    },
    /// Create an auth client. M2M (default) returns a secret key for
    /// server/CI use; B2M returns a publishable key for the browser and
    /// requires at least one `--allowed-origin`. The key is shown
    /// EXACTLY ONCE — store it immediately.
    Create {
        /// Client type: `m2m` (machine-to-machine secret, default) or
        /// `b2m` (browser-to-machine publishable key).
        #[arg(long = "type", value_enum, default_value_t = ClientType::M2m)]
        client_type: ClientType,
        /// Allowed origin for a B2M client (repeatable). Required for
        /// `--type b2m`, ignored for m2m — e.g. `--allowed-origin
        /// https://app.example.com --allowed-origin https://example.com`.
        #[arg(long = "allowed-origin", value_name = "ORIGIN")]
        allowed_origins: Vec<String>,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Raw JSON body escape hatch — a file path, `-` for stdin, or
        /// inline JSON. Overrides `--type`/`--allowed-origin` when given.
        #[arg(long)]
        body: Option<String>,
        /// Emit the raw JSON response (still contains the key once).
        #[arg(long)]
        json: bool,
    },
    /// Update a B2M client's allowed origins (replaces the whole list).
    /// Only browser-to-machine clients have origins — updating an M2M
    /// client is rejected by the API.
    Update {
        /// The client id from `th api keys list`.
        client_id: String,
        /// New allowed origin (repeatable). Replaces the existing list;
        /// at least one is required.
        #[arg(long = "allowed-origin", value_name = "ORIGIN")]
        allowed_origins: Vec<String>,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the pretty summary.
        #[arg(long)]
        json: bool,
    },
    /// Revoke (delete) an auth client. The key stops working
    /// immediately and the action is irreversible — mint a new client
    /// to replace it (or use `rotate`).
    Revoke {
        /// The client id from `th api keys list`.
        client_id: String,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the confirmation line.
        #[arg(long)]
        json: bool,
    },
    /// Rotate an auth client: mint a fresh client of the SAME type (and
    /// same allowed origins for B2M), then revoke the old one. The
    /// backend has no in-place rotation, so the new client has a NEW
    /// client id + key (shown once) — update every consumer. The new
    /// client is created BEFORE the old is revoked, so a failure leaves
    /// the existing key working.
    Rotate {
        /// The client id to rotate, from `th api keys list`.
        client_id: String,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response (still contains the new key once).
        #[arg(long)]
        json: bool,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = UserClient::from_user_session()?;
    match cmd {
        Cmd::List { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client.get(&format!("/organizations/{org}/auth-clients")).await.context("GET auth-clients")?;
            if json {
                print_json(&resp);
            } else {
                print_clients(&resp);
            }
        }
        Cmd::Create {
            client_type,
            allowed_origins,
            org_id,
            body,
            json,
        } => {
            let org = crate::active_org::resolve(org_id)?;
            let payload = match body {
                Some(b) => super::read_body(&b)?,
                None => build_create_body(client_type, &allowed_origins)?,
            };
            let resp = client
                .post(&format!("/organizations/{org}/auth-clients"), &payload)
                .await
                .context("POST auth-client")?;
            if json {
                print_json(&resp);
            } else {
                print_created(&resp, "auth client created");
            }
        }
        Cmd::Update {
            client_id,
            allowed_origins,
            org_id,
            json,
        } => {
            let org = crate::active_org::resolve(org_id)?;
            if allowed_origins.is_empty() {
                bail!("at least one --allowed-origin is required (it replaces the client's full origin list)");
            }
            let payload = json!({ "allowedOrigins": allowed_origins });
            let resp = client
                .patch(&format!("/organizations/{org}/auth-clients/{client_id}"), &payload)
                .await
                .context("PATCH auth-client")?;
            if json {
                print_json(&resp);
            } else {
                println!();
                println!("  {} updated allowed origins for {}", "✓".green(), client_id.bold());
                print_origins(&resp);
                println!();
            }
        }
        Cmd::Revoke { client_id, org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .delete(&format!("/organizations/{org}/auth-clients/{client_id}"))
                .await
                .context("DELETE auth-client")?;
            if json {
                print_json(&resp);
            } else {
                println!();
                println!("  {} revoked client {}", "✓".green(), client_id.bold());
                println!();
            }
        }
        Cmd::Rotate { client_id, org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            rotate(&client, &org, &client_id, json).await?;
        }
    }
    Ok(())
}

/// Build the create payload from `--type` + `--allowed-origin`,
/// enforcing the B2M origin requirement client-side with a clear
/// message (the API would otherwise 400).
fn build_create_body(client_type: ClientType, origins: &[String]) -> Result<Value> {
    match client_type {
        ClientType::M2m => Ok(json!({ "type": client_type.api_value() })),
        ClientType::B2m => {
            if origins.is_empty() {
                bail!("--type b2m requires at least one --allowed-origin (browser clients are origin-restricted)");
            }
            Ok(json!({ "type": client_type.api_value(), "allowedOrigins": origins }))
        }
    }
}

/// Rotate: mint a replacement of the same type/origins, then revoke the
/// old client. Reads the existing client from the list (the API has no
/// single-client GET) to carry over type + origins.
async fn rotate(client: &UserClient, org: &str, client_id: &str, json: bool) -> Result<()> {
    let list = client
        .get(&format!("/organizations/{org}/auth-clients"))
        .await
        .context("GET auth-clients (for rotate)")?;
    let items = list.as_array().cloned().unwrap_or_default();
    let existing = items
        .iter()
        .find(|c| c.get("clientId").and_then(Value::as_str) == Some(client_id))
        .ok_or_else(|| anyhow::anyhow!("no auth client with id {client_id} in this org — check `th api keys list`"))?;

    let is_b2m = existing.get("type").and_then(Value::as_str) == Some("browser-to-machine");
    let payload = if is_b2m {
        let origins: Vec<String> = existing
            .get("allowedOrigins")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if origins.is_empty() {
            bail!("existing browser-to-machine client {client_id} has no allowed origins to carry over");
        }
        json!({ "type": "browser-to-machine", "allowedOrigins": origins })
    } else {
        json!({ "type": "machine-to-machine" })
    };

    // Create the replacement FIRST so a failure leaves the old key valid.
    let created = client
        .post(&format!("/organizations/{org}/auth-clients"), &payload)
        .await
        .context("POST replacement auth-client during rotate")?;
    client
        .delete(&format!("/organizations/{org}/auth-clients/{client_id}"))
        .await
        .context("DELETE old auth-client after rotate (the replacement is live; revoke the old one manually if this failed)")?;

    if json {
        print_json(&created);
    } else {
        println!();
        println!("  {} rotated — old client {} revoked", "✓".green().bold(), client_id.dimmed());
        print_created(&created, "replacement");
        println!("  {} the client id AND key both changed — update consumers", "→".dimmed());
        println!();
    }
    Ok(())
}

/// Print a freshly-minted client. The key value (secret for M2M, public
/// for B2M) is returned once, so make it prominent.
fn print_created(resp: &Value, heading: &str) {
    let cid = resp.get("clientId").and_then(Value::as_str).unwrap_or("?");
    let ty = resp.get("type").and_then(Value::as_str).unwrap_or("");
    println!();
    println!("  {} {heading} — key shown once, store it now:", "✓".green().bold());
    println!();
    println!("    {} {}", "client id".dimmed(), cid.bold());
    if !ty.is_empty() {
        println!("    {} {}", "type".dimmed(), ty);
    }
    if let Some(sk) = resp.get("secretKey").and_then(Value::as_str) {
        println!("    {} {}", "secret key".dimmed(), sk.bold());
    }
    if let Some(pk) = resp.get("publicKey").and_then(Value::as_str) {
        println!("    {} {}", "public key".dimmed(), pk.bold());
    }
    if let Some(exp) = resp.get("expiresAt").and_then(Value::as_str) {
        println!("    {} {}", "expires".dimmed(), exp);
    }
    println!();
}

/// Print the `allowedOrigins` of an update response, one per line.
fn print_origins(resp: &Value) {
    if let Some(origins) = resp.get("allowedOrigins").and_then(Value::as_array) {
        for o in origins {
            if let Some(s) = o.as_str() {
                println!("    {} {}", "•".dimmed(), s);
            }
        }
    }
}

/// Pretty-print the bare-array list of auth clients.
fn print_clients(resp: &Value) {
    println!();
    let Some(items) = resp.as_array() else {
        print_json(resp);
        return;
    };
    if items.is_empty() {
        println!("  {} {}", "●".dimmed(), "no auth clients".dimmed());
        println!();
        return;
    }
    for c in items {
        let cid = c.get("clientId").and_then(Value::as_str).unwrap_or("?");
        let ty = c.get("type").and_then(Value::as_str).unwrap_or("?");
        let short = match ty {
            "machine-to-machine" => "m2m",
            "browser-to-machine" => "b2m",
            other => other,
        };
        println!("  {} {}  {}", "○".dimmed(), cid.bold(), short.dimmed());
        if let Some(origins) = c.get("allowedOrigins").and_then(Value::as_array) {
            for o in origins {
                if let Some(s) = o.as_str() {
                    println!("      {} {}", "•".dimmed(), s.dimmed());
                }
            }
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m2m_body_has_no_origins() {
        let body = build_create_body(ClientType::M2m, &[]).expect("m2m needs no origins");
        assert_eq!(body["type"], "machine-to-machine");
        assert!(body.get("allowedOrigins").is_none());
    }

    #[test]
    fn b2m_requires_origins() {
        assert!(build_create_body(ClientType::B2m, &[]).is_err(), "b2m with no origins must error");
        let body = build_create_body(ClientType::B2m, &["https://a.example.com".into(), "https://b.example.com".into()]).expect("b2m with origins");
        assert_eq!(body["type"], "browser-to-machine");
        assert_eq!(body["allowedOrigins"], json!(["https://a.example.com", "https://b.example.com"]));
    }

    #[test]
    fn client_type_api_values() {
        assert_eq!(ClientType::M2m.api_value(), "machine-to-machine");
        assert_eq!(ClientType::B2m.api_value(), "browser-to-machine");
    }
}

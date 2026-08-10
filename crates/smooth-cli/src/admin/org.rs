//! `th admin org *` — list / show / create / member ops / product ops.
//!
//! Every subcommand accepts `--json` for raw JSON; default is a
//! pretty table.

use anstream::{eprintln, println};
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::json;

use super::client::{print_ok, AdminClient};
use super::render::{render, Format, TableOptions};

#[derive(Debug, Subcommand)]
pub enum OrgCommands {
    /// List organizations. Paginated, searchable.
    List {
        /// Substring filter on org name.
        #[arg(long)]
        search: Option<String>,
        /// Max rows (server caps at 50 by default).
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Pagination offset.
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[arg(long)]
        json: bool,
    },
    /// Show one org with its members + products inline.
    Show {
        /// Org UUID.
        org_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Create a new organization. The caller is auto-added as an
    /// admin member by a DB trigger.
    Create {
        /// Organization display name.
        #[arg(long)]
        name: String,
        /// Optional parent org UUID to link the new org under (a `manages`
        /// relationship — the client-portal parent/child model). One-shot
        /// equivalent of `create` then `link-child`.
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List members of an org with their roles.
    Members {
        /// Org UUID.
        org_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Add a user as a member of an org (bypasses invitation).
    AddMember {
        /// Org UUID.
        org_id: String,
        /// User UUID to add.
        #[arg(long)]
        user_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove a member from an org.
    RemoveMember {
        /// Org UUID.
        org_id: String,
        /// User UUID to remove.
        #[arg(long)]
        user_id: String,
        #[arg(long)]
        json: bool,
    },
    /// List products active on an org.
    Products {
        /// Org UUID.
        org_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Activate a product on an org (creates a bypass-Stripe order).
    ActivateProduct {
        /// Org UUID.
        org_id: String,
        /// Stripe product name (e.g. "Smoo AI CRM").
        #[arg(long)]
        product: String,
        #[arg(long)]
        json: bool,
    },
    /// Revoke a product (sets status='cancelled').
    RevokeProduct {
        /// Org UUID.
        org_id: String,
        /// Product UUID.
        #[arg(long)]
        product_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Extend an active product's trial period by N days.
    ExtendTrial {
        /// Org UUID.
        org_id: String,
        /// Product UUID.
        #[arg(long)]
        product_id: String,
        /// Days to extend.
        #[arg(long)]
        days: u32,
        #[arg(long)]
        json: bool,
    },
    /// Mint an auth client (API key) for an org. The secret/public key is
    /// returned ONCE — capture it immediately (the server stores only a hash).
    /// SMOODEV-1950.
    MintClient {
        /// Org UUID.
        org_id: String,
        /// `m2m` (server-to-server secretKey) or `b2m` (browser-to-machine,
        /// CORS-locked publicKey safe to embed in browser JS).
        #[arg(long, value_parser = ["m2m", "b2m"])]
        kind: String,
        /// B2M only: a CORS-allowed origin (e.g. `https://customer.com`).
        /// Repeatable; at least one is required for `--kind b2m`.
        #[arg(long = "allowed-origin")]
        allowed_origin: Vec<String>,
        /// Optional ISO-8601 expiry (must be in the future; defaults to +1 year).
        #[arg(long)]
        expires_at: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Get or set an org's Social Command Center (SCC) tier. Omit `--set` to
    /// read the current tier. SMOODEV-1950.
    SccTier {
        /// Org UUID.
        org_id: String,
        /// Set the tier: none | pilot | standard | enterprise. Omit to read.
        #[arg(long, value_parser = ["none", "pilot", "standard", "enterprise"])]
        set: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Link a child org under a parent org (creates an organization
    /// relationship — the client-portal parent/child model). The parent
    /// defaults to the active org, so from the master org this is just
    /// `th admin org link-child <child-org-id>`.
    LinkChild {
        /// Child org UUID to link under the parent.
        child_org_id: String,
        /// Parent org UUID. Defaults to the active org.
        #[arg(long)]
        parent: Option<String>,
        /// Relationship type. `manages` is the platform's parent-management
        /// convention (what the client portal uses).
        #[arg(long = "type", default_value = "manages")]
        relationship_type: String,
        #[arg(long)]
        json: bool,
    },
    /// Unlink a child org (deletes the matching organization relationship).
    UnlinkChild {
        /// Child org UUID to unlink from the parent.
        child_org_id: String,
        /// Parent org UUID. Defaults to the active org.
        #[arg(long)]
        parent: Option<String>,
        /// Relationship type to remove (must match how it was linked).
        #[arg(long = "type", default_value = "manages")]
        relationship_type: String,
        #[arg(long)]
        json: bool,
    },
    /// List an org's child orgs. Defaults to the active org.
    Children {
        /// Parent org UUID. Defaults to the active org.
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

pub async fn dispatch(cmd: OrgCommands) -> Result<()> {
    let client = AdminClient::from_user_session().await?;
    match cmd {
        OrgCommands::List { search, limit, offset, json } => {
            let mut path = format!("/admin/organizations?limit={limit}&offset={offset}");
            if let Some(q) = search {
                path.push_str(&format!("&search={}", urlencoding::encode(&q)));
            }
            let body = client.get(&path).await?;
            render(
                &body,
                Format::from_flag(json),
                &TableOptions::default()
                    .with_label("organizations")
                    .with_columns(&["id", "name", "memberCount", "createdAt"]),
            );
        }
        OrgCommands::Show { org_id, json } => {
            let members = client.get(&format!("/admin/organizations/{org_id}/members")).await?;
            let products = client.get(&format!("/admin/organizations/{org_id}/products")).await?;
            if json {
                render(&json!({ "members": members, "products": products }), Format::Json, &TableOptions::default());
            } else {
                println!("{} {org_id}", "Org:".bold().cyan());
                println!();
                render(
                    &members,
                    Format::Table,
                    &TableOptions::default()
                        .with_label("members")
                        .with_columns(&["id", "email", "fullName", "role", "createdAt"]),
                );
                println!();
                render(
                    &products,
                    Format::Table,
                    &TableOptions::default()
                        .with_label("products")
                        .with_columns(&["id", "stripeProductId", "status", "createdAt"]),
                );
            }
        }
        OrgCommands::Create { name, parent, json } => {
            // SMOODEV-1937: the endpoint requires `createdBy` as well as `name`.
            // It's the calling user (the DB trigger adds them as an admin member).
            let created_by = client.user_id().context("derive createdBy from session")?;
            let body = client.post("/admin/organizations", &json!({ "name": name, "createdBy": created_by })).await?;
            print_ok(format!("created org \"{name}\""));

            // Optional one-shot parent link (same `manages` relationship LinkChild
            // creates) so onboarding a child org is a single command.
            if let Some(parent) = parent {
                let child_id = body.get("id").and_then(|v| v.as_str()).context("new org id missing from create response")?;
                client
                    .post(
                        &format!("/organizations/{parent}/relationships"),
                        &json!({ "childOrgId": child_id, "relationshipType": "manages" }),
                    )
                    .await
                    .with_context(|| format!("link new org under parent {parent}"))?;
                print_ok(format!("linked under parent {parent} (manages)"));
            }

            render(&body, Format::from_flag(json), &TableOptions::default());
        }
        OrgCommands::Members { org_id, json } => {
            let body = client.get(&format!("/admin/organizations/{org_id}/members")).await?;
            render(
                &body,
                Format::from_flag(json),
                &TableOptions::default()
                    .with_label("members")
                    .with_columns(&["id", "email", "fullName", "role", "createdAt"]),
            );
        }
        OrgCommands::AddMember { org_id, user_id, json } => {
            // SMOODEV-1937: endpoint expects camelCase `userId`, not `user_id`.
            let body = client
                .post(&format!("/admin/organizations/{org_id}/members"), &json!({ "userId": user_id }))
                .await?;
            print_ok(format!("added {user_id} to {org_id}"));
            render(&body, Format::from_flag(json), &TableOptions::default());
        }
        OrgCommands::RemoveMember { org_id, user_id, json } => {
            let body = client.delete(&format!("/admin/organizations/{org_id}/members/{user_id}")).await?;
            print_ok(format!("removed {user_id} from {org_id}"));
            render(&body, Format::from_flag(json), &TableOptions::default());
        }
        OrgCommands::Products { org_id, json } => {
            let body = client.get(&format!("/admin/organizations/{org_id}/products")).await?;
            render(
                &body,
                Format::from_flag(json),
                &TableOptions::default()
                    .with_label("products")
                    .with_columns(&["id", "stripeProductId", "status", "createdAt"]),
            );
        }
        OrgCommands::ActivateProduct { org_id, product, json } => {
            let body = client
                .post(&format!("/admin/organizations/{org_id}/products"), &json!({ "productName": product }))
                .await?;
            print_ok(format!("activated \"{product}\" on {org_id}"));
            render(&body, Format::from_flag(json), &TableOptions::default());
        }
        OrgCommands::RevokeProduct { org_id, product_id, json } => {
            let body = client.delete(&format!("/admin/organizations/{org_id}/products/{product_id}")).await?;
            print_ok(format!("revoked product {product_id} on {org_id}"));
            render(&body, Format::from_flag(json), &TableOptions::default());
        }
        OrgCommands::ExtendTrial {
            org_id,
            product_id,
            days,
            json,
        } => {
            let body = client
                .post(
                    &format!("/admin/organizations/{org_id}/products/{product_id}/extend-trial"),
                    &json!({ "days": days }),
                )
                .await?;
            print_ok(format!("extended trial on {product_id} ({org_id}) by {days} days"));
            render(&body, Format::from_flag(json), &TableOptions::default());
        }
        OrgCommands::MintClient {
            org_id,
            kind,
            allowed_origin,
            expires_at,
            json,
        } => {
            let mut payload = serde_json::Map::new();
            if let Some(exp) = expires_at {
                payload.insert("expiresAt".into(), json!(exp));
            }
            if kind == "b2m" {
                if allowed_origin.is_empty() {
                    anyhow::bail!("`--allowed-origin` is required for `--kind b2m` (pass one or more CORS-allowed origins)");
                }
                payload.insert("allowedOrigins".into(), json!(allowed_origin));
            } else if !allowed_origin.is_empty() {
                // m2m has no origin allowlist — flag the misuse rather than silently dropping it.
                anyhow::bail!("`--allowed-origin` only applies to `--kind b2m`");
            }
            let body = client
                .post(&format!("/admin/organizations/{org_id}/clients/{kind}"), &serde_json::Value::Object(payload))
                .await?;
            // The secret/public key is in the response and is shown ONCE — render
            // the full JSON (never a truncating table) so it can be captured.
            eprintln!("{}", "⚠  The secret is returned ONCE and cannot be recovered — capture it now.".yellow());
            print_ok(format!("minted {kind} client for {org_id}"));
            render(&body, Format::Json, &TableOptions::default());
            let _ = json; // output is always full JSON for safety
        }
        OrgCommands::SccTier { org_id, set, json } => {
            let path = format!("/admin/organizations/{org_id}/billing/scc-tier");
            let body = if let Some(tier) = set {
                let body = client.post(&path, &json!({ "tier": tier })).await?;
                print_ok(format!("set SCC tier = {tier} on {org_id}"));
                body
            } else {
                client.get(&path).await?
            };
            render(&body, Format::from_flag(json), &TableOptions::default().with_columns(&["orgId", "tier"]));
        }
        OrgCommands::LinkChild {
            child_org_id,
            parent,
            relationship_type,
            json,
        } => {
            // These are the platform's user-JWT relationship endpoints (not
            // /admin/*) — a parent-org admin's session is authorized for them.
            let parent = crate::active_org::resolve(parent).context("resolve parent org (pass --parent or set an active org)")?;
            let body = client
                .post(
                    &format!("/organizations/{parent}/relationships"),
                    &json!({ "childOrgId": child_org_id, "relationshipType": relationship_type }),
                )
                .await?;
            print_ok(format!("linked {child_org_id} under {parent} ({relationship_type})"));
            render(
                &body,
                Format::from_flag(json),
                &TableOptions::default().with_columns(&["id", "parentOrgId", "childOrgId", "relationshipType", "status"]),
            );
        }
        OrgCommands::UnlinkChild {
            child_org_id,
            parent,
            relationship_type,
            json,
        } => {
            let parent = crate::active_org::resolve(parent).context("resolve parent org (pass --parent or set an active org)")?;
            let rows = client.get(&format!("/organizations/{parent}/relationships")).await?;
            let Some(rel_id) = find_relationship_id(&rows, &child_org_id, &relationship_type) else {
                anyhow::bail!("no `{relationship_type}` relationship from {parent} to {child_org_id} — see `th admin org children`");
            };
            let body = client.delete(&format!("/organizations/{parent}/relationships/{rel_id}")).await?;
            print_ok(format!("unlinked {child_org_id} from {parent} (relationship {rel_id})"));
            render(&body, Format::from_flag(json), &TableOptions::default());
        }
        OrgCommands::Children { parent, json } => {
            let parent = crate::active_org::resolve(parent).context("resolve parent org (pass --parent or set an active org)")?;
            let body = client.get(&format!("/organizations/{parent}/children")).await?;
            render(
                &body,
                Format::from_flag(json),
                &TableOptions::default().with_label("children").with_columns(&["id", "name", "createdAt"]),
            );
        }
    }
    Ok(())
}

/// Find the relationship id for `child` with `relationship_type` in the
/// GET /organizations/{parent}/relationships response. Tolerates both a bare
/// array and `{ "relationships": [...] }` / `{ "data": [...] }` envelopes.
fn find_relationship_id(rows: &serde_json::Value, child: &str, relationship_type: &str) -> Option<String> {
    let arr = rows
        .as_array()
        .or_else(|| rows.get("relationships").and_then(|v| v.as_array()))
        .or_else(|| rows.get("data").and_then(|v| v.as_array()))?;
    arr.iter()
        .find(|r| r.get("childOrgId").and_then(|v| v.as_str()) == Some(child) && r.get("relationshipType").and_then(|v| v.as_str()) == Some(relationship_type))
        .and_then(|r| r.get("id").and_then(|v| v.as_str()))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> serde_json::Value {
        json!([
            { "id": "rel-1", "childOrgId": "child-a", "relationshipType": "manages" },
            { "id": "rel-2", "childOrgId": "child-a", "relationshipType": "subaccount" },
            { "id": "rel-3", "childOrgId": "child-b", "relationshipType": "manages" },
        ])
    }

    #[test]
    fn finds_the_matching_child_and_type() {
        assert_eq!(find_relationship_id(&rows(), "child-a", "manages").as_deref(), Some("rel-1"));
        assert_eq!(find_relationship_id(&rows(), "child-a", "subaccount").as_deref(), Some("rel-2"));
        assert_eq!(find_relationship_id(&rows(), "child-b", "manages").as_deref(), Some("rel-3"));
    }

    #[test]
    fn misses_return_none() {
        assert!(find_relationship_id(&rows(), "child-c", "manages").is_none());
        assert!(find_relationship_id(&rows(), "child-b", "subaccount").is_none());
    }

    #[test]
    fn tolerates_enveloped_responses() {
        let enveloped = json!({ "relationships": rows() });
        assert_eq!(find_relationship_id(&enveloped, "child-b", "manages").as_deref(), Some("rel-3"));
        let data = json!({ "data": rows() });
        assert_eq!(find_relationship_id(&data, "child-a", "manages").as_deref(), Some("rel-1"));
        assert!(find_relationship_id(&json!({ "nope": true }), "child-a", "manages").is_none());
    }
}

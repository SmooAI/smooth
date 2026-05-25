//! `th admin org *` — list / show / create / member ops / product ops.
//!
//! Every subcommand accepts `--json` for raw JSON; default is a
//! pretty table.

use anyhow::Result;
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
}

pub async fn dispatch(cmd: OrgCommands) -> Result<()> {
    let client = AdminClient::from_user_session()?;
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
        OrgCommands::Create { name, json } => {
            let body = client.post("/admin/organizations", &json!({ "name": name })).await?;
            print_ok(format!("created org \"{name}\""));
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
            let body = client
                .post(&format!("/admin/organizations/{org_id}/members"), &json!({ "user_id": user_id }))
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
                .post(&format!("/admin/organizations/{org_id}/products"), &json!({ "product_name": product }))
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
    }
    Ok(())
}

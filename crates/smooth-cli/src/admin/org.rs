//! `th admin org *` — list / show / create / member ops / product ops.

use anyhow::Result;
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::json;

use super::client::{print_json, print_ok, AdminClient};

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
    },
    /// Show one org with its members + products inline.
    Show {
        /// Org UUID.
        org_id: String,
    },
    /// Create a new organization. The caller is auto-added as an
    /// admin member by a DB trigger.
    Create {
        /// Organization display name.
        #[arg(long)]
        name: String,
    },
    /// List members of an org with their roles.
    Members {
        /// Org UUID.
        org_id: String,
    },
    /// Add a user as a member of an org (bypasses invitation).
    AddMember {
        /// Org UUID.
        org_id: String,
        /// User UUID to add.
        #[arg(long)]
        user_id: String,
    },
    /// Remove a member from an org.
    RemoveMember {
        /// Org UUID.
        org_id: String,
        /// User UUID to remove.
        #[arg(long)]
        user_id: String,
    },
    /// List products active on an org.
    Products {
        /// Org UUID.
        org_id: String,
    },
    /// Activate a product on an org (creates a bypass-Stripe order).
    ActivateProduct {
        /// Org UUID.
        org_id: String,
        /// Stripe product name (e.g. "Smoo AI CRM").
        #[arg(long)]
        product: String,
    },
    /// Revoke a product (sets status='cancelled').
    RevokeProduct {
        /// Org UUID.
        org_id: String,
        /// Product UUID.
        #[arg(long)]
        product_id: String,
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
    },
}

pub async fn dispatch(cmd: OrgCommands) -> Result<()> {
    let client = AdminClient::from_user_session()?;
    match cmd {
        OrgCommands::List { search, limit, offset } => {
            let mut path = format!("/admin/organizations?limit={limit}&offset={offset}");
            if let Some(q) = search {
                path.push_str(&format!("&search={}", urlencoding::encode(&q)));
            }
            let body = client.get(&path).await?;
            print_json(&body);
        }
        OrgCommands::Show { org_id } => {
            let members = client.get(&format!("/admin/organizations/{org_id}/members")).await?;
            let products = client.get(&format!("/admin/organizations/{org_id}/products")).await?;
            println!("{} {org_id}", "Org:".bold().cyan());
            println!();
            println!("{}", "Members:".bold().cyan());
            print_json(&members);
            println!();
            println!("{}", "Products:".bold().cyan());
            print_json(&products);
        }
        OrgCommands::Create { name } => {
            let body = client.post("/admin/organizations", &json!({ "name": name })).await?;
            print_ok(format!("created org \"{name}\""));
            print_json(&body);
        }
        OrgCommands::Members { org_id } => {
            let body = client.get(&format!("/admin/organizations/{org_id}/members")).await?;
            print_json(&body);
        }
        OrgCommands::AddMember { org_id, user_id } => {
            let body = client
                .post(&format!("/admin/organizations/{org_id}/members"), &json!({ "user_id": user_id }))
                .await?;
            print_ok(format!("added {user_id} to {org_id}"));
            print_json(&body);
        }
        OrgCommands::RemoveMember { org_id, user_id } => {
            let body = client.delete(&format!("/admin/organizations/{org_id}/members/{user_id}")).await?;
            print_ok(format!("removed {user_id} from {org_id}"));
            print_json(&body);
        }
        OrgCommands::Products { org_id } => {
            let body = client.get(&format!("/admin/organizations/{org_id}/products")).await?;
            print_json(&body);
        }
        OrgCommands::ActivateProduct { org_id, product } => {
            let body = client
                .post(&format!("/admin/organizations/{org_id}/products"), &json!({ "product_name": product }))
                .await?;
            print_ok(format!("activated \"{product}\" on {org_id}"));
            print_json(&body);
        }
        OrgCommands::RevokeProduct { org_id, product_id } => {
            let body = client.delete(&format!("/admin/organizations/{org_id}/products/{product_id}")).await?;
            print_ok(format!("revoked product {product_id} on {org_id}"));
            print_json(&body);
        }
        OrgCommands::ExtendTrial { org_id, product_id, days } => {
            let body = client
                .post(
                    &format!("/admin/organizations/{org_id}/products/{product_id}/extend-trial"),
                    &json!({ "days": days }),
                )
                .await?;
            print_ok(format!("extended trial on {product_id} ({org_id}) by {days} days"));
            print_json(&body);
        }
    }
    Ok(())
}

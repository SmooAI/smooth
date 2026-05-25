//! `th admin user *` — list / search / magic-link / roles.

use anyhow::Result;
use clap::Subcommand;
use serde_json::json;

use super::client::{print_json, print_ok, AdminClient};

#[derive(Debug, Subcommand)]
pub enum UserCommands {
    /// List users (paginated, sortable). Calls `GET /admin/users`.
    List {
        /// Max rows to return (server default applies if omitted).
        #[arg(long)]
        limit: Option<u32>,
        /// Server-side cursor (from the previous page's response).
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Search users by email / display name. Min 3 chars.
    Search {
        /// Query string (substring match).
        query: String,
        /// Max results to return (server caps at 20).
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Get the admin role set assigned to a user.
    Roles {
        /// User UUID.
        user_id: String,
    },
    /// Set the admin role set on a user (replaces existing roles).
    SetRoles {
        /// User UUID.
        user_id: String,
        /// Comma-separated role names. Pass empty to revoke all.
        #[arg(long)]
        roles: String,
    },
    /// Mint a one-time Supabase magic link for the given email.
    /// Useful for issuing a passwordless login to an internal user
    /// or for setting an initial password.
    MagicLink {
        /// Target user email.
        #[arg(long)]
        email: String,
    },
}

pub async fn dispatch(cmd: UserCommands) -> Result<()> {
    let client = AdminClient::from_user_session()?;
    match cmd {
        UserCommands::List { limit, cursor } => {
            let mut path = String::from("/admin/users?");
            if let Some(n) = limit {
                path.push_str(&format!("limit={n}&"));
            }
            if let Some(c) = cursor {
                path.push_str(&format!("cursor={c}&"));
            }
            let body = client.get(path.trim_end_matches(['?', '&'])).await?;
            print_json(&body);
        }
        UserCommands::Search { query, limit } => {
            let path = format!("/admin/users/search?q={}&limit={limit}", urlencoding::encode(&query));
            let body = client.get(&path).await?;
            print_json(&body);
        }
        UserCommands::Roles { user_id } => {
            let body = client.get(&format!("/admin/users/{user_id}/roles")).await?;
            print_json(&body);
        }
        UserCommands::SetRoles { user_id, roles } => {
            let role_vec: Vec<String> = roles.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
            let body = client.put(&format!("/admin/users/{user_id}/roles"), &json!({ "roles": role_vec })).await?;
            print_ok(format!("set roles on {user_id} → {}", role_vec.join(", ")));
            print_json(&body);
        }
        UserCommands::MagicLink { email } => {
            let body = client.post("/admin/magic-link", &json!({ "email": email })).await?;
            print_ok(format!("magic link minted for {email}"));
            print_json(&body);
        }
    }
    Ok(())
}

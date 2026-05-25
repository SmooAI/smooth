//! `th admin user *` — list / search / magic-link / roles.
//!
//! Every subcommand accepts `--json` for raw JSON; default is a
//! pretty table.

use anyhow::Result;
use clap::Subcommand;
use serde_json::json;

use super::client::{print_ok, AdminClient};
use super::render::{render, Format, TableOptions};

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
        /// Emit raw JSON instead of a pretty table.
        #[arg(long)]
        json: bool,
    },
    /// Search users by email / display name. Min 3 chars.
    Search {
        /// Query string (substring match).
        query: String,
        /// Max results to return (server caps at 20).
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Get the admin role set assigned to a user.
    Roles {
        /// User UUID.
        user_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Set the admin role set on a user (replaces existing roles).
    SetRoles {
        /// User UUID.
        user_id: String,
        /// Comma-separated role names. Pass empty to revoke all.
        #[arg(long)]
        roles: String,
        #[arg(long)]
        json: bool,
    },
    /// Mint a one-time Supabase magic link for the given email.
    /// Useful for issuing a passwordless login to an internal user
    /// or for setting an initial password.
    MagicLink {
        /// Target user email.
        #[arg(long)]
        email: String,
        #[arg(long)]
        json: bool,
    },
}

pub async fn dispatch(cmd: UserCommands) -> Result<()> {
    let client = AdminClient::from_user_session()?;
    match cmd {
        UserCommands::List { limit, cursor, json } => {
            let mut path = String::from("/admin/users?");
            if let Some(n) = limit {
                path.push_str(&format!("limit={n}&"));
            }
            if let Some(c) = cursor {
                path.push_str(&format!("cursor={c}&"));
            }
            let body = client.get(path.trim_end_matches(['?', '&'])).await?;
            render(
                &body,
                Format::from_flag(json),
                &TableOptions::default()
                    .with_label("users")
                    .with_columns(&["id", "email", "full_name", "created_at"]),
            );
        }
        UserCommands::Search { query, limit, json } => {
            let path = format!("/admin/users/search?q={}&limit={limit}", urlencoding::encode(&query));
            let body = client.get(&path).await?;
            render(
                &body,
                Format::from_flag(json),
                &TableOptions::default().with_label("matches").with_columns(&["id", "email", "full_name"]),
            );
        }
        UserCommands::Roles { user_id, json } => {
            let body = client.get(&format!("/admin/users/{user_id}/roles")).await?;
            render(&body, Format::from_flag(json), &TableOptions::default().with_label("roles"));
        }
        UserCommands::SetRoles { user_id, roles, json } => {
            let role_vec: Vec<String> = roles.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
            let body = client.put(&format!("/admin/users/{user_id}/roles"), &json!({ "roles": role_vec })).await?;
            print_ok(format!("set roles on {user_id} → {}", role_vec.join(", ")));
            render(&body, Format::from_flag(json), &TableOptions::default());
        }
        UserCommands::MagicLink { email, json } => {
            let body = client.post("/admin/magic-link", &json!({ "email": email })).await?;
            print_ok(format!("magic link minted for {email}"));
            render(&body, Format::from_flag(json), &TableOptions::default());
        }
    }
    Ok(())
}

//! `th api teams …` — organization teams (RBAC groupings), SMOODEV-2645 / ADR-091.
//!
//! A team is a named grouping of org members that also holds roles; members
//! inherit the team's role permissions. Backed by
//! `/organizations/:org_id/teams` (reads need org membership, writes need the
//! `org.teams.manage` permission). Authenticated as the logged-in *user*
//! (`th auth login`) via [`UserClient`], same as the CRM commands, so a real
//! person's permissions are checked.
//!
//! `set-members` / `set-roles` are replace-all — the ids you pass become the
//! team's complete membership / role set. Members resolve from email (or a
//! member-id uuid); roles resolve from role name (or a role-id uuid).

use anstream::println;
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::print_json;
use crate::smooai::crm::resolve_org;
use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's teams with member and role counts.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Create a team.
    Create {
        /// Team name.
        name: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Optional description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Rename a team (resolved by name or id).
    Rename {
        /// The team to rename (name or id).
        team: String,
        /// The new team name.
        new_name: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a team (resolved by name or id).
    Delete {
        /// The team to delete (name or id).
        team: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Replace a team's members (pass emails or member ids — replace-all).
    SetMembers {
        /// The team (name or id).
        team: String,
        /// Members to set — each an email or a member-id uuid.
        members: Vec<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Replace a team's roles (pass role names or role ids — replace-all).
    SetRoles {
        /// The team (name or id).
        team: String,
        /// Roles to set — each a role name or a role-id uuid.
        roles: Vec<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        Cmd::List { org, json } => {
            let org = resolve_org(org)?;
            let body = client.get(&format!("/organizations/{org}/teams")).await.context("GET teams")?;
            if json {
                print_json(&body);
            } else {
                render_teams(&body);
            }
        }
        Cmd::Create { name, org, description } => {
            let org = resolve_org(org)?;
            let mut body = json!({ "name": name });
            if let Some(d) = description.filter(|s| !s.trim().is_empty()) {
                body["description"] = json!(d);
            }
            let r = client.post(&format!("/organizations/{org}/teams"), &body).await.context("POST team")?;
            let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
            println!("  {} created team {} {}", "✚".green(), id.dimmed(), name.bold());
        }
        Cmd::Rename { team, new_name, org } => {
            let org = resolve_org(org)?;
            let id = resolve_team_id(&client, &org, &team).await?;
            client
                .patch(&format!("/organizations/{org}/teams/{id}"), &json!({ "name": new_name }))
                .await
                .context("PATCH team")?;
            println!("  {} renamed team {} → {}", "↻".yellow(), id.dimmed(), new_name.bold());
        }
        Cmd::Delete { team, org } => {
            let org = resolve_org(org)?;
            let id = resolve_team_id(&client, &org, &team).await?;
            client.delete(&format!("/organizations/{org}/teams/{id}")).await.context("DELETE team")?;
            println!("  {} deleted team {}", "🗑".red(), id.dimmed());
        }
        Cmd::SetMembers { team, members, org } => {
            let org = resolve_org(org)?;
            let id = resolve_team_id(&client, &org, &team).await?;
            let member_ids = resolve_member_ids(&client, &org, &members).await?;
            client
                .put(&format!("/organizations/{org}/teams/{id}/members"), &json!({ "memberIds": member_ids }))
                .await
                .context("PUT team members")?;
            println!(
                "  {} team {} now has {} member(s)",
                "✓".green(),
                id.dimmed(),
                member_ids.len().to_string().bold()
            );
        }
        Cmd::SetRoles { team, roles, org } => {
            let org = resolve_org(org)?;
            let id = resolve_team_id(&client, &org, &team).await?;
            let role_ids = resolve_role_ids(&client, &org, &roles).await?;
            client
                .put(&format!("/organizations/{org}/teams/{id}/roles"), &json!({ "roleIds": role_ids }))
                .await
                .context("PUT team roles")?;
            println!("  {} team {} now has {} role(s)", "✓".green(), id.dimmed(), role_ids.len().to_string().bold());
        }
    }
    Ok(())
}

/// Resolve a team id from a uuid (used as-is) or a case-insensitive name.
async fn resolve_team_id(client: &UserClient, org: &str, s: &str) -> Result<String> {
    if looks_like_uuid(s) {
        return Ok(s.to_string());
    }
    let body = client.get(&format!("/organizations/{org}/teams")).await.context("GET teams for match")?;
    let needle = s.trim().to_lowercase();
    body.get("teams")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .find(|t| t.get("name").and_then(Value::as_str).unwrap_or_default().trim().to_lowercase() == needle)
                .and_then(|t| t.get("id").and_then(Value::as_str))
                .map(str::to_string)
        })
        .with_context(|| format!("no team matches '{s}' — run `th api teams list`"))
}

/// Resolve `email-or-id` inputs to `organization_members.id` values. A uuid is
/// used as-is; an email is matched (case-insensitive) against the org members.
async fn resolve_member_ids(client: &UserClient, org: &str, inputs: &[String]) -> Result<Vec<String>> {
    let mut members: Option<Vec<Value>> = None; // fetched lazily, once
    let mut out = Vec::with_capacity(inputs.len());
    for input in inputs {
        let s = input.trim();
        if looks_like_uuid(s) {
            out.push(s.to_string());
            continue;
        }
        if members.is_none() {
            let body = client.get(&format!("/organizations/{org}/members")).await.context("GET members")?;
            members = Some(body.get("members").and_then(Value::as_array).cloned().unwrap_or_default());
        }
        let needle = s.to_lowercase();
        let found = members.as_ref().unwrap().iter().find_map(|m| {
            let email = m.get("userEmail").and_then(Value::as_str)?.trim().to_lowercase();
            (email == needle)
                .then(|| m.get("memberId").and_then(Value::as_str).map(str::to_string))
                .flatten()
        });
        match found {
            Some(id) => out.push(id),
            None => anyhow::bail!("no org member matches '{s}' — run `th api members list`"),
        }
    }
    Ok(dedup(out))
}

/// Resolve `name-or-id` inputs to `organization_member_roles.id` values. A uuid
/// is used as-is; a role name is matched (case-insensitive) against the catalog.
async fn resolve_role_ids(client: &UserClient, org: &str, inputs: &[String]) -> Result<Vec<String>> {
    let mut roles: Option<Vec<Value>> = None;
    let mut out = Vec::with_capacity(inputs.len());
    for input in inputs {
        let s = input.trim();
        if looks_like_uuid(s) {
            out.push(s.to_string());
            continue;
        }
        if roles.is_none() {
            let body = client.get(&format!("/organizations/{org}/roles")).await.context("GET roles")?;
            roles = Some(body.get("roles").and_then(Value::as_array).cloned().unwrap_or_default());
        }
        let needle = s.to_lowercase();
        let found = roles.as_ref().unwrap().iter().find_map(|r| {
            let name = r.get("name").and_then(Value::as_str)?.trim().to_lowercase();
            (name == needle).then(|| r.get("id").and_then(Value::as_str).map(str::to_string)).flatten()
        });
        match found {
            Some(id) => out.push(id),
            None => anyhow::bail!("no role matches '{s}' — run `th api members roles`"),
        }
    }
    Ok(dedup(out))
}

fn render_teams(body: &Value) {
    let teams = body.get("teams").and_then(Value::as_array).cloned().unwrap_or_default();
    println!();
    println!("  {} {}", "Teams".bold(), format!("({})", teams.len()).dimmed());
    if teams.is_empty() {
        println!("\n  {}\n", "none — create one with `th api teams create <name>`".dimmed());
        return;
    }
    let (h_name, h_mem, h_role) = (format!("{:<28}", "NAME"), format!("{:>8}", "MEMBERS"), format!("{:>6}", "ROLES"));
    println!();
    println!("  {}  {}  {}  {}", h_name.dimmed(), h_mem.dimmed(), h_role.dimmed(), "DESCRIPTION".dimmed());
    for t in &teams {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("—");
        let members = t.get("memberIds").and_then(Value::as_array).map_or(0, Vec::len);
        let roles = t.get("roleIds").and_then(Value::as_array).map_or(0, Vec::len);
        let desc = t.get("description").and_then(Value::as_str).unwrap_or("");
        println!(
            "  {:<28}  {:>8}  {:>6}  {}",
            truncate(name, 28),
            members.to_string(),
            roles.to_string(),
            truncate(desc, 40).dimmed()
        );
    }
    println!();
}

/// True for a canonical 8-4-4-4-12 hex uuid, so team/member/role args accept
/// either an id or a human name/email.
fn looks_like_uuid(s: &str) -> bool {
    let s = s.trim();
    s.len() == 36
        && s.chars()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// Order-preserving dedup so a replace-all set carries no duplicate ids.
fn dedup(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter().filter(|id| seen.insert(id.clone())).collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_detection_gates_name_vs_id() {
        assert!(looks_like_uuid("660e8400-e29b-41d4-a716-446655440000"));
        assert!(looks_like_uuid("  660e8400-e29b-41d4-a716-446655440000  ")); // trimmed
        assert!(!looks_like_uuid("Sales"));
        assert!(!looks_like_uuid("jane@acme.com"));
        assert!(!looks_like_uuid("660e8400e29b-41d4-a716-4466554400001")); // wrong dashes
        assert!(!looks_like_uuid("zzzze400-e29b-41d4-a716-446655440000")); // non-hex
    }

    #[test]
    fn dedup_preserves_order_removes_dups() {
        let out = dedup(vec!["a".into(), "b".into(), "a".into(), "c".into(), "b".into()]);
        assert_eq!(out, vec!["a".to_string(), "b".into(), "c".into()]);
        assert!(dedup(Vec::<String>::new()).is_empty());
    }

    #[test]
    fn truncate_adds_ellipsis_past_max() {
        assert_eq!(truncate("short", 28), "short");
        assert_eq!(truncate("abcdef", 4), "abc…");
        // multibyte-safe (counts chars, not bytes)
        assert_eq!(truncate("héllo wörld", 5), "héll…");
    }

    #[test]
    fn render_teams_empty_does_not_panic() {
        render_teams(&json!({ "teams": [] }));
        render_teams(&json!({}));
    }
}

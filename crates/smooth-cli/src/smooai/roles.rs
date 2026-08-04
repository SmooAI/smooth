//! `th roles …` — organization RBAC roles (custom-role catalog), SMOODEV-2368 / ADR-105.
//!
//! A role is a named permission bundle. The catalog is org-scoped: every org
//! sees the SYSTEM roles (`organizationId == null`, immutable) plus its own
//! custom roles. Backed by `/organizations/:org_id/roles` (list/create),
//! `/organizations/:org_id/roles/:role_id` (patch/delete), the member-role
//! endpoints, and the template shortcut
//! `/organizations/:org_id/workforce/role-templates/create-role`.
//!
//! Authenticated as the logged-in *user* (`th auth login`) via [`UserClient`] —
//! the roles routes 401 under an M2M token. Mirrors `teams.rs`.
//!
//! `grant` / `revoke` are read-modify-write: they fetch the role's current
//! permission array, add/remove the given keys, and PATCH the *whole* array
//! back — every existing key is preserved exactly. `set-permissions` replaces
//! the array wholesale. `assign` / `unassign` do the same read-modify-write over
//! a member's assigned role ids (the PUT is replace-all).
//!
//! System roles are immutable: grant/revoke/set-permissions/delete refuse them
//! locally with a clear error *before* the API call (the server also 403s).

use anstream::{eprintln, println};
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::print_json;
use crate::smooai::crm::resolve_org;
use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's roles (system + custom) with permission counts.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Show one role (resolved by name or id) with its full permission-key list.
    Show {
        /// The role to show (name or id).
        role: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
    },
    /// Create a custom role. With `--template`, seed it from an archetype.
    Create {
        /// Role name.
        name: String,
        /// Optional description (ignored when `--template` is used).
        #[arg(long)]
        description: Option<String>,
        /// Seed from a built-in archetype (e.g. `read_only`, `sales_rep`, `admin`).
        #[arg(long)]
        template: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a custom role (system roles cannot be deleted).
    Delete {
        /// The role to delete (name or id).
        role: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Add permission keys to a role, preserving every existing key.
    Grant {
        /// The role (name or id).
        role: String,
        /// Permission keys to add.
        keys: Vec<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove permission keys from a role, preserving every other key.
    Revoke {
        /// The role (name or id).
        role: String,
        /// Permission keys to remove.
        keys: Vec<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Replace a role's entire permission-key set (replace-all).
    SetPermissions {
        /// The role (name or id).
        role: String,
        /// The complete set of permission keys the role should have.
        keys: Vec<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show the roles currently assigned to a member (email or member id).
    MemberRoles {
        /// The member (email or member-id uuid).
        member: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
    },
    /// Assign roles to a member, keeping their existing roles.
    Assign {
        /// The member (email or member-id uuid).
        member: String,
        /// Roles to assign (each a name or role-id uuid).
        roles: Vec<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Unassign roles from a member, keeping their other roles.
    Unassign {
        /// The member (email or member-id uuid).
        member: String,
        /// Roles to unassign (each a name or role-id uuid).
        roles: Vec<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        Cmd::List { org, json } => list(&client, resolve_org(org)?, json).await,
        Cmd::Show { role, org, json } => show(&client, resolve_org(org)?, &role, json).await,
        Cmd::Create {
            name,
            description,
            template,
            org,
        } => create(&client, resolve_org(org)?, &name, description, template).await,
        Cmd::Delete { role, org } => delete(&client, resolve_org(org)?, &role).await,
        Cmd::Grant { role, keys, org } => grant(&client, resolve_org(org)?, &role, &keys).await,
        Cmd::Revoke { role, keys, org } => revoke(&client, resolve_org(org)?, &role, &keys).await,
        Cmd::SetPermissions { role, keys, org } => set_permissions(&client, resolve_org(org)?, &role, keys).await,
        Cmd::MemberRoles { member, org, json } => member_roles(&client, resolve_org(org)?, &member, json).await,
        Cmd::Assign { member, roles, org } => assign(&client, resolve_org(org)?, &member, &roles).await,
        Cmd::Unassign { member, roles, org } => unassign(&client, resolve_org(org)?, &member, &roles).await,
    }
}

async fn list(client: &UserClient, org: String, json: bool) -> Result<()> {
    let body = client.get(&format!("/organizations/{org}/roles")).await.context("GET roles")?;
    if json {
        print_json(&body);
    } else {
        render_roles(&body);
    }
    Ok(())
}

async fn show(client: &UserClient, org: String, role: &str, json: bool) -> Result<()> {
    let roles = fetch_roles(client, &org).await?;
    let found = find_role(&roles, role)?;
    if json {
        print_json(found);
    } else {
        render_role(found);
    }
    Ok(())
}

async fn create(client: &UserClient, org: String, name: &str, description: Option<String>, template: Option<String>) -> Result<()> {
    if let Some(kind) = template.filter(|s| !s.trim().is_empty()) {
        if description.is_some() {
            eprintln!("  {} --description is ignored when --template is used", "!".yellow());
        }
        let r = client
            .post(
                &format!("/organizations/{org}/workforce/role-templates/create-role"),
                &json!({ "kind": kind, "name": name }),
            )
            .await
            .context("POST create-role from template")?;
        let role = r.get("role").unwrap_or(&r);
        let id = role.get("id").and_then(Value::as_str).unwrap_or("?");
        let perms = permissions_of(role).len();
        println!(
            "  {} created role {} {} from template {} ({perms} permission(s))",
            "✚".green(),
            id.dimmed(),
            name.bold(),
            kind.cyan()
        );
    } else {
        let mut body = json!({ "name": name, "permissions": [] });
        if let Some(d) = description.filter(|s| !s.trim().is_empty()) {
            body["description"] = json!(d);
        }
        let r = client.post(&format!("/organizations/{org}/roles"), &body).await.context("POST role")?;
        let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
        println!("  {} created role {} {}", "✚".green(), id.dimmed(), name.bold());
    }
    Ok(())
}

async fn delete(client: &UserClient, org: String, role: &str) -> Result<()> {
    let roles = fetch_roles(client, &org).await?;
    let found = find_role(&roles, role)?;
    refuse_system(found, "delete")?;
    let id = role_id(found);
    client.delete(&format!("/organizations/{org}/roles/{id}")).await.context("DELETE role")?;
    println!("  {} deleted role {}", "🗑".red(), id.dimmed());
    Ok(())
}

async fn grant(client: &UserClient, org: String, role: &str, keys: &[String]) -> Result<()> {
    let roles = fetch_roles(client, &org).await?;
    let found = find_role(&roles, role)?;
    refuse_system(found, "modify")?;
    warn_bad_keys(keys);
    let mut perms = permissions_of(found);
    let mut added = 0_usize;
    for k in keys {
        if !perms.iter().any(|p| p == k) {
            perms.push(k.clone());
            added += 1;
        }
    }
    patch_permissions(client, &org, role_id(found), &perms).await?;
    println!("  {} granted {added} key(s) to {} — now {} total", "✓".green(), role.bold(), perms.len());
    Ok(())
}

async fn revoke(client: &UserClient, org: String, role: &str, keys: &[String]) -> Result<()> {
    let roles = fetch_roles(client, &org).await?;
    let found = find_role(&roles, role)?;
    refuse_system(found, "modify")?;
    let before = permissions_of(found);
    let perms: Vec<String> = before.iter().filter(|p| !keys.iter().any(|k| k == *p)).cloned().collect();
    let removed = before.len() - perms.len();
    patch_permissions(client, &org, role_id(found), &perms).await?;
    println!("  {} revoked {removed} key(s) from {} — now {} total", "✓".green(), role.bold(), perms.len());
    Ok(())
}

async fn set_permissions(client: &UserClient, org: String, role: &str, keys: Vec<String>) -> Result<()> {
    let roles = fetch_roles(client, &org).await?;
    let found = find_role(&roles, role)?;
    refuse_system(found, "modify")?;
    warn_bad_keys(&keys);
    let perms = dedup(keys);
    patch_permissions(client, &org, role_id(found), &perms).await?;
    println!("  {} set {} to {} permission(s)", "✓".green(), role.bold(), perms.len());
    Ok(())
}

async fn member_roles(client: &UserClient, org: String, member: &str, json: bool) -> Result<()> {
    let member_id = resolve_member_id(client, &org, member).await?;
    let body = client
        .get(&format!("/organizations/{org}/members/{member_id}/roles"))
        .await
        .context("GET member roles")?;
    if json {
        print_json(&body);
    } else {
        let roles = fetch_roles(client, &org).await?;
        render_member_roles(member, &body, &roles);
    }
    Ok(())
}

async fn assign(client: &UserClient, org: String, member: &str, roles: &[String]) -> Result<()> {
    let member_id = resolve_member_id(client, &org, member).await?;
    let catalog = fetch_roles(client, &org).await?;
    let want = resolve_role_ids(&catalog, roles)?;
    let mut next = member_role_ids(client, &org, &member_id).await?;
    for id in want {
        if !next.iter().any(|x| x == &id) {
            next.push(id);
        }
    }
    set_member_roles(client, &org, &member_id, &next).await?;
    println!("  {} {} now has {} role(s)", "✓".green(), member.bold(), next.len());
    Ok(())
}

async fn unassign(client: &UserClient, org: String, member: &str, roles: &[String]) -> Result<()> {
    let member_id = resolve_member_id(client, &org, member).await?;
    let catalog = fetch_roles(client, &org).await?;
    let drop = resolve_role_ids(&catalog, roles)?;
    let current = member_role_ids(client, &org, &member_id).await?;
    let next: Vec<String> = current.into_iter().filter(|id| !drop.iter().any(|d| d == id)).collect();
    set_member_roles(client, &org, &member_id, &next).await?;
    println!("  {} {} now has {} role(s)", "✓".green(), member.bold(), next.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// Role fetch / resolve
// ---------------------------------------------------------------------------

/// Fetch the org's role catalog (system + custom) as a `Vec<Value>`.
async fn fetch_roles(client: &UserClient, org: &str) -> Result<Vec<Value>> {
    let body = client.get(&format!("/organizations/{org}/roles")).await.context("GET roles")?;
    Ok(body.get("roles").and_then(Value::as_array).cloned().unwrap_or_default())
}

/// Find a role by exact id (uuid input) or case-insensitive name.
fn find_role<'a>(roles: &'a [Value], needle: &str) -> Result<&'a Value> {
    let s = needle.trim();
    if looks_like_uuid(s) {
        return roles
            .iter()
            .find(|r| r.get("id").and_then(Value::as_str) == Some(s))
            .with_context(|| format!("no role with id '{s}' in this org — run `th roles list`"));
    }
    let lower = s.to_lowercase();
    roles
        .iter()
        .find(|r| r.get("name").and_then(Value::as_str).unwrap_or_default().trim().to_lowercase() == lower)
        .with_context(|| format!("no role matches '{needle}' — run `th roles list`"))
}

/// Resolve `name-or-id` role inputs to role-id strings against the catalog.
fn resolve_role_ids(catalog: &[Value], inputs: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(inputs.len());
    for input in inputs {
        out.push(role_id(find_role(catalog, input)?).to_string());
    }
    Ok(dedup(out))
}

fn role_id(role: &Value) -> &str {
    role.get("id").and_then(Value::as_str).unwrap_or_default()
}

/// True when `organizationId` is null — a SYSTEM role, immutable.
fn is_system(role: &Value) -> bool {
    role.get("organizationId").is_none_or(Value::is_null)
}

/// Refuse an edit to a system role locally, before any API call.
fn refuse_system(role: &Value, verb: &str) -> Result<()> {
    if is_system(role) {
        let name = role.get("name").and_then(Value::as_str).unwrap_or("this role");
        anyhow::bail!("cannot {verb} system role '{name}' — system roles are immutable");
    }
    Ok(())
}

fn permissions_of(role: &Value) -> Vec<String> {
    role.get("permissions")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

async fn patch_permissions(client: &UserClient, org: &str, role_id: &str, perms: &[String]) -> Result<()> {
    client
        .patch(&format!("/organizations/{org}/roles/{role_id}"), &json!({ "permissions": perms }))
        .await
        .context("PATCH role permissions")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Member resolve / roles
// ---------------------------------------------------------------------------

/// Resolve an `email-or-id` input to an `organization_members.id`. A uuid is
/// used as-is; an email is matched (case-insensitive) against the org members.
async fn resolve_member_id(client: &UserClient, org: &str, input: &str) -> Result<String> {
    let s = input.trim();
    if looks_like_uuid(s) {
        return Ok(s.to_string());
    }
    let body = client.get(&format!("/organizations/{org}/members")).await.context("GET members")?;
    let members = body.get("members").and_then(Value::as_array).cloned().unwrap_or_default();
    let needle = s.to_lowercase();
    members
        .iter()
        .find_map(|m| {
            let email = m.get("userEmail").and_then(Value::as_str)?.trim().to_lowercase();
            (email == needle)
                .then(|| m.get("memberId").and_then(Value::as_str).map(str::to_string))
                .flatten()
        })
        .with_context(|| format!("no org member matches '{input}' — run `th members list`"))
}

async fn member_role_ids(client: &UserClient, org: &str, member_id: &str) -> Result<Vec<String>> {
    let body = client
        .get(&format!("/organizations/{org}/members/{member_id}/roles"))
        .await
        .context("GET member roles")?;
    Ok(body
        .get("roleIds")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default())
}

async fn set_member_roles(client: &UserClient, org: &str, member_id: &str, role_ids: &[String]) -> Result<()> {
    client
        .put(&format!("/organizations/{org}/members/{member_id}/roles"), &json!({ "roleIds": role_ids }))
        .await
        .context("PUT member roles")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Warn (never fail) on keys that don't match `^[a-z0-9_.*-]+$`. The server is
/// the source of truth for what's valid — this only catches obvious typos.
fn warn_bad_keys(keys: &[String]) {
    for k in keys {
        if k.is_empty()
            || !k
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '*' | '-'))
        {
            eprintln!(
                "  {} '{k}' doesn't look like a permission key (expected [a-z0-9_.*-]) — sending anyway",
                "!".yellow()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_roles(body: &Value) {
    let roles = body.get("roles").and_then(Value::as_array).cloned().unwrap_or_default();
    println!();
    println!("  {} {}", "Roles".bold(), format!("({})", roles.len()).dimmed());
    if roles.is_empty() {
        println!("\n  {}\n", "none".dimmed());
        return;
    }
    println!();
    println!(
        "  {}  {}  {}  {}",
        format!("{:<24}", "NAME").dimmed(),
        format!("{:<7}", "TYPE").dimmed(),
        format!("{:>5}", "PERMS").dimmed(),
        "DESCRIPTION".dimmed()
    );
    for r in &roles {
        let name = r.get("name").and_then(Value::as_str).unwrap_or("—");
        let kind = if is_system(r) { "system" } else { "custom" };
        let perms = permissions_of(r).len();
        let desc = r.get("description").and_then(Value::as_str).unwrap_or("");
        println!(
            "  {:<24}  {:<7}  {:>5}  {}",
            truncate(name, 24),
            kind,
            perms.to_string(),
            truncate(desc, 40).dimmed()
        );
    }
    println!();
}

fn render_role(role: &Value) {
    let name = role.get("name").and_then(Value::as_str).unwrap_or("—");
    let id = role_id(role);
    let kind = if is_system(role) { "system (immutable)" } else { "custom" };
    let desc = role.get("description").and_then(Value::as_str).unwrap_or("");
    let perms = permissions_of(role);
    println!();
    println!("  {} {}", name.bold(), format!("({kind})").dimmed());
    println!("  {} {}", "id:".dimmed(), id);
    if !desc.is_empty() {
        println!("  {} {desc}", "description:".dimmed());
    }
    println!("  {} {}", "permissions:".dimmed(), perms.len());
    if perms.is_empty() {
        println!("    {}", "(none)".dimmed());
    } else {
        for p in &perms {
            println!("    {p}");
        }
    }
    println!();
}

fn render_member_roles(member: &str, body: &Value, catalog: &[Value]) {
    let ids: Vec<String> = body
        .get("roleIds")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    println!();
    println!("  {} {}", member.bold(), format!("({} role(s))", ids.len()).dimmed());
    if ids.is_empty() {
        println!("\n  {}\n", "no roles assigned".dimmed());
        return;
    }
    for id in &ids {
        let name = catalog
            .iter()
            .find(|r| role_id(r) == id)
            .and_then(|r| r.get("name").and_then(Value::as_str))
            .unwrap_or("(unknown)");
        println!("    {}  {}", name, id.dimmed());
    }
    println!();
}

// ---------------------------------------------------------------------------
// Shared small helpers (mirrors teams.rs)
// ---------------------------------------------------------------------------

/// True for a canonical 8-4-4-4-12 hex uuid, so args accept an id or a name.
fn looks_like_uuid(s: &str) -> bool {
    let s = s.trim();
    s.len() == 36
        && s.chars()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// Order-preserving dedup.
fn dedup(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items.into_iter().filter(|x| seen.insert(x.clone())).collect()
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

    fn system_role() -> Value {
        json!({ "id": "00000000-0000-0000-0000-000000000000", "name": "admin", "organizationId": null, "permissions": ["*"] })
    }
    fn custom_role() -> Value {
        json!({ "id": "660e8400-e29b-41d4-a716-446655440000", "name": "Support", "organizationId": "org-1", "permissions": ["a.b", "c.d"] })
    }

    #[test]
    fn is_system_detects_null_org() {
        assert!(is_system(&system_role()));
        assert!(!is_system(&custom_role()));
        // missing key => treated as system (safe: refuses edits).
        assert!(is_system(&json!({ "name": "x" })));
    }

    #[test]
    fn refuse_system_blocks_system_allows_custom() {
        assert!(refuse_system(&system_role(), "modify").is_err());
        assert!(refuse_system(&custom_role(), "modify").is_ok());
    }

    #[test]
    fn find_role_by_name_and_id() {
        let roles = vec![system_role(), custom_role()];
        assert_eq!(role_id(find_role(&roles, "support").unwrap()), "660e8400-e29b-41d4-a716-446655440000");
        assert_eq!(
            find_role(&roles, "660e8400-e29b-41d4-a716-446655440000").unwrap().get("name").unwrap(),
            "Support"
        );
        assert!(find_role(&roles, "nope").is_err());
        // uuid that isn't in the catalog is a miss, not a name fallback.
        assert!(find_role(&roles, "11111111-1111-1111-1111-111111111111").is_err());
    }

    #[test]
    fn permissions_of_reads_string_array() {
        assert_eq!(permissions_of(&custom_role()), vec!["a.b".to_string(), "c.d".into()]);
        assert!(permissions_of(&json!({ "permissions": null })).is_empty());
        assert!(permissions_of(&json!({})).is_empty());
    }

    #[test]
    fn grant_preserves_existing_keys_and_appends_missing() {
        // Mirrors the read-modify-write in Cmd::Grant.
        let mut perms = permissions_of(&custom_role()); // ["a.b", "c.d"]
        for k in ["c.d", "e.f", "g.h"] {
            if !perms.iter().any(|p| p == k) {
                perms.push(k.to_string());
            }
        }
        assert_eq!(perms, vec!["a.b".to_string(), "c.d".into(), "e.f".into(), "g.h".into()]);
    }

    #[test]
    fn revoke_removes_only_named_keys() {
        let before = permissions_of(&custom_role());
        let drop = ["a.b".to_string()];
        let after: Vec<String> = before.iter().filter(|p| !drop.iter().any(|k| k == *p)).cloned().collect();
        assert_eq!(after, vec!["c.d".to_string()]);
    }

    #[test]
    fn resolve_role_ids_maps_names_and_dedups() {
        let catalog = vec![system_role(), custom_role()];
        let ids = resolve_role_ids(&catalog, &["admin".into(), "Support".into(), "admin".into()]).unwrap();
        assert_eq!(
            ids,
            vec![
                "00000000-0000-0000-0000-000000000000".to_string(),
                "660e8400-e29b-41d4-a716-446655440000".into()
            ]
        );
    }

    #[test]
    fn uuid_detection_gates_name_vs_id() {
        assert!(looks_like_uuid("660e8400-e29b-41d4-a716-446655440000"));
        assert!(!looks_like_uuid("Support"));
        assert!(!looks_like_uuid("admin"));
    }

    #[test]
    fn dedup_preserves_order() {
        assert_eq!(dedup(vec!["a".into(), "b".into(), "a".into()]), vec!["a".to_string(), "b".into()]);
    }

    #[test]
    fn render_helpers_do_not_panic_on_empty() {
        render_roles(&json!({ "roles": [] }));
        render_roles(&json!({}));
        render_role(&custom_role());
        render_member_roles("x@y.com", &json!({ "roleIds": [] }), &[]);
        render_member_roles("x@y.com", &json!({ "roleIds": ["660e8400-e29b-41d4-a716-446655440000"] }), &[custom_role()]);
    }
}

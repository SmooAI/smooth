//! `th smoo admin members add | list | remove` — super-admin org membership
//! without raw SQL (SMOODEV-3291).
//!
//! Every call goes to `/admin/organizations/{org}/members…`, which the backend
//! gates with `requireSuperAdmin`. The server is the gate; this CLI only makes
//! the operation hard to aim at the wrong org or the wrong person:
//!
//! - `--org-id` is **required** (no active-org fallback). These verbs act
//!   across tenants, so the persisted active org — possibly set hours ago by
//!   an unrelated `smoo org switch` — is exactly the wrong default.
//! - `--email` is resolved to a user id client-side (exact, case-insensitive
//!   match via `GET /admin/users/search`), so a typo errors instead of adding
//!   whoever the substring search happened to return first.
//! - Writes print the target (org, user, role, host) and confirm on a TTY;
//!   off a TTY they refuse without `--yes` — the same fail-closed matrix as
//!   every destructive verb (`crate::destructive::decide`).
//! - `add` is idempotent from the operator's seat: an existing member, a 409
//!   from the server, or a `200 { alreadyMember: true }` (the server-side
//!   idempotency change on the same ticket) all end in "already a member".

use std::io::IsTerminal;

use anstream::{eprintln, println};
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::client::{print_ok, AdminApiError, AdminClient};
use super::render::{render, Format, TableOptions};
use crate::destructive::{decide, Confirm, Decision};

/// Columns for the members table, in reading order.
const MEMBER_COLUMNS: &[&str] = &["userId", "email", "fullName", "role", "createdAt"];

/// `GET /admin/users/search` caps `limit` at 100. Ask for the cap so an exact
/// email is found even when the substring (`ilike %q%`) also matches others.
const SEARCH_LIMIT: u32 = 100;

#[derive(Debug, Subcommand)]
pub enum MembersCommands {
    /// List an org's members with their roles.
    List {
        /// Org UUID (required — admin verbs never fall back to the active org).
        #[arg(long = "org-id", visible_alias = "org", value_name = "ORG_UUID")]
        org: String,
        /// Print the raw JSON response instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Add a user to an org directly (no invitation). Idempotent: an existing
    /// member is reported as "already a member", not an error.
    Add {
        /// Org UUID (required — admin verbs never fall back to the active org).
        #[arg(long = "org-id", visible_alias = "org", value_name = "ORG_UUID")]
        org: String,
        #[command(flatten)]
        user: UserSelector,
        /// Role to assign. Roles are org-defined names; the server assigns
        /// none (and this warns) when no role by that name exists.
        #[arg(long, default_value = "admin")]
        role: String,
        #[command(flatten)]
        confirm: Confirm,
        /// Print the resulting member as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove a user from an org.
    #[command(visible_alias = "rm")]
    Remove {
        /// Org UUID (required — admin verbs never fall back to the active org).
        #[arg(long = "org-id", visible_alias = "org", value_name = "ORG_UUID")]
        org: String,
        #[command(flatten)]
        user: UserSelector,
        #[command(flatten)]
        confirm: Confirm,
        /// Print the server response as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Exactly one way to name the user. clap enforces "exactly one" through the
/// required, single-member group; [`UserSelector::parse`] then validates the
/// value itself.
#[derive(Debug, Clone, Args)]
#[group(required = true, multiple = false)]
pub struct UserSelector {
    /// The user's email (exact match, case-insensitive).
    #[arg(long)]
    email: Option<String>,
    /// The user's id (UUID).
    #[arg(long = "user-id", value_name = "USER_UUID")]
    user_id: Option<String>,
}

/// A validated [`UserSelector`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Who {
    Email(String),
    UserId(String),
}

impl UserSelector {
    /// Validate the one value clap let through.
    ///
    /// # Errors
    /// Not an email / not a UUID, or (defensively) both or neither given.
    pub fn parse(&self) -> Result<Who> {
        match (&self.email, &self.user_id) {
            (Some(email), None) => {
                let email = email.trim();
                // A structural check, not RFC 5322: enough to reject the
                // user-id-pasted-into---email slip before any request.
                let valid = email
                    .split_once('@')
                    .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.') && !domain.contains('@'));
                if !valid {
                    bail!("`--email {email}` is not an email address");
                }
                Ok(Who::Email(email.to_string()))
            }
            (None, Some(id)) => Ok(Who::UserId(parse_uuid("--user-id", id)?)),
            _ => bail!("pass exactly one of `--email` or `--user-id`"),
        }
    }
}

/// Parse and canonicalise (lowercase, hyphenated) a UUID flag value. The
/// server's path params are UUID columns; a malformed one otherwise surfaces
/// as an opaque Postgres 500.
fn parse_uuid(flag: &str, raw: &str) -> Result<String> {
    uuid::Uuid::parse_str(raw.trim())
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| anyhow::anyhow!("`{flag} {raw}` is not a UUID"))
}

/// A user as the admin endpoints describe them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUser {
    pub id: String,
    pub email: Option<String>,
    pub full_name: Option<String>,
}

impl ResolvedUser {
    fn from_row(row: &Value, id_key: &str) -> Option<Self> {
        let s = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_string);
        Some(Self {
            id: s(id_key)?,
            email: s("email"),
            full_name: s("fullName"),
        })
    }

    /// `jane@acme.com (Jane Doe)` / the bare id when nothing better is known.
    fn label(&self) -> String {
        match (&self.email, &self.full_name) {
            (Some(e), Some(n)) if !n.is_empty() => format!("{e} ({n})"),
            (Some(e), _) => e.clone(),
            _ => self.id.clone(),
        }
    }
}

/// The array inside a `{ <key>: [...] }` envelope, or a bare array.
fn rows<'a>(body: &'a Value, key: &str) -> &'a [Value] {
    body.get(key).and_then(Value::as_array).or_else(|| body.as_array()).map_or(&[], Vec::as_slice)
}

fn email_matches(row: &Value, email: &str) -> bool {
    row.get("email").and_then(Value::as_str).is_some_and(|e| e.trim().eq_ignore_ascii_case(email))
}

/// Pick the ONE user whose email equals `email` out of a
/// `GET /admin/users/search` response. The search is a substring match over
/// email *and* full name, so its first row is not "the user" — only an exact
/// email match is.
///
/// # Errors
/// No exact match (lists the near misses), or more than one.
pub fn pick_exact_email(search: &Value, email: &str) -> Result<ResolvedUser> {
    let users = rows(search, "users");
    let exact: Vec<&Value> = users.iter().filter(|u| email_matches(u, email)).collect();
    match exact.as_slice() {
        [one] => ResolvedUser::from_row(one, "id").with_context(|| format!("search result for {email} has no `id`")),
        [] => {
            let near: Vec<&str> = users.iter().filter_map(|u| u.get("email").and_then(Value::as_str)).take(5).collect();
            let total = search.get("total").and_then(Value::as_u64).unwrap_or(users.len() as u64);
            if near.is_empty() {
                bail!("no user with email {email} — they need to sign up at smoo.ai first");
            }
            let more = if total > near.len() as u64 {
                format!(" (+{} more)", total - near.len() as u64)
            } else {
                String::new()
            };
            bail!("no user with email exactly {email}; similar: {}{more}", near.join(", "))
        }
        many => bail!(
            "{} users share email {email} ({}) — pass `--user-id` instead",
            many.len(),
            many.iter().filter_map(|u| u.get("id").and_then(Value::as_str)).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Find `who` in a `GET /admin/organizations/{org}/members` response.
#[must_use]
pub fn find_member<'a>(members: &'a Value, who: &Who) -> Option<&'a Value> {
    rows(members, "members").iter().find(|m| match who {
        Who::Email(e) => email_matches(m, e),
        Who::UserId(id) => m.get("userId").and_then(Value::as_str).is_some_and(|u| u.eq_ignore_ascii_case(id)),
    })
}

/// How an add ended, from the operator's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddOutcome {
    /// Newly added; the server's member row.
    Added(Value),
    /// Already a member — a 409, or a `200 { alreadyMember: true }` (whose
    /// body, when there is one, is carried along).
    AlreadyMember(Option<Value>),
}

/// Map the POST result onto [`AddOutcome`]: 2xx is `Added` unless it says
/// `alreadyMember: true`; a 409 is `AlreadyMember`; anything else stays an
/// error (403 keeps its "requires the super_admin role" message).
///
/// # Errors
/// Every non-409 failure, unchanged.
pub fn classify_add(result: Result<Value>) -> Result<AddOutcome> {
    match result {
        Ok(body) if body.get("alreadyMember").and_then(Value::as_bool) == Some(true) => Ok(AddOutcome::AlreadyMember(Some(body))),
        Ok(body) => Ok(AddOutcome::Added(body)),
        Err(e) if e.downcast_ref::<AdminApiError>().is_some_and(|a| a.status == 409) => Ok(AddOutcome::AlreadyMember(None)),
        Err(e) => Err(e),
    }
}

/// `email` → the one user with that email.
///
/// # Errors
/// Transport / auth errors, or no single exact match.
pub async fn resolve_email(client: &AdminClient, email: &str) -> Result<ResolvedUser> {
    let search = client
        .get(&format!("/admin/users/search?q={}&limit={SEARCH_LIMIT}", urlencoding::encode(email)))
        .await
        .with_context(|| format!("look up user {email}"))?;
    pick_exact_email(&search, email)
}

async fn list_members(client: &AdminClient, org: &str) -> Result<Value> {
    client
        .get(&format!("/admin/organizations/{org}/members"))
        .await
        .with_context(|| format!("list members of {org}"))
}

/// What a write is about to do, for the banner and the prompt.
struct WriteTarget<'a> {
    /// "add" / "remove".
    verb: &'a str,
    /// "added" / "removed".
    verb_past: &'a str,
    /// "to" / "from" — `add jane to org X`, `remove jane from org X`.
    prep: &'a str,
    org: &'a str,
    user: &'a ResolvedUser,
    /// Role being granted (add) or currently held (remove).
    role: Option<&'a str>,
}

fn api_base() -> String {
    std::env::var("SMOOAI_API_URL").unwrap_or_else(|_| "https://api.smoo.ai".to_string())
}

/// Print the target on every path (`--yes` included, so unattended runs log
/// what they touched), then resolve the `--dry-run`/`--yes`/TTY matrix.
/// `Ok(true)` = go, `Ok(false)` = dry run.
fn confirm_write(t: &WriteTarget, confirm: Confirm) -> Result<bool> {
    println!();
    println!("  {} {} {}", "about to".yellow().bold(), t.verb.yellow().bold(), t.user.label().cyan().bold());
    println!("    {}  {}", "org ".dimmed(), t.org.yellow());
    println!("    {}  {}", "user".dimmed(), t.user.id);
    if let Some(role) = t.role {
        println!("    {}  {}", "role".dimmed(), role);
    }
    println!("    {}  {}", "host".dimmed(), api_base().dimmed());
    println!();
    match decide(confirm.dry_run, confirm.yes, std::io::stdin().is_terminal()) {
        Decision::DryRun => {
            println!("  {} dry-run — nothing was {}", "●".dimmed(), t.verb_past);
            Ok(false)
        }
        Decision::Proceed => Ok(true),
        Decision::Prompt => {
            let ok = dialoguer::Confirm::new()
                .with_prompt(format!("{} {} {} org {}?", t.verb, t.user.label(), t.prep, t.org))
                .default(false)
                .interact()
                .context("read confirmation")?;
            if !ok {
                eprintln!("  {} aborted — nothing was {}", "✗".yellow(), t.verb_past);
                bail!("aborted by operator");
            }
            Ok(true)
        }
        Decision::Refuse => bail!(
            "refusing to {} {} {} org {} without confirmation: not a terminal, so there is no way to ask. \
             Re-run with `--dry-run` to see the target, or `--yes` to confirm.",
            t.verb,
            t.user.label(),
            t.prep,
            t.org
        ),
    }
}

fn member_table() -> TableOptions {
    TableOptions::default().with_label("members").with_columns(MEMBER_COLUMNS)
}

pub async fn dispatch(cmd: MembersCommands) -> Result<()> {
    // Validate every flag before loading (and possibly refreshing) a session.
    match cmd {
        MembersCommands::List { org, json } => {
            let org = parse_uuid("--org-id", &org)?;
            let client = AdminClient::from_user_session().await?;
            let body = list_members(&client, &org).await?;
            render(&body, Format::from_flag(json), &member_table());
        }
        MembersCommands::Add {
            org,
            user,
            role,
            confirm,
            json,
        } => {
            let org = parse_uuid("--org-id", &org)?;
            let who = user.parse()?;
            let role = role.trim().to_string();
            if role.is_empty() {
                bail!("`--role` cannot be empty");
            }
            let client = AdminClient::from_user_session().await?;
            add(&client, &org, &who, &role, confirm, json).await?;
        }
        MembersCommands::Remove { org, user, confirm, json } => {
            let org = parse_uuid("--org-id", &org)?;
            let who = user.parse()?;
            let client = AdminClient::from_user_session().await?;
            remove(&client, &org, &who, confirm, json).await?;
        }
    }
    Ok(())
}

fn print_already(org: &str, user: &ResolvedUser, member: Option<&Value>, json: bool) {
    print_ok(format!("{} is already a member of {org} — nothing to do", user.label()));
    match member {
        Some(m) if json => render(&json!({ "alreadyMember": true, "member": m }), Format::Json, &TableOptions::default()),
        Some(m) => render(&json!([m]), Format::Table, &member_table()),
        None if json => render(&json!({ "alreadyMember": true, "userId": user.id }), Format::Json, &TableOptions::default()),
        None => {}
    }
}

async fn add(client: &AdminClient, org: &str, who: &Who, role: &str, confirm: Confirm, json: bool) -> Result<()> {
    let user = match who {
        Who::Email(e) => resolve_email(client, e).await?,
        Who::UserId(id) => ResolvedUser {
            id: id.clone(),
            email: None,
            full_name: None,
        },
    };

    // Read first: an existing member short-circuits before any prompt/write,
    // and a --user-id target picks up its email for the banner.
    let members = list_members(client, org).await?;
    if let Some(existing) = find_member(&members, &Who::UserId(user.id.clone())) {
        let known = ResolvedUser::from_row(existing, "userId").unwrap_or_else(|| user.clone());
        print_already(org, &known, Some(existing), json);
        return Ok(());
    }

    let target = WriteTarget {
        verb: "add",
        verb_past: "added",
        prep: "to",
        org,
        user: &user,
        role: Some(role),
    };
    if !confirm_write(&target, confirm)? {
        return Ok(());
    }

    let result = client
        .post(&format!("/admin/organizations/{org}/members"), &json!({ "userId": user.id, "role": role }))
        .await;
    match classify_add(result)? {
        AddOutcome::Added(member) => {
            print_ok(format!("added {} to {org}", user.label()));
            if member.get("role").is_some_and(Value::is_null) {
                eprintln!(
                    "{}",
                    format!("⚠  no role named `{role}` exists — the member was added WITHOUT a role. Assign one in Org Settings → Members.").yellow()
                );
            }
            if json {
                render(&member, Format::Json, &TableOptions::default());
            } else {
                render(&json!([member]), Format::Table, &member_table());
            }
        }
        // Lost a race with another add (or the server is on the idempotent
        // 200 path): re-read so the operator still sees the member row.
        AddOutcome::AlreadyMember(body) => {
            let members = list_members(client, org).await.ok();
            let row = members.as_ref().and_then(|m| find_member(m, &Who::UserId(user.id.clone()))).cloned();
            print_already(org, &user, row.as_ref().or(body.as_ref()), json);
        }
    }
    Ok(())
}

async fn remove(client: &AdminClient, org: &str, who: &Who, confirm: Confirm, json: bool) -> Result<()> {
    // Resolve against the org's own member list: only members can be removed,
    // and it answers `--email` without a platform-wide user search.
    let members = list_members(client, org).await?;
    let Some(member) = find_member(&members, who) else {
        let named = match who {
            Who::Email(e) => e.as_str(),
            Who::UserId(id) => id.as_str(),
        };
        bail!("{named} is not a member of org {org} — see `smoo admin members list --org-id {org}`");
    };
    let user = ResolvedUser::from_row(member, "userId").context("member row has no `userId`")?;
    let role = member.get("role").and_then(Value::as_str);

    let target = WriteTarget {
        verb: "remove",
        verb_past: "removed",
        prep: "from",
        org,
        user: &user,
        role,
    };
    if !confirm_write(&target, confirm)? {
        return Ok(());
    }

    let body = client
        .delete(&format!("/admin/organizations/{org}/members/{}", user.id))
        .await
        .with_context(|| format!("remove {} from {org}", user.label()))?;
    print_ok(format!("removed {} from {org}", user.label()));
    if json {
        render(&body, Format::Json, &TableOptions::default());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;
    use clap::Parser;

    const ORG: &str = "11111111-1111-4111-8111-111111111111";
    const USER: &str = "22222222-2222-4222-8222-222222222222";

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: MembersCommands,
    }

    fn parse(args: &[&str]) -> Result<MembersCommands, clap::Error> {
        let mut full = vec!["members"];
        full.extend_from_slice(args);
        Wrap::try_parse_from(full).map(|w| w.cmd)
    }

    // ── arg parsing ────────────────────────────────────────────────────

    #[test]
    fn add_takes_email_or_user_id_and_defaults_role_to_admin() {
        let MembersCommands::Add {
            org,
            user,
            role,
            confirm,
            json,
        } = parse(&["add", "--org", ORG, "--email", "jane@acme.com"]).unwrap()
        else {
            panic!("expected add");
        };
        assert_eq!(org, ORG);
        assert_eq!(role, "admin");
        assert!(!confirm.yes && !confirm.dry_run && !json);
        assert_eq!(user.parse().unwrap(), Who::Email("jane@acme.com".into()));

        let MembersCommands::Add { user, role, confirm, .. } = parse(&["add", "--org-id", ORG, "--user-id", USER, "--role", "member", "--yes"]).unwrap() else {
            panic!("expected add");
        };
        assert_eq!(role, "member");
        assert!(confirm.yes);
        assert_eq!(user.parse().unwrap(), Who::UserId(USER.into()));
    }

    #[test]
    fn exactly_one_of_email_or_user_id() {
        for verb in ["add", "remove"] {
            let both = parse(&[verb, "--org", ORG, "--email", "a@b.co", "--user-id", USER]).expect_err("both must be rejected");
            assert_eq!(both.kind(), clap::error::ErrorKind::ArgumentConflict, "{verb}: {both}");
            let neither = parse(&[verb, "--org", ORG]).expect_err("neither must be rejected");
            assert_eq!(neither.kind(), clap::error::ErrorKind::MissingRequiredArgument, "{verb}: {neither}");
        }
    }

    #[test]
    fn org_is_required_on_every_verb() {
        for args in [&["list"][..], &["add", "--email", "a@b.co"], &["remove", "--user-id", USER]] {
            let err = parse(args).expect_err("missing --org must be rejected");
            assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument, "{args:?}");
        }
    }

    #[test]
    fn list_json_and_remove_confirm_flags_parse() {
        assert!(matches!(
            parse(&["list", "--org", ORG, "--json"]).unwrap(),
            MembersCommands::List { json: true, .. }
        ));
        let MembersCommands::Remove { confirm, .. } = parse(&["rm", "--org", ORG, "--email", "a@b.co", "--dry-run"]).unwrap() else {
            panic!("expected remove");
        };
        assert!(confirm.dry_run && !confirm.yes);
    }

    #[test]
    fn selector_values_are_validated() {
        let sel = |email: Option<&str>, id: Option<&str>| UserSelector {
            email: email.map(str::to_string),
            user_id: id.map(str::to_string),
        };
        assert_eq!(sel(Some("  Jane@Acme.com "), None).parse().unwrap(), Who::Email("Jane@Acme.com".into()));
        for bad in [USER, "jane", "@acme.com", "jane@acme", "a@b@c.com"] {
            assert!(sel(Some(bad), None).parse().is_err(), "{bad} must not pass as an email");
        }
        // uuids are canonicalised to lowercase
        assert_eq!(sel(None, Some(&USER.to_uppercase())).parse().unwrap(), Who::UserId(USER.into()));
        assert!(sel(None, Some("jane@acme.com")).parse().is_err());
        assert!(sel(None, None).parse().is_err());
        assert!(sel(Some("a@b.co"), Some(USER)).parse().is_err());
        assert!(parse_uuid("--org-id", "not-a-uuid").is_err());
    }

    // ── email resolution ───────────────────────────────────────────────

    fn search_body() -> Value {
        json!({
            "users": [
                { "id": "u-1", "email": "jane.doe@acme.com", "fullName": "Jane Doe" },
                { "id": "u-2", "email": "Jane@Acme.com", "fullName": "Jane" },
                { "id": "u-3", "email": "ops@acme.com", "fullName": "Jane's ops alias" },
            ],
            "total": 3
        })
    }

    #[test]
    fn email_resolution_needs_an_exact_case_insensitive_match() {
        // substring search returned three rows; only u-2 IS jane@acme.com
        let u = pick_exact_email(&search_body(), "jane@acme.com").unwrap();
        assert_eq!(u.id, "u-2");
        assert_eq!(u.label(), "Jane@Acme.com (Jane)");
    }

    #[test]
    fn email_resolution_rejects_near_misses_and_unknowns() {
        let err = pick_exact_email(&search_body(), "doe@acme.com").unwrap_err().to_string();
        assert!(err.contains("similar") && err.contains("jane.doe@acme.com"), "{err}");
        let err = pick_exact_email(&json!({ "users": [], "total": 0 }), "who@acme.com").unwrap_err().to_string();
        assert!(err.contains("no user with email who@acme.com"), "{err}");
    }

    #[test]
    fn email_resolution_refuses_to_guess_between_duplicates() {
        let dup = json!({ "users": [{ "id": "a", "email": "x@y.co" }, { "id": "b", "email": "X@Y.co" }] });
        let err = pick_exact_email(&dup, "x@y.co").unwrap_err().to_string();
        assert!(err.contains("--user-id") && err.contains('a') && err.contains('b'), "{err}");
    }

    #[test]
    fn find_member_by_email_or_id() {
        let members = json!({ "members": [
            { "id": "m-1", "userId": USER, "email": "Jane@Acme.com", "role": "admin" },
        ]});
        assert!(find_member(&members, &Who::Email("jane@acme.com".into())).is_some());
        assert!(find_member(&members, &Who::UserId(USER.into())).is_some());
        assert!(find_member(&members, &Who::Email("bob@acme.com".into())).is_none());
        assert!(find_member(&json!({ "members": [] }), &Who::UserId(USER.into())).is_none());
    }

    // ── 409-as-success ─────────────────────────────────────────────────

    fn api_err(status: u16) -> anyhow::Error {
        AdminApiError {
            method: "POST".into(),
            url: format!("https://api.smoo.ai/admin/organizations/{ORG}/members"),
            status,
            body: r#"{"error":"x"}"#.into(),
        }
        .into()
    }

    #[test]
    fn a_409_is_already_a_member_not_a_failure() {
        assert_eq!(classify_add(Err(api_err(409))).unwrap(), AddOutcome::AlreadyMember(None));
    }

    #[test]
    fn idempotent_200_is_already_a_member() {
        let body = json!({ "alreadyMember": true, "userId": USER, "role": "admin" });
        assert_eq!(classify_add(Ok(body.clone())).unwrap(), AddOutcome::AlreadyMember(Some(body)));
    }

    #[test]
    fn a_201_is_added_and_other_failures_stay_failures() {
        let body = json!({ "id": "m-1", "userId": USER, "role": "admin" });
        assert_eq!(classify_add(Ok(body.clone())).unwrap(), AddOutcome::Added(body));
        let err = classify_add(Err(api_err(403))).unwrap_err().to_string();
        assert!(err.contains("requires the super_admin role"), "{err}");
        assert!(classify_add(Err(api_err(404))).is_err());
        assert!(classify_add(Err(anyhow::anyhow!("connection refused"))).is_err());
    }

    // ── against a fake api.smoo.ai (the real reqwest + AdminClient path) ──

    /// A local HTTP server with canned answers, standing in for api.smoo.ai.
    async fn fake_api(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn resolve_email_goes_through_admin_user_search() {
        use axum::{extract::Query, routing::get, Json};
        use std::collections::HashMap;
        let router = axum::Router::new().route(
            "/admin/users/search",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                // the full email is the query and the page is the server cap
                assert_eq!(q.get("q").map(String::as_str), Some("jane+ops@acme.com"));
                assert_eq!(q.get("limit").map(String::as_str), Some("100"));
                Json(json!({ "users": [
                    { "id": "u-9", "email": "jane+ops@acme.com.au" },
                    { "id": USER, "email": "jane+ops@acme.com", "fullName": "Jane" },
                ], "total": 2 }))
            }),
        );
        let client = AdminClient::with_base(fake_api(router).await, "tok");
        let user = resolve_email(&client, "jane+ops@acme.com").await.unwrap();
        assert_eq!(user.id, USER);
    }

    #[tokio::test]
    async fn a_live_409_classifies_as_already_member() {
        use axum::{http::StatusCode, routing::post, Json};
        let router = axum::Router::new().route(
            "/admin/organizations/{org}/members",
            post(|| async { (StatusCode::CONFLICT, Json(json!({ "error": "User is already a member of this organization" }))) }),
        );
        let client = AdminClient::with_base(fake_api(router).await, "tok");
        let result = client.post(&format!("/admin/organizations/{ORG}/members"), &json!({ "userId": USER })).await;
        assert_eq!(classify_add(result).unwrap(), AddOutcome::AlreadyMember(None));
    }

    #[tokio::test]
    async fn a_live_403_names_the_super_admin_role() {
        use axum::{http::StatusCode, routing::get};
        let router = axum::Router::new().route("/admin/organizations/{org}/members", get(|| async { (StatusCode::FORBIDDEN, "Forbidden") }));
        let client = AdminClient::with_base(fake_api(router).await, "tok");
        let err = list_members(&client, ORG).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("requires the super_admin role"), "{msg}");
        assert_eq!(err.downcast_ref::<AdminApiError>().map(|e| e.status), Some(403));
    }
}

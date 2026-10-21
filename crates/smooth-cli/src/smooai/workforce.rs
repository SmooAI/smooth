//! `smoo workforce …` — the org's people directory.
//! CLI twin of the hosted MCP `workforce_directory` tool (pearl th-a5d991).
//!
//! `create-account` (SMOODEV-4052) is the twin of the hosted MCP
//! `workforce_create_accounts` and the Workforce page's "Create account": it
//! gives directory people a Smoo account and org membership, with no email
//! sent, through `POST /organizations/{org}/workforce/employees/accounts`
//! (smooai api-prime `handlers/workforce_accounts.rs`). The route decides:
//! `org.members.manage`, the `workforce` product, verified work-email domains,
//! and a `--role-id` you could not grant yourself is refused. A 403 is
//! reported as "admin only".

use std::collections::{HashMap, HashSet};
use std::future::Future;

use anstream::println;
use anyhow::{anyhow, bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use smooth_api_client::SmoothApiClient;

use super::{print_json, require_active_org, require_authed, require_user_session};
use crate::destructive::{Confirm, Severity, Target};

/// `MAX_EMPLOYEES_PER_REQUEST` in smooai api-prime `handlers/workforce_accounts.rs`.
/// A longer list is sent in batches of this size.
pub const MAX_EMPLOYEES_PER_REQUEST: usize = 50;

/// Which slice of the directory to read. Mirrors the MCP tool's `view` arg.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, ValueEnum)]
pub enum View {
    /// Name, email, title, department, manager.
    #[default]
    Employees,
    /// The department tree.
    OrgUnits,
    /// Employees + units + the manager → report edge count.
    OrgChart,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Read this org's people directory: `employees` (default), `org-units`
    /// (the department tree), or `org-chart` (both plus the manager → report
    /// edges). Use for "who works here", "who reports to X".
    Directory {
        /// Which view to read.
        #[arg(value_enum, default_value_t = View::Employees)]
        view: View,
        /// Print raw JSON instead of the compact listing.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Give directory people a Smoo account and membership in this org, on their
    /// work email, with no email sent (needs org.members.manage). They can then
    /// be put in ring groups and given calling seats.
    #[command(visible_alias = "create-accounts")]
    CreateAccount {
        /// Employee ids from `workforce directory --json` (repeatable).
        #[arg(value_name = "EMPLOYEE_ID")]
        employee_ids: Vec<String>,
        /// Everyone in the directory marked as needing an account who has none yet.
        #[arg(long = "needs-account", conflicts_with = "employee_ids")]
        needs_account: bool,
        /// Role for members who have no role yet (from `smoo roles list`). Without it they
        /// get the org's default member role; someone who already has a role keeps it.
        #[arg(long = "role-id")]
        role_id: Option<String>,
        /// Print the raw results as JSON.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
}

/// A UUID, lowercased, so a value can never re-point the request.
pub fn uuid_arg(label: &str, raw: &str) -> Result<String> {
    let id = raw.trim();
    let ok = id.len() == 36
        && id
            .chars()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() });
    if !ok {
        bail!("{label} must be a uuid (from `th smoo workforce directory --json`); got {id:?}");
    }
    Ok(id.to_ascii_lowercase())
}

/// Checked, de-duplicated ids in the order given.
pub fn employee_ids(raw: &[String]) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in raw {
        let id = uuid_arg("employee id", r)?;
        if seen.insert(id.clone()) {
            out.push(id);
        }
    }
    if out.is_empty() {
        bail!("name at least one employee id, or pass --needs-account");
    }
    Ok(out)
}

/// The directory people waiting for an account: `needsAccount` and not yet a member.
#[must_use]
pub fn needing_account(directory: &Value) -> Vec<String> {
    rows(directory, "data")
        .map(|rows| {
            rows.iter()
                .filter(|e| e.get("needsAccount").and_then(Value::as_bool) == Some(true))
                .filter(|e| e.get("orgMemberUserId").is_none_or(Value::is_null))
                .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// One POST body per batch of [`MAX_EMPLOYEES_PER_REQUEST`].
#[must_use]
pub fn batches(ids: &[String], role_id: Option<&str>) -> Vec<Value> {
    ids.chunks(MAX_EMPLOYEES_PER_REQUEST)
        .map(|chunk| {
            let mut body = json!({ "employeeIds": chunk });
            if let Some(r) = role_id {
                body["roleId"] = json!(r);
            }
            body
        })
        .collect()
}

/// One result as a line: what happened, the role, the SSO link.
#[must_use]
pub fn result_line(r: &Value) -> String {
    let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("");
    let who = if s("email").is_empty() { s("employeeId") } else { s("email") };
    let status = s("status");
    if status == "failed" {
        return format!("{who}: failed — {}", s("error"));
    }
    let mut out = format!("{who}: {}", status.replace('_', " "));
    match r.get("role").filter(|v| !v.is_null()) {
        Some(role) => {
            out.push_str("; role ");
            out.push_str(role.get("name").and_then(Value::as_str).unwrap_or(""));
        }
        None => out.push_str("; NO ROLE (run again with --role-id)"),
    }
    if r.get("ssoLinked").and_then(Value::as_bool) == Some(true) {
        out.push_str("; linked to SSO");
    }
    if let Some(e) = r.get("ssoLinkError").and_then(Value::as_str) {
        out.push_str("; SSO link refused: ");
        out.push_str(e);
    }
    out
}

/// How many people the confirm prompt and `--dry-run` name before "+N more".
pub const PREVIEW_LIMIT: usize = 10;

/// Why the route is refused without a person's session: it is `auth: 'user'`
/// (the account is attributed to, and role-checked against, the admin), so an
/// org API key gets a bare 401.
pub const NEEDS_USER: &str = "creating accounts acts as you, so it needs a signed-in Smoo user — run `th auth login` \
     (an org API key / `th api login` session is refused by this route)";

/// The people about to get accounts, as "Name <email>", the first
/// [`PREVIEW_LIMIT`] of them then "+N more". An id the directory doesn't know
/// is shown as-is so the prompt never hides who is affected.
#[must_use]
pub fn who_preview(ids: &[String], directory: &Value) -> String {
    let by_id: HashMap<&str, &Value> = rows(directory, "data")
        .map(|rows| rows.iter().filter_map(|e| Some((e.get("id")?.as_str()?, e))).collect())
        .unwrap_or_default();
    let mut names: Vec<String> = ids
        .iter()
        .take(PREVIEW_LIMIT)
        .map(|id| match by_id.get(id.as_str()) {
            Some(e) => {
                let name = e.get("fullName").and_then(Value::as_str).unwrap_or("(unnamed)");
                match e.get("primaryEmail").and_then(Value::as_str).filter(|m| !m.is_empty()) {
                    Some(mail) => format!("{name} <{mail}>"),
                    None => format!("{name} (no work email)"),
                }
            }
            None => format!("{id} (not in the directory)"),
        })
        .collect();
    if ids.len() > PREVIEW_LIMIT {
        names.push(format!("+{} more", ids.len() - PREVIEW_LIMIT));
    }
    names.join(", ")
}

/// "BDR (uuid)" from the org's `GET /roles` catalog, or the bare id if it isn't there.
#[must_use]
pub fn role_label(roles: &Value, role_id: &str) -> String {
    roles
        .get("roles")
        .and_then(Value::as_array)
        .and_then(|rs| rs.iter().find(|r| r.get("id").and_then(Value::as_str) == Some(role_id)))
        .and_then(|r| r.get("name").and_then(Value::as_str))
        .map_or_else(|| role_id.to_string(), |name| format!("{name} ({role_id})"))
}

/// What the confirm prompt and `--dry-run` say is about to happen.
#[must_use]
pub fn plan_summary(ids: &[String], directory: &Value, role: Option<&str>) -> String {
    let role = role.map(|r| format!(" with role {r}")).unwrap_or_default();
    format!("{} person(s){role}: {}", ids.len(), who_preview(ids, directory))
}

/// The results of every batch that went through, and the batch that didn't (0-based) with its error.
pub struct BatchRun {
    pub results: Vec<Value>,
    pub failure: Option<(usize, anyhow::Error)>,
}

/// POST each batch in order and stop at the first failure, keeping what the
/// earlier batches returned: those accounts exist now, so dropping their
/// results would hide changes that were made.
pub async fn send_batches<F, Fut>(bodies: &[Value], mut post: F) -> BatchRun
where
    F: FnMut(Value) -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let mut results = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        match post(body.clone()).await {
            Ok(resp) => results.extend(resp.get("results").and_then(Value::as_array).cloned().unwrap_or_default()),
            Err(e) => {
                return BatchRun {
                    results,
                    failure: Some((i, e)),
                }
            }
        }
    }
    BatchRun { results, failure: None }
}

/// A 403 reads as "admin only" and a 401 as "sign in as a user", not an outage.
fn explain(err: &anyhow::Error) -> Option<&'static str> {
    let text = format!("{err:#}");
    if text.contains("HTTP 403") {
        Some("admin only: creating accounts needs org.members.manage and the Workforce product, and --role-id may not carry a permission you don't hold")
    } else if text.contains("HTTP 401") {
        Some(NEEDS_USER)
    } else {
        None
    }
}

/// The error for batch `i` (0-based) of `total`. Only a refusal (401/403) of
/// the FIRST batch is known to have changed nothing; anything later means the
/// earlier batches' accounts exist, and a timeout or 5xx may have landed.
#[must_use]
pub fn batch_error(i: usize, total: usize, err: anyhow::Error) -> anyhow::Error {
    let why = explain(&err);
    let msg = match (i, why) {
        (0, Some(why)) => format!("{why} — nothing was changed"),
        (0, None) if total == 1 => "the request failed; check the directory before retrying (re-running is safe: existing members come back as already member)".to_string(),
        (_, why) => format!(
            "batch {} of {total} failed{}; the results above are from the batches that went through — re-run to continue (people who already have an account come back as already member)",
            i + 1,
            why.map(|w| format!(" ({w})")).unwrap_or_default()
        ),
    };
    err.context(msg)
}

/// The create-account route is user-only, so refuse an M2M session up front
/// instead of letting it 401.
async fn user_client() -> Result<SmoothApiClient> {
    require_user_session().await.map_err(|_| anyhow!(NEEDS_USER))
}

fn print_results(results: &[Value]) {
    println!();
    for r in results {
        let mark = if r.get("status").and_then(Value::as_str) == Some("failed") {
            "✗".red().to_string()
        } else {
            "✓".green().to_string()
        };
        println!("  {mark} {}", result_line(r));
    }
    println!();
}

async fn create_account(
    client: &SmoothApiClient,
    raw: &[String],
    needs_account: bool,
    role_id: Option<String>,
    json: bool,
    org: Option<String>,
    confirm: Confirm,
) -> Result<()> {
    let role_id = role_id.as_deref().map(|r| uuid_arg("--role-id", r)).transpose()?;
    let o = require_active_org(client, org)?;
    // Read the directory either way: --needs-account picks from it, and the
    // prompt names who gets an account.
    let directory = client
        .get(&format!("/organizations/{o}/workforce/employees"))
        .await
        .context("GET workforce employees")?;
    let ids = if needs_account {
        let ids = needing_account(&directory);
        if ids.is_empty() {
            println!("\n  {} {}\n", "●".dimmed(), "nobody in the directory is waiting for an account".dimmed());
            return Ok(());
        }
        ids
    } else {
        employee_ids(raw)?
    };
    let role = match role_id.as_deref() {
        // A failed catalog read only costs the prompt the role's name.
        Some(r) => Some(match client.get(&format!("/organizations/{o}/roles")).await {
            Ok(roles) => role_label(&roles, r),
            Err(_) => r.to_string(),
        }),
        None => None,
    };
    let what = plan_summary(&ids, &directory, role.as_deref());
    let bodies = batches(&ids, role_id.as_deref());
    if confirm.dry_run {
        println!("\n  {} would create accounts for {what}", "●".dimmed());
        for b in &bodies {
            println!("\n  {} would POST /organizations/{o}/workforce/employees/accounts", "●".dimmed());
            print_json(b);
        }
    }
    let proceed = crate::destructive::gate_with(
        &Target {
            verb: "create Smoo accounts (and org membership) for",
            noun: "directory",
            id: &what,
            org: &o,
            severity: Severity::Standard,
        },
        confirm,
    )?;
    if !proceed {
        return Ok(());
    }
    let path = format!("/organizations/{o}/workforce/employees/accounts");
    let run = send_batches(&bodies, |body| {
        let path = &path;
        async move { client.post(path, Some(&body)).await }
    })
    .await;
    let failure = run.failure.map(|(i, e)| (i, batch_error(i, bodies.len(), e)));
    if json {
        let error = failure
            .as_ref()
            .map(|(i, e)| json!({ "batch": i + 1, "of": bodies.len(), "message": format!("{e:#}") }));
        print_json(&json!({ "results": run.results, "error": error }));
    } else if !run.results.is_empty() || failure.is_none() {
        print_results(&run.results);
    }
    match failure {
        Some((_, e)) => Err(e),
        None => Ok(()),
    }
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = match &cmd {
        Cmd::CreateAccount { .. } => user_client().await?,
        Cmd::Directory { .. } => require_authed().await?,
    };
    match cmd {
        Cmd::CreateAccount {
            employee_ids: raw,
            needs_account,
            role_id,
            json,
            org,
            confirm,
        } => create_account(&client, &raw, needs_account, role_id, json, org, confirm).await?,
        Cmd::Directory { view, json, org } => {
            let o = require_active_org(&client, org)?;
            match view {
                View::Employees => {
                    let resp = client
                        .get(&format!("/organizations/{o}/workforce/employees"))
                        .await
                        .context("GET workforce employees")?;
                    if json {
                        print_json(&resp);
                    } else {
                        print_employees(&resp, "data");
                        println!();
                    }
                }
                View::OrgUnits => {
                    let resp = client
                        .get(&format!("/organizations/{o}/workforce/org-units"))
                        .await
                        .context("GET workforce org-units")?;
                    if json {
                        print_json(&resp);
                    } else {
                        print_org_units(&resp, "data");
                        println!();
                    }
                }
                View::OrgChart => {
                    let resp = client
                        .get(&format!("/organizations/{o}/workforce/org-chart"))
                        .await
                        .context("GET workforce org-chart")?;
                    if json {
                        print_json(&resp);
                    } else {
                        print_employees(&resp, "employees");
                        print_org_units(&resp, "units");
                        // The edges are implied by each employee's managerEmployeeId,
                        // so report the count rather than repeating every pair.
                        let edges = resp.get("edges").and_then(|v| v.as_array()).map_or(0, Vec::len);
                        println!();
                        println!("  {edges} manager → report edge(s).");
                        println!();
                    }
                }
            }
        }
    }
    Ok(())
}

/// Rows under `key`, accepting the enveloped shape or a bare top-level array —
/// the routes answer with either (same contract as the MCP server's `rows()`).
fn rows<'a>(body: &'a serde_json::Value, key: &str) -> Option<&'a Vec<serde_json::Value>> {
    body.as_array().or_else(|| body.get(key).and_then(|v| v.as_array()))
}

fn print_employees(body: &serde_json::Value, key: &str) {
    let Some(rows) = rows(body, key) else {
        return;
    };
    println!();
    if rows.is_empty() {
        println!("  {} {}", "●".dimmed(), "No employees in the directory yet.".dimmed());
        return;
    }
    for e in rows {
        let name = e.get("fullName").and_then(|v| v.as_str()).unwrap_or("(unnamed)");
        let email = e.get("primaryEmail").and_then(|v| v.as_str()).unwrap_or("");
        let title = e.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let dept = e.get("department").and_then(|v| v.as_str()).unwrap_or("");
        let role = [title, dept].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(", ");
        let status = e.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let suffix = if status.is_empty() { String::new() } else { format!(" [{status}]") };
        println!("  {} {} {} {}{}", "○".dimmed(), name.bold(), email.cyan(), role.dimmed(), suffix.dimmed());
    }
}

fn print_org_units(body: &serde_json::Value, key: &str) {
    let Some(rows) = rows(body, key) else {
        return;
    };
    println!();
    if rows.is_empty() {
        println!("  {} {}", "●".dimmed(), "No org units defined yet.".dimmed());
        return;
    }
    for u in rows {
        let name = u.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)");
        let path = u.get("orgUnitPath").and_then(|v| v.as_str()).unwrap_or("");
        println!("  {} {} {}", "▸".cyan(), name.bold(), path.dimmed());
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::json;

    use super::{Cmd, View};

    const E1: &str = "11111111-2222-3333-4444-555555555555";
    const E2: &str = "11111111-2222-3333-4444-666666666666";
    const R: &str = "aaaaaaaa-0000-0000-0000-000000000001";

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[test]
    fn directory_defaults_to_employees() {
        let w = Wrap::try_parse_from(["t", "directory"]).expect("bare directory must parse");
        assert!(matches!(
            w.cmd,
            Cmd::Directory {
                view: View::Employees,
                json: false,
                org: None
            }
        ));
    }

    #[test]
    fn directory_parses_each_view() {
        for (arg, want) in [("employees", View::Employees), ("org-units", View::OrgUnits), ("org-chart", View::OrgChart)] {
            let w = Wrap::try_parse_from(["t", "directory", arg]).expect("view must parse");
            match w.cmd {
                Cmd::Directory { view, .. } => assert_eq!(view, want),
                Cmd::CreateAccount { .. } => panic!("wrong command"),
            }
        }
    }

    #[test]
    fn create_account_parses_ids_role_and_confirm() {
        let w = Wrap::try_parse_from(["t", "create-account", E1, E2, "--role-id", R, "--yes"]).expect("create-account must parse");
        match w.cmd {
            Cmd::CreateAccount {
                employee_ids,
                needs_account,
                role_id,
                confirm,
                ..
            } => {
                assert_eq!(employee_ids.len(), 2);
                assert!(!needs_account);
                assert_eq!(role_id.as_deref(), Some(R));
                assert!(confirm.yes && !confirm.dry_run);
            }
            Cmd::Directory { .. } => panic!("wrong command"),
        }
        assert!(Wrap::try_parse_from(["t", "create-accounts", "--needs-account", "--dry-run"]).is_ok());
        assert!(
            Wrap::try_parse_from(["t", "create-account", E1, "--needs-account"]).is_err(),
            "ids and --needs-account are exclusive"
        );
    }

    #[test]
    fn ids_are_checked_and_deduped() {
        let ids = super::employee_ids(&[E2.to_uppercase(), E1.into(), E2.into()]).unwrap();
        assert_eq!(ids, vec![E2.to_string(), E1.to_string()]);
        assert!(super::employee_ids(&[]).is_err());
        assert!(super::employee_ids(&["../org-chart".into()]).is_err());
        assert!(super::uuid_arg("--role-id", "admin").is_err());
    }

    #[test]
    fn needs_account_picks_only_people_without_an_account() {
        let directory = serde_json::json!({ "data": [
            { "id": E1, "needsAccount": true, "orgMemberUserId": null },
            { "id": E2, "needsAccount": true, "orgMemberUserId": R },
            { "id": R, "needsAccount": false },
        ] });
        assert_eq!(super::needing_account(&directory), vec![E1.to_string()]);
        assert!(super::needing_account(&serde_json::json!({ "data": [] })).is_empty());
    }

    #[test]
    fn long_lists_go_in_batches_of_fifty() {
        let ids: Vec<String> = (0..120).map(|i| format!("{i}")).collect();
        let b = super::batches(&ids, Some(R));
        assert_eq!(b.len(), 3);
        assert_eq!(b[0]["employeeIds"].as_array().unwrap().len(), super::MAX_EMPLOYEES_PER_REQUEST);
        assert_eq!(b[2]["employeeIds"].as_array().unwrap().len(), 20);
        assert!(b.iter().all(|x| x["roleId"] == R));
        assert!(super::batches(&ids[..1], None)[0].get("roleId").is_none());
    }

    #[test]
    fn result_lines_say_what_happened() {
        use serde_json::json;
        assert_eq!(
            super::result_line(&json!({ "employeeId": E1, "status": "created", "email": "linda@corp.com", "role": { "name": "BDR" }, "ssoLinked": true })),
            "linda@corp.com: created; role BDR; linked to SSO"
        );
        assert!(super::result_line(&json!({ "employeeId": E1, "status": "already_member", "role": null })).contains("already member; NO ROLE"));
        assert_eq!(
            super::result_line(&json!({ "employeeId": E1, "status": "failed", "error": "no work email" })),
            format!("{E1}: failed — no work email")
        );
    }

    #[test]
    fn a_refused_first_batch_says_nothing_changed() {
        let e = super::batch_error(0, 1, anyhow::anyhow!("HTTP 403 Forbidden"));
        let msg = format!("{e:#}");
        assert!(msg.contains("admin only") && msg.contains("nothing was changed"), "{msg}");
        let e = super::batch_error(0, 1, anyhow::anyhow!("HTTP 401 Unauthorized"));
        assert!(format!("{e:#}").contains("th auth login"));
        let e = super::batch_error(0, 1, anyhow::anyhow!("HTTP 500"));
        let msg = format!("{e:#}");
        assert!(!msg.contains("admin only") && !msg.contains("nothing was changed"), "{msg}");
    }

    #[test]
    fn a_later_batch_failure_never_claims_nothing_changed() {
        for err in ["HTTP 403 Forbidden", "HTTP 500", "connection reset"] {
            let msg = format!("{:#}", super::batch_error(2, 3, anyhow::anyhow!(err)));
            assert!(msg.contains("batch 3 of 3 failed") && msg.contains("re-run to continue"), "{msg}");
            assert!(!msg.contains("nothing was changed"), "{msg}");
            assert!(msg.contains(err), "the cause is kept: {msg}");
        }
        assert!(format!("{:#}", super::batch_error(1, 2, anyhow::anyhow!("HTTP 403"))).contains("admin only"));
    }

    #[tokio::test]
    async fn a_failed_batch_keeps_earlier_results() {
        let ids: Vec<String> = (0..120).map(|i| format!("00000000-0000-4000-8000-{i:012}")).collect();
        let bodies = super::batches(&ids, None);
        let mut calls = 0;
        let run = super::send_batches(&bodies, |body| {
            calls += 1;
            let n = calls;
            async move {
                if n == 2 {
                    anyhow::bail!("HTTP 403 Forbidden");
                }
                let first = body["employeeIds"][0].clone();
                Ok(json!({ "results": [{ "employeeId": first, "status": "added" }] }))
            }
        })
        .await;
        assert_eq!(calls, 2, "stops at the failed batch");
        assert_eq!(run.results.len(), 1, "batch 1's results survive");
        assert_eq!(run.results[0]["employeeId"], ids[0].as_str());
        let (i, _) = run.failure.expect("batch 2 failed");
        assert_eq!(i, 1);

        let run = super::send_batches(&bodies, |_| async { Ok(json!({ "results": [{ "status": "added" }] })) }).await;
        assert_eq!(run.results.len(), 3);
        assert!(run.failure.is_none());
    }

    #[test]
    fn a_missing_user_session_points_at_auth_login() {
        assert!(super::NEEDS_USER.contains("th auth login"));
        assert!(super::NEEDS_USER.contains("API key"), "says why the M2M session won't do");
    }

    #[test]
    fn the_plan_names_people_and_the_role() {
        let dir = json!({ "data": [
            { "id": E1, "fullName": "Linda Ray", "primaryEmail": "linda@corp.com" },
            { "id": E2, "fullName": "Sam Lee", "primaryEmail": null }
        ] });
        let ghost = "00000000-0000-4000-8000-00000000abcd".to_string();
        let ids = vec![E1.to_string(), E2.to_string(), ghost.clone()];
        let roles = json!({ "roles": [{ "id": R, "name": "BDR" }] });
        let role = super::role_label(&roles, R);
        assert_eq!(role, format!("BDR ({R})"));
        assert_eq!(super::role_label(&json!({ "roles": [] }), R), R, "unknown role falls back to the id");
        let plan = super::plan_summary(&ids, &dir, Some(&role));
        assert!(plan.starts_with(&format!("3 person(s) with role BDR ({R}): ")), "{plan}");
        assert!(plan.contains("Linda Ray <linda@corp.com>"), "{plan}");
        assert!(plan.contains("Sam Lee (no work email)"), "{plan}");
        assert!(plan.contains(&format!("{ghost} (not in the directory)")), "{plan}");
    }

    #[test]
    fn a_long_plan_is_truncated() {
        let ids: Vec<String> = (0..25).map(|i| format!("00000000-0000-4000-8000-{i:012}")).collect();
        let dir = json!({ "data": ids.iter().map(|id| json!({ "id": id, "fullName": "P", "primaryEmail": "p@corp.com" })).collect::<Vec<_>>() });
        let preview = super::who_preview(&ids, &dir);
        assert_eq!(preview.matches("P <p@corp.com>").count(), super::PREVIEW_LIMIT);
        assert!(preview.ends_with("+15 more"), "{preview}");
    }

    #[test]
    fn directory_rejects_unknown_view() {
        assert!(Wrap::try_parse_from(["t", "directory", "robots"]).is_err());
    }

    #[test]
    fn directory_parses_json_and_org() {
        let w = Wrap::try_parse_from(["t", "directory", "org-chart", "--json", "--org-id", "o1"]).expect("flags must parse");
        match w.cmd {
            Cmd::Directory { view, json, org } => {
                assert_eq!(view, View::OrgChart);
                assert!(json);
                assert_eq!(org.as_deref(), Some("o1"));
            }
            Cmd::CreateAccount { .. } => panic!("wrong command"),
        }
    }
}

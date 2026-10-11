//! `th smoo crm duplicates …` — the CRM duplicate-contact merge queue
//! (SMOODEV-3859, smooai ADR-139). CLI twin of the copilot's
//! `crm.list_duplicates` / `crm.merge_duplicate_group` / … tools and the
//! hosted MCP `crm_*_duplicate*` tools: all three call the same api-prime
//! routes the web app's CRM → Duplicates page calls.
//!
//! | verb          | route (under `/organizations/{org}/crm/merge-candidates`) |
//! |---------------|------------------------------------------------------------|
//! | `list`        | `GET /groups` — open pairs, grouped by person              |
//! | `history`     | `GET ?status=…` — merged / auto-merged / rejected pairs     |
//! | `merge-group` | `POST /merge-group` — fold several records into one        |
//! | `approve`     | `POST /:id/approve` — merge one pair                       |
//! | `reject`      | `POST /:id/reject` — "not the same person"                |
//! | `undo`        | `POST /:id/undo` — reverse one merge from its snapshot     |
//!
//! Every route needs `crm.contacts.merge` (admins hold it) and the paid
//! `crm_ai_dedupe` feature; the server answers 403 / 402 and [`explain`]
//! rewords those. Merges and undo go through [`crate::destructive`]: the
//! target is printed, `--dry-run` stops there, and a script needs `--yes`.
//! Each merge stays individually undoable (`history`, then `undo`).

use std::fmt::Write as _;

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::{print_json, require_authed};
use crate::destructive::{Confirm, Severity, Target};

/// Statuses the server's `status` filter accepts (`history` expands to
/// merged, rejected, unmerged and stale). Checked locally so a typo fails
/// before the round-trip; the server re-validates.
pub const STATUSES: &[&str] = &["pending", "snoozed", "merged", "auto_merged", "rejected", "stale", "unmerged", "history"];

/// The default `history` filter: every decision a person or the auto-merger made.
pub const DEFAULT_HISTORY: &str = "merged,auto_merged,rejected";

/// Reasons longer than this are cut in the human view (`--json` has them whole).
const REASON_MAX: usize = 160;

/// Pairs listed per group in the human view (`--json` has them all).
const PAIRS_SHOWN: usize = 10;

#[derive(Subcommand)]
pub enum Cmd {
    /// List open duplicates, grouped by person (biggest groups first).
    List {
        /// Groups per page.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Groups to skip.
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Also show pairs the AI judged to be different people.
        #[arg(long)]
        include_different: bool,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Decided pairs: merges (yours and the auto-merger's) and rejections.
    History {
        /// Comma-separated statuses: merged, auto_merged, rejected, unmerged,
        /// stale, pending, snoozed — or `history`.
        #[arg(long, default_value = DEFAULT_HISTORY)]
        status: String,
        /// Pairs per page.
        #[arg(long, default_value_t = 25)]
        limit: u32,
        /// Pairs to skip.
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Merge several records of one person into the one you keep.
    ///
    /// The contacts must be linked by open pairs (one group from `list`).
    /// Each merge can be undone on its own.
    MergeGroup {
        /// The contact id to keep (the survivor).
        #[arg(long)]
        keep: String,
        /// The contact ids to fold into it.
        #[arg(required = true)]
        contacts: Vec<String>,
        /// The survivor's primary email — any address one of the records has.
        /// Default: a work address beats a personal one.
        #[arg(long)]
        primary_email: Option<String>,
        /// A note recorded on each merge.
        #[arg(long)]
        note: Option<String>,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Merge one pair (a candidate id from `list`).
    Approve {
        /// The pair's candidate id.
        candidate_id: String,
        /// The contact id to keep. Default: the AI's suggestion.
        #[arg(long)]
        keep: Option<String>,
        /// The survivor's primary email — any address either record has.
        #[arg(long)]
        primary_email: Option<String>,
        /// A note recorded on the merge.
        #[arg(long)]
        note: Option<String>,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Mark a pair as two different people.
    ///
    /// It does not come back until one of the two contacts changes.
    Reject {
        /// The pair's candidate id.
        candidate_id: String,
        /// Why (recorded on the pair).
        #[arg(long)]
        note: Option<String>,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Reverse one merge (a merged pair's candidate id from `history`).
    Undo {
        /// The merged pair's candidate id.
        candidate_id: String,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[allow(clippy::too_many_lines)] // one flat dispatch arm per verb, like the sibling modules
pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List {
            limit,
            offset,
            include_different,
            json,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let r = client
                .get(&groups_path(&org, limit, offset, include_different))
                .await
                .map_err(explain)
                .context("GET crm/merge-candidates/groups")?;
            if json {
                print_json(&r);
            } else {
                println!();
                println!("{}", render_groups(&r, limit, offset));
            }
            Ok(())
        }
        Cmd::History {
            status,
            limit,
            offset,
            json,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let path = history_path(&org, &status, limit, offset)?;
            let r = client.get(&path).await.map_err(explain).context("GET crm/merge-candidates")?;
            if json {
                print_json(&r);
            } else {
                println!();
                println!("{}", render_history(&r, limit, offset));
            }
            Ok(())
        }
        Cmd::MergeGroup {
            keep,
            contacts,
            primary_email,
            note,
            json,
            confirm,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let body = merge_group_body(&keep, &contacts, primary_email.as_deref(), note.as_deref())?;
            let n = body["contactIds"].as_array().map_or(0, Vec::len);
            let what = format!("{n} record(s) into contact");
            if !gate(&org, "merge", &what, &keep, confirm)? {
                return Ok(());
            }
            let r = client
                .post(&base(&org, "/merge-group"), Some(&body))
                .await
                .map_err(explain)
                .context("POST crm/merge-candidates/merge-group")?;
            if json {
                print_json(&r);
            } else {
                println!("{}", render_merge_group(&r));
            }
            Ok(())
        }
        Cmd::Approve {
            candidate_id,
            keep,
            primary_email,
            note,
            json,
            confirm,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let id = check_id(&candidate_id, "candidate")?;
            let body = approve_body(keep.as_deref(), primary_email.as_deref(), note.as_deref())?;
            if !gate(&org, "merge", "duplicate pair", &id, confirm)? {
                return Ok(());
            }
            let r = client
                .post(&base(&org, &format!("/{id}/approve")), Some(&body))
                .await
                .map_err(explain)
                .context("POST crm/merge-candidates/:id/approve")?;
            if json {
                print_json(&r);
            } else {
                println!("  {} {}", "✓".green(), pair_line(r.get("candidate").unwrap_or(&r)));
            }
            Ok(())
        }
        Cmd::Reject { candidate_id, note, json, org } => {
            let org = crate::active_org::resolve(org)?;
            let id = check_id(&candidate_id, "candidate")?;
            let body = note_body(note.as_deref());
            let r = client
                .post(&base(&org, &format!("/{id}/reject")), Some(&body))
                .await
                .map_err(explain)
                .context("POST crm/merge-candidates/:id/reject")?;
            if json {
                print_json(&r);
            } else {
                println!("  {} not the same person: {}", "✗".yellow(), pair_line(&r));
            }
            Ok(())
        }
        Cmd::Undo {
            candidate_id,
            json,
            confirm,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let id = check_id(&candidate_id, "candidate")?;
            if !gate(&org, "undo", "merge of pair", &id, confirm)? {
                return Ok(());
            }
            let r = client
                .post(&base(&org, &format!("/{id}/undo")), None)
                .await
                .map_err(explain)
                .context("POST crm/merge-candidates/:id/undo")?;
            if json {
                print_json(&r);
            } else {
                println!("  {} undone: {}", "↺".green(), pair_line(&r));
            }
            Ok(())
        }
    }
}

fn gate(org: &str, verb: &str, noun: &str, id: &str, confirm: Confirm) -> Result<bool> {
    crate::destructive::gate_with(
        &Target {
            verb,
            noun,
            id,
            org,
            severity: Severity::Standard,
        },
        confirm,
    )
}

fn base(org: &str, suffix: &str) -> String {
    format!("/organizations/{org}/crm/merge-candidates{suffix}")
}

/// `GET /groups` with its paging.
#[must_use]
pub fn groups_path(org: &str, limit: u32, offset: u32, include_different: bool) -> String {
    let mut p = base(org, &format!("/groups?limit={limit}&offset={offset}"));
    if include_different {
        p.push_str("&includeDifferent=true");
    }
    p
}

/// `GET ?status=…`, refusing a status the server does not know.
///
/// # Errors
/// An empty or unknown status.
pub fn history_path(org: &str, status: &str, limit: u32, offset: u32) -> Result<String> {
    let parts: Vec<&str> = status.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        bail!("--status needs at least one of: {}", STATUSES.join(", "));
    }
    if let Some(bad) = parts.iter().find(|s| !STATUSES.contains(s)) {
        bail!("unknown status \"{bad}\" — use one of: {}", STATUSES.join(", "));
    }
    Ok(base(org, &format!("?status={}&limit={limit}&offset={offset}", parts.join(","))))
}

/// An id bound for a URL path or body: a UUID's alphabet only, so an argument
/// can never re-point the request at another route.
///
/// # Errors
/// Anything but hex digits and dashes.
pub fn check_id(id: &str, what: &str) -> Result<String> {
    let id = id.trim();
    if !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Ok(id.to_string())
    } else {
        bail!("\"{id}\" is not a {what} id — see `th smoo crm duplicates list`")
    }
}

fn opt_text(v: Option<&str>) -> Option<String> {
    v.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn check_email(e: Option<&str>) -> Result<Option<String>> {
    match opt_text(e) {
        Some(e) if !e.contains('@') || e.contains(char::is_whitespace) => bail!("--primary-email \"{e}\" is not an email address"),
        other => Ok(other),
    }
}

/// The `merge-group` body. The survivor is dropped from `contacts` and repeats
/// are collapsed, so `--keep A A B` merges just B.
///
/// # Errors
/// A malformed id, a malformed email, or nothing left to merge.
pub fn merge_group_body(keep: &str, contacts: &[String], primary_email: Option<&str>, note: Option<&str>) -> Result<Value> {
    let keep = check_id(keep, "contact")?;
    let mut ids: Vec<String> = Vec::new();
    for c in contacts {
        let id = check_id(c, "contact")?;
        if !id.eq_ignore_ascii_case(&keep) && !ids.iter().any(|x| x.eq_ignore_ascii_case(&id)) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        bail!("name at least one contact to merge into {keep} (other than {keep} itself)");
    }
    let mut body = json!({ "survivorId": keep, "contactIds": ids });
    if let Some(e) = check_email(primary_email)? {
        body["primaryEmail"] = json!(e);
    }
    if let Some(n) = opt_text(note) {
        body["note"] = json!(n);
    }
    Ok(body)
}

/// The `approve` body. Absent fields keep the server's defaults: the AI's
/// suggested survivor and field picks, and a work address as primary.
///
/// # Errors
/// A malformed survivor id or email.
pub fn approve_body(keep: Option<&str>, primary_email: Option<&str>, note: Option<&str>) -> Result<Value> {
    let mut body = json!({});
    if let Some(k) = opt_text(keep) {
        body["survivorId"] = json!(check_id(&k, "contact")?);
    }
    if let Some(e) = check_email(primary_email)? {
        body["primaryEmail"] = json!(e);
    }
    if let Some(n) = opt_text(note) {
        body["note"] = json!(n);
    }
    Ok(body)
}

/// The `reject` body.
#[must_use]
pub fn note_body(note: Option<&str>) -> Value {
    opt_text(note).map_or_else(|| json!({}), |n| json!({ "note": n }))
}

/// Reword the queue's refusals: 402 is a plan the org lacks, 403 a permission
/// the caller lacks — neither reads well as a raw status line.
#[must_use]
pub fn explain(e: anyhow::Error) -> anyhow::Error {
    let msg = e.to_string();
    if msg.contains("HTTP 402") {
        anyhow::anyhow!("AI duplicate merging is part of CRM Pro — upgrade this org's plan to review and merge duplicate contacts")
    } else if msg.contains("HTTP 403") {
        anyhow::anyhow!("you need the `crm.contacts.merge` permission in this org to review duplicates (org admins have it) — ask an admin, or check `--org`")
    } else {
        e
    }
}

// ── rendering ────────────────────────────────────────────────────────────────

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

/// "Jane Doe", falling back to the email, the phone, then "(no name)".
fn contact_name(c: &Value) -> String {
    let name = [s(c, "firstName"), s(c, "lastName")].into_iter().flatten().collect::<Vec<_>>().join(" ");
    if !name.is_empty() {
        return name;
    }
    s(c, "email")
        .or_else(|| s(c, "phone"))
        .or_else(|| s(c, "mobilePhone"))
        .unwrap_or("(no name)")
        .to_string()
}

/// One contact in a group: id, name, reachability, company, source, added.
fn contact_line(c: &Value, keeper: bool) -> String {
    let mut parts = vec![contact_name(c)];
    if let Some(e) = s(c, "email") {
        parts.push(e.to_string());
    }
    if let Some(p) = s(c, "phone").or_else(|| s(c, "mobilePhone")) {
        parts.push(p.to_string());
    }
    if let Some(co) = s(c, "companyName") {
        parts.push(co.to_string());
    }
    parts.push(format!("source {}", s(c, "source").unwrap_or("unknown")));
    if let Some(at) = s(c, "createdAt") {
        parts.push(format!("added {}", at.get(..10).unwrap_or(at)));
    }
    let mark = if keeper { "★" } else { " " };
    format!("{mark} {}  {}", s(c, "id").unwrap_or("?"), parts.join(" · "))
}

fn percent(v: Option<f64>) -> String {
    #[allow(clippy::cast_possible_truncation)]
    v.map_or_else(|| "—".to_string(), |c| format!("{}%", (c * 100.0).round() as i64))
}

fn truncate(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        text
    } else {
        format!("{}…", text.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// The AI's verdict as a reader says it (`same_person` → "same person").
fn verdict_label(v: &str) -> String {
    v.replace('_', " ")
}

/// One pair: status, both names, the AI's verdict, and the candidate id.
fn pair_line(p: &Value) -> String {
    let a = p.get("contactA").map_or_else(|| "?".to_string(), contact_name);
    let b = p.get("contactB").map_or_else(|| "?".to_string(), contact_name);
    let mut line = format!(
        "{a} ↔ {b}  [{}{}]  candidate {}",
        s(p, "status").unwrap_or("?"),
        s(p, "verdict")
            .map(|v| format!(", AI: {} {}", verdict_label(v), percent(p.get("confidence").and_then(Value::as_f64))))
            .unwrap_or_default(),
        s(p, "id").unwrap_or("?")
    );
    if p.get("canUndo").and_then(Value::as_bool) == Some(true) {
        line.push_str("  (undoable)");
    }
    line
}

fn more_hint(shown: usize, limit: u32, offset: u32) -> Option<String> {
    (u32::try_from(shown).ok() == Some(limit) && limit > 0).then(|| format!("  More may exist — `--offset {}`.", offset + limit))
}

/// The `list` view: one block per person, the suggested keeper starred, the
/// pairs that link them with the AI's verdict, and a ready-to-run merge line.
#[must_use]
pub fn render_groups(groups: &Value, limit: u32, offset: u32) -> String {
    let Some(arr) = groups.as_array().filter(|a| !a.is_empty()) else {
        return if offset > 0 {
            "  No more duplicate groups.".to_string()
        } else {
            "  No open duplicates. 🎉".to_string()
        };
    };
    let mut out = Vec::new();
    for (i, g) in arr.iter().enumerate() {
        let contacts = g.get("contacts").and_then(Value::as_array).cloned().unwrap_or_default();
        let keeper = s(g, "suggestedSurvivorId").unwrap_or_default();
        out.push(format!(
            "  {} {} · {} records",
            "Group".bold(),
            usize::try_from(offset).unwrap_or(0) + i + 1,
            contacts.len()
        ));
        for c in &contacts {
            out.push(format!("    {}", contact_line(c, s(c, "id") == Some(keeper))));
        }
        let pairs = g.get("pairs").and_then(Value::as_array).cloned().unwrap_or_default();
        for p in pairs.iter().take(PAIRS_SHOWN) {
            let short = |k: &str| s(p, k).map_or("?", |id| id.get(..8).unwrap_or(id)).to_string();
            let mut line = format!(
                "      {} ↔ {}  {} {}  candidate {}",
                short("contactAId"),
                short("contactBId"),
                s(p, "verdict").map_or_else(|| "unjudged".to_string(), verdict_label),
                percent(p.get("confidence").and_then(Value::as_f64)),
                s(p, "id").unwrap_or("?")
            );
            if let Some(r) = s(p, "reason") {
                let _ = write!(line, "\n        {}", truncate(r, REASON_MAX).dimmed());
            }
            out.push(line);
        }
        if pairs.len() > PAIRS_SHOWN {
            out.push(format!("      … showing {PAIRS_SHOWN} of {} pairs (`--json` for all)", pairs.len()));
        }
        // A ready-to-run merge line only when the AI judged every link the same
        // person: a group chained by a shared name ("Hello") is not one person.
        let all_same = !pairs.is_empty() && pairs.iter().all(|p| s(p, "verdict") == Some("same_person"));
        if !keeper.is_empty() && all_same {
            let others: Vec<&str> = contacts.iter().filter_map(|c| s(c, "id")).filter(|id| *id != keeper).collect();
            out.push(format!(
                "    {} th smoo crm duplicates merge-group --keep {keeper} {}",
                "→".dimmed(),
                others.join(" ")
            ));
        } else {
            out.push(format!(
                "    {} not every pair is judged the same person — check the records; `reject <candidate>` the ones that are not",
                "→".dimmed()
            ));
        }
        out.push(String::new());
    }
    if let Some(h) = more_hint(arr.len(), limit, offset) {
        out.push(h);
    }
    out.join("\n")
}

/// The `history` view: one line per decided pair.
#[must_use]
pub fn render_history(pairs: &Value, limit: u32, offset: u32) -> String {
    let Some(arr) = pairs.as_array().filter(|a| !a.is_empty()) else {
        return "  Nothing decided yet.".to_string();
    };
    let mut out: Vec<String> = arr
        .iter()
        .map(|p| {
            let when = s(p, "reviewedAt").or_else(|| s(p, "updatedAt")).map_or("", |t| t.get(..10).unwrap_or(t));
            format!("  {when}  {}", pair_line(p))
        })
        .collect();
    if let Some(h) = more_hint(arr.len(), limit, offset) {
        out.push(h);
    }
    out.join("\n")
}

/// The `merge-group` outcome: one line per folded record.
#[must_use]
pub fn render_merge_group(r: &Value) -> String {
    let mut out = vec![format!("  kept {}", s(r, "survivorId").unwrap_or("?"))];
    let mut failed = 0;
    for o in r.get("results").and_then(Value::as_array).into_iter().flatten() {
        let id = s(o, "contactId").unwrap_or("?");
        if s(o, "status") == Some("merged") {
            out.push(format!(
                "  {} merged {id}  (undo: candidate {})",
                "✓".green(),
                s(o, "candidateId").unwrap_or("?")
            ));
        } else {
            failed += 1;
            out.push(format!("  {} {id}: {}", "✗".red(), s(o, "error").unwrap_or("failed")));
        }
    }
    if failed > 0 {
        out.push(format!("  {failed} record(s) not merged — the rest are; fix and re-run for those."));
    }
    out.join("\n")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "unwrap/expect/panic are the idiom for test assertions"
)]
mod tests {
    use super::*;

    const A: &str = "11111111-1111-1111-1111-111111111111";
    const B: &str = "22222222-2222-2222-2222-222222222222";
    const C: &str = "33333333-3333-3333-3333-333333333333";

    fn v(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn groups_path_pages_and_opts_into_different() {
        assert_eq!(
            groups_path("o1", 20, 0, false),
            "/organizations/o1/crm/merge-candidates/groups?limit=20&offset=0"
        );
        assert_eq!(
            groups_path("o1", 5, 10, true),
            "/organizations/o1/crm/merge-candidates/groups?limit=5&offset=10&includeDifferent=true"
        );
    }

    #[test]
    fn history_path_validates_statuses() {
        assert_eq!(
            history_path("o1", DEFAULT_HISTORY, 25, 0).unwrap(),
            "/organizations/o1/crm/merge-candidates?status=merged,auto_merged,rejected&limit=25&offset=0"
        );
        assert_eq!(
            history_path("o1", " history , ", 1, 2).unwrap(),
            "/organizations/o1/crm/merge-candidates?status=history&limit=1&offset=2"
        );
        assert!(history_path("o1", "merged,done", 25, 0).is_err());
        assert!(history_path("o1", " , ", 25, 0).is_err());
    }

    #[test]
    fn ids_keep_to_the_uuid_alphabet() {
        assert_eq!(check_id(&format!(" {A} "), "contact").unwrap(), A);
        for bad in ["", "../groups", "abc/approve", "x".repeat(10).as_str(), &"a".repeat(65)] {
            assert!(check_id(bad, "contact").is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn merge_group_body_drops_the_keeper_and_repeats() {
        assert_eq!(
            merge_group_body(A, &v(&[A, B, C, B]), Some(" brent@smoo.ai "), Some(" dupes of me ")).unwrap(),
            json!({ "survivorId": A, "contactIds": [B, C], "primaryEmail": "brent@smoo.ai", "note": "dupes of me" })
        );
        assert_eq!(
            merge_group_body(A, &v(&[B]), None, Some("  ")).unwrap(),
            json!({ "survivorId": A, "contactIds": [B] })
        );
    }

    #[test]
    fn merge_group_body_refuses_bad_input() {
        assert!(merge_group_body(A, &v(&[A]), None, None).is_err(), "nothing to merge");
        assert!(merge_group_body(A, &[], None, None).is_err());
        assert!(merge_group_body("nope", &v(&[B]), None, None).is_err());
        assert!(merge_group_body(A, &v(&["b/../x"]), None, None).is_err());
        assert!(merge_group_body(A, &v(&[B]), Some("not-an-email"), None).is_err());
        assert!(merge_group_body(A, &v(&[B]), Some("a b@c.com"), None).is_err());
    }

    #[test]
    fn approve_body_sends_only_what_was_chosen() {
        assert_eq!(approve_body(None, None, None).unwrap(), json!({}));
        assert_eq!(
            approve_body(Some(B), Some("b@acme.com"), Some("same person")).unwrap(),
            json!({ "survivorId": B, "primaryEmail": "b@acme.com", "note": "same person" })
        );
        assert!(approve_body(Some("zz"), None, None).is_err());
        assert!(approve_body(None, Some("x"), None).is_err());
    }

    #[test]
    fn note_body_omits_a_blank_note() {
        assert_eq!(note_body(None), json!({}));
        assert_eq!(note_body(Some("   ")), json!({}));
        assert_eq!(note_body(Some(" different dad ")), json!({ "note": "different dad" }));
    }

    #[test]
    fn explain_rewords_plan_and_permission_refusals() {
        let e = explain(anyhow::anyhow!("GET /x returned HTTP 402 Payment Required: {{}}"));
        assert!(e.to_string().contains("CRM Pro"));
        let e = explain(anyhow::anyhow!("GET /x returned HTTP 403 Forbidden: {{}}"));
        assert!(e.to_string().contains("crm.contacts.merge"));
        let e = explain(anyhow::anyhow!("GET /x returned HTTP 409 Conflict: already merged"));
        assert!(e.to_string().contains("409"), "other errors pass through");
    }

    fn contact(id: &str, first: &str, email: &str, source: &str) -> Value {
        json!({ "id": id, "firstName": first, "lastName": "Rager", "email": email, "phone": null,
                "companyName": "Smoo AI", "source": source, "createdAt": "2025-03-04T05:06:07Z" })
    }

    fn group() -> Value {
        json!([{
            "id": "c1",
            "suggestedSurvivorId": A,
            "contacts": [contact(A, "Brent", "brent@smoo.ai", "manual"), contact(B, "Brent", "brent+x@smoo.ai", "chat_widget"), contact(C, "", "", "sms")],
            "pairs": [
                { "id": "cand-1", "contactAId": A, "contactBId": B, "verdict": "same_person", "confidence": 0.937, "reason": "Same   name\nand domain" },
                { "id": "cand-2", "contactAId": B, "contactBId": C, "verdict": "unsure", "confidence": null }
            ]
        }])
    }

    #[test]
    fn groups_render_keeper_pairs_and_merge_line() {
        let out = render_groups(&group(), 20, 0);
        assert!(out.contains("Group"), "{out}");
        assert!(out.contains("3 records"));
        assert!(out.contains(&format!("★ {A}  Brent Rager · brent@smoo.ai · Smoo AI · source manual · added 2025-03-04")));
        assert!(out.contains(&format!("  {B}  Brent Rager")), "non-keeper has no star");
        assert!(out.contains("Rager · Smoo AI · source sms"), "nameless-first contact still renders: {out}");
        assert!(out.contains("same person 94%  candidate cand-1"));
        assert!(out.contains("unsure —  candidate cand-2"));
        assert!(out.contains("Same name and domain"), "reason whitespace collapsed");
        assert!(!out.contains("merge-group --keep"), "an unsure link gets no ready-made merge line");
        assert!(out.contains("not every pair is judged the same person"));
        assert!(!out.contains("More may exist"));

        let mut sure = group();
        sure[0]["pairs"][1]["verdict"] = json!("same_person");
        let out = render_groups(&sure, 20, 0);
        assert!(out.contains(&format!("merge-group --keep {A} {B} {C}")), "{out}");
    }

    #[test]
    fn groups_render_empty_and_paging() {
        assert!(render_groups(&json!([]), 20, 0).contains("No open duplicates"));
        assert!(render_groups(&json!([]), 20, 40).contains("No more"));
        assert!(render_groups(&group(), 1, 4).contains("`--offset 5`"));
        assert!(render_groups(&group(), 1, 4).contains("Group") && render_groups(&group(), 1, 4).contains(" 5 · "));
    }

    #[test]
    fn big_groups_report_how_many_pairs_are_hidden() {
        let pairs: Vec<Value> = (0..12)
            .map(|i| json!({ "id": format!("cand-{i}"), "contactAId": A, "contactBId": B }))
            .collect();
        let g = json!([{ "suggestedSurvivorId": A, "contacts": [contact(A, "X", "x@a.com", "gmail")], "pairs": pairs }]);
        let out = render_groups(&g, 20, 0);
        assert!(out.contains("candidate cand-9"));
        assert!(!out.contains("candidate cand-10"));
        assert!(out.contains("showing 10 of 12 pairs"));
    }

    #[test]
    fn contact_name_falls_back() {
        assert_eq!(contact_name(&json!({ "email": "a@b.com" })), "a@b.com");
        assert_eq!(contact_name(&json!({ "mobilePhone": "+15551234567" })), "+15551234567");
        assert_eq!(contact_name(&json!({})), "(no name)");
    }

    #[test]
    fn history_renders_one_line_per_pair() {
        let pairs = json!([{
            "id": "cand-9", "status": "merged", "verdict": "same_person", "confidence": 0.5, "canUndo": true,
            "reviewedAt": "2026-10-09T10:00:00Z",
            "contactA": { "firstName": "Ann" }, "contactB": { "email": "ann@x.com" }
        }]);
        let out = render_history(&pairs, 25, 0);
        assert_eq!(out, "  2026-10-09  Ann ↔ ann@x.com  [merged, AI: same person 50%]  candidate cand-9  (undoable)");
        assert!(render_history(&json!([]), 25, 0).contains("Nothing decided"));
    }

    #[test]
    fn merge_group_outcomes_report_each_record() {
        let r = json!({ "survivorId": A, "results": [
            { "contactId": B, "status": "merged", "candidateId": "cand-1" },
            { "contactId": C, "status": "error", "error": "already merged into another contact" }
        ]});
        let out = render_merge_group(&r);
        assert!(out.contains(&format!("kept {A}")));
        assert!(out.contains(&format!("merged {B}  (undo: candidate cand-1)")));
        assert!(out.contains(&format!("{C}: already merged into another contact")));
        assert!(out.contains("1 record(s) not merged"));
        assert!(!render_merge_group(&json!({ "survivorId": A, "results": [] })).contains("not merged"));
    }

    #[derive(clap::Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    fn parse(args: &[&str]) -> Result<Cmd, clap::Error> {
        use clap::Parser;
        Wrap::try_parse_from(std::iter::once("t").chain(args.iter().copied())).map(|w| w.cmd)
    }

    #[test]
    fn every_verb_parses() {
        assert!(matches!(
            parse(&["list"]).unwrap(),
            Cmd::List {
                limit: 20,
                offset: 0,
                include_different: false,
                json: false,
                org: None
            }
        ));
        assert!(matches!(
            parse(&["list", "--limit", "5", "--offset", "5", "--include-different", "--json", "--org", "o"]).unwrap(),
            Cmd::List {
                limit: 5,
                offset: 5,
                include_different: true,
                json: true,
                org: Some(_)
            }
        ));
        match parse(&["history"]).unwrap() {
            Cmd::History { status, limit: 25, .. } => assert_eq!(status, DEFAULT_HISTORY),
            _ => panic!("history"),
        }
        match parse(&["merge-group", "--keep", A, B, C, "--primary-email", "a@b.com", "--yes"]).unwrap() {
            Cmd::MergeGroup {
                keep,
                contacts,
                primary_email,
                confirm,
                ..
            } => {
                assert_eq!(keep, A);
                assert_eq!(contacts, v(&[B, C]));
                assert_eq!(primary_email.as_deref(), Some("a@b.com"));
                assert!(confirm.yes && !confirm.dry_run);
            }
            _ => panic!("merge-group"),
        }
        assert!(parse(&["merge-group", "--keep", A]).is_err(), "merge-group needs contacts");
        assert!(parse(&["merge-group", B]).is_err(), "merge-group needs --keep");
        match parse(&["approve", "cand-1", "--keep", B, "--dry-run"]).unwrap() {
            Cmd::Approve {
                candidate_id, keep, confirm, ..
            } => {
                assert_eq!(candidate_id, "cand-1");
                assert_eq!(keep.as_deref(), Some(B));
                assert!(confirm.dry_run);
            }
            _ => panic!("approve"),
        }
        assert!(matches!(parse(&["reject", "c", "--note", "n"]).unwrap(), Cmd::Reject { note: Some(_), .. }));
        assert!(matches!(parse(&["undo", "c", "--yes"]).unwrap(), Cmd::Undo { .. }));
        assert!(parse(&["undo"]).is_err(), "undo needs a candidate id");
    }

    #[test]
    fn truncate_counts_chars_not_bytes() {
        assert_eq!(truncate("ééééé", 3), "éé…");
        assert_eq!(truncate("short", 10), "short");
    }
}

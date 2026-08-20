//! `smoo audiences …` — saved contact segments campaigns and drips send to:
//! list, create, members, add-members, resolve. CLI twin of the hosted MCP
//! `audience_*` tools (pearl th-b1f09c).
//!
//! `resolve` PREVIEWS by default — it reports who the audience currently
//! matches without changing the stored membership; only `--materialize`
//! writes the result back. Creating an audience or adding members sends
//! nothing — they only define who a later send would reach.

use std::fmt::Write as _;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's audiences (segment = saved filter, static = fixed list).
    List {
        /// Print the raw JSON instead of the compact list.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a reusable audience. `--kind segment` needs at least one filter
    /// flag; `--kind static` starts empty (fill it with add-members).
    Create {
        /// Display name for the audience.
        #[arg(long)]
        name: String,
        /// "segment" (a saved filter that re-resolves against the CRM) or "static" (a fixed list).
        #[arg(long, value_parser = ["segment", "static"])]
        kind: String,
        /// Optional description.
        #[arg(long)]
        description: Option<String>,
        /// Segment filter: contacts carrying ALL of these tag ids, comma-separated.
        #[arg(long = "tags", value_delimiter = ',')]
        tag_ids: Option<Vec<String>>,
        /// Segment filter: contacts in this funnel.
        #[arg(long = "funnel")]
        funnel_id: Option<String>,
        /// Segment filter: contacts at this funnel stage.
        #[arg(long = "stage")]
        stage_id: Option<String>,
        /// Segment filter: contacts created on/after this RFC3339 timestamp.
        #[arg(long = "created-after")]
        created_after: Option<String>,
        /// Segment filter: contacts created on/before this RFC3339 timestamp.
        #[arg(long = "created-before")]
        created_before: Option<String>,
        /// Segment filter: only contacts that have an email address.
        #[arg(long = "has-email")]
        has_email: bool,
        /// Segment filter: only contacts that have a phone number.
        #[arg(long = "has-phone")]
        has_phone: bool,
        /// Print the raw JSON response instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// The contacts currently in an audience (a segment shows whatever was
    /// last resolved — run `resolve` first if the CRM has moved on).
    Members {
        /// The audience id from `smoo audiences list`.
        audience_id: String,
        /// Max members to return (1-200, default 50).
        #[arg(long)]
        limit: Option<u64>,
        /// Rows to skip, for paging through a large audience.
        #[arg(long)]
        offset: Option<u64>,
        /// Print the raw JSON instead of the compact list.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Add named contacts to a STATIC audience (idempotent; sends nothing).
    AddMembers {
        /// The audience id from `smoo audiences list` — must be a static audience.
        audience_id: String,
        /// Contact ids to add, comma-separated.
        #[arg(long, value_delimiter = ',', required = true)]
        contacts: Vec<String>,
        /// Print the raw JSON response instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Work out who an audience currently matches. PREVIEW by default — pass
    /// --materialize to write the result back as the stored membership.
    Resolve {
        /// The audience id from `smoo audiences list`.
        audience_id: String,
        /// Write the resolved membership back to the audience.
        #[arg(long)]
        materialize: bool,
        /// Print the raw JSON response instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[allow(clippy::too_many_lines)] // one arm per verb, same shape as the sibling modules
pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List { json: as_json, org } => {
            let o = require_active_org(&client, org)?;
            let body = client.get(&format!("/organizations/{o}/audiences")).await.context("GET audiences")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            let rows = body.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if rows.is_empty() {
                println!("\nNo audiences defined for this org. This is a confirmed read, not a read failure — create one with `smoo audiences create`.\n");
                return Ok(());
            }
            println!();
            for r in &rows {
                let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let kind = r.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                println!("  {id}  {name}  [{kind}]");
            }
            println!("\n{} audience(s).\n", rows.len());
        }
        Cmd::Create {
            name,
            kind,
            description,
            tag_ids,
            funnel_id,
            stage_id,
            created_after,
            created_before,
            has_email,
            has_phone,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let payload = create_body(&CreateInput {
                name,
                kind,
                description,
                tag_ids,
                funnel_id,
                stage_id,
                created_after,
                created_before,
                has_email,
                has_phone,
            })?;
            let body = client
                .post(&format!("/organizations/{o}/audiences"), Some(&payload))
                .await
                .context("POST audience")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            let id = body.get("id").and_then(Value::as_str).unwrap_or("?");
            let kind = body.get("kind").and_then(Value::as_str).unwrap_or("?");
            println!("\nCreated {kind} audience {id}. Creating an audience sends nothing — it only defines who a later send would reach.\n");
        }
        Cmd::Members {
            audience_id,
            limit,
            offset,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let path = format!(
                "/organizations/{o}/audiences/{}/members?limit={}&offset={}",
                urlencoding::encode(audience_id.trim()),
                limit.unwrap_or(50).clamp(1, 200),
                offset.unwrap_or(0)
            );
            let body = client.get(&path).await.context("GET audience members")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            let rows = body.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if rows.is_empty() {
                println!(
                    "\nAudience {audience_id} has no members in this page. This is a confirmed read, not a read failure — a segment audience reports zero until it has been resolved.\n"
                );
                return Ok(());
            }
            println!();
            for r in &rows {
                let id = r.get("contactId").and_then(|v| v.as_str()).unwrap_or("?");
                let email = r.get("contactEmail").and_then(|v| v.as_str()).unwrap_or("");
                let first = r.get("contactFirstName").and_then(|v| v.as_str()).unwrap_or("");
                let last = r.get("contactLastName").and_then(|v| v.as_str()).unwrap_or("");
                println!("  {id}  {first} {last}  {email}");
            }
            let total = body.get("total").and_then(Value::as_u64);
            match total {
                Some(t) if t > rows.len() as u64 => println!("\nShowing {} of {t} member(s) — page the rest with --offset.\n", rows.len()),
                _ => println!("\n{} member(s) shown.\n", rows.len()),
            }
        }
        Cmd::AddMembers {
            audience_id,
            contacts,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            if contacts.is_empty() {
                bail!("no contact ids given — name the contacts to add with --contacts");
            }
            let asked = contacts.len();
            let body = client
                .post(
                    &format!("/organizations/{o}/audiences/{}/members", urlencoding::encode(audience_id.trim())),
                    Some(&json!({ "contactIds": contacts })),
                )
                .await
                .context("POST audience members")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            println!("\n{}\n", render_add_members(&body, asked, &audience_id));
        }
        Cmd::Resolve {
            audience_id,
            materialize,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let body = client
                .post(
                    &format!("/organizations/{o}/audiences/{}/resolve", urlencoding::encode(audience_id.trim())),
                    Some(&json!({ "materialize": materialize })),
                )
                .await
                .context("POST audience resolve")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            println!("\n{}\n", render_resolve(&body, &audience_id, materialize));
        }
    }
    Ok(())
}

/// Resolved `create` flags, separate from clap so the payload shaping and the
/// segment-needs-a-filter guard are unit-testable without a `Cmd`.
struct CreateInput {
    name: String,
    kind: String,
    description: Option<String>,
    tag_ids: Option<Vec<String>>,
    funnel_id: Option<String>,
    stage_id: Option<String>,
    created_after: Option<String>,
    created_before: Option<String>,
    has_email: bool,
    has_phone: bool,
}

/// Build the POST body. Refuses a segment with no predicates — one with none
/// matches EVERY contact in the org, which is an expensive mistake downstream.
fn create_body(input: &CreateInput) -> Result<Value> {
    let name = input.name.trim();
    if name.is_empty() {
        bail!("--name is empty — an audience needs a name to be reusable");
    }

    let mut filter = serde_json::Map::new();
    if let Some(tags) = input.tag_ids.as_ref().filter(|t| !t.is_empty()) {
        filter.insert("tagIds".to_string(), json!(tags));
    }
    for (key, value) in [
        ("funnelId", input.funnel_id.as_deref()),
        ("stageId", input.stage_id.as_deref()),
        ("createdAfter", input.created_after.as_deref()),
        ("createdBefore", input.created_before.as_deref()),
    ] {
        if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
            filter.insert(key.to_string(), json!(v));
        }
    }
    if input.has_email {
        filter.insert("hasEmail".to_string(), json!(true));
    }
    if input.has_phone {
        filter.insert("hasPhone".to_string(), json!(true));
    }
    if input.kind == "segment" && filter.is_empty() {
        bail!(
            "a segment audience needs at least one filter — one with none matches EVERY contact in the org. \
             Pass a filter flag, or use --kind static and add the contacts by id."
        );
    }

    let mut body = json!({ "name": name, "kind": input.kind });
    if let Some(d) = input.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        body["description"] = json!(d);
    }
    if !filter.is_empty() {
        body["filter"] = Value::Object(filter);
    }
    Ok(body)
}

/// Human summary of an add-members response. Reports the gap, not just the
/// win — a lower count means ids that were already members or belong to
/// another org, and hiding that would hide a wrong id.
fn render_add_members(body: &Value, asked: usize, audience_id: &str) -> String {
    let added = body
        .get("addedCount")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
    let note = if added < asked {
        format!(" The other {} were already members or are not contacts in this org.", asked - added)
    } else {
        String::new()
    };
    format!("Added {added} of {asked} contact(s) to audience {audience_id}.{note}")
}

/// How many resolved contact ids we print before summarizing.
const MAX_RENDER_IDS: usize = 50;

/// Human summary of a resolve response — always says whether the stored
/// membership changed, and reports any truncation of the id list.
fn render_resolve(body: &Value, audience_id: &str, materialize: bool) -> String {
    let matched = body.get("matchedCount").map_or_else(|| "0".to_string(), ToString::to_string);
    let kind = body.get("kind").and_then(Value::as_str).unwrap_or("audience");
    let mut out = format!("Audience {audience_id} ({kind}) currently matches {matched} contact(s).");
    if let Some(ids) = body.get("contactIds").and_then(|v| v.as_array()) {
        let shown: Vec<&str> = ids.iter().take(MAX_RENDER_IDS).filter_map(Value::as_str).collect();
        let _ = write!(out, "\nContact ids: {}", shown.join(", "));
        if ids.len() > shown.len() {
            let _ = write!(
                out,
                "\n(Showing {} of {} ids — page the rest with `smoo audiences members`.)",
                shown.len(),
                ids.len()
            );
        }
    }
    if materialize {
        let written = body.get("materializedCount").map_or_else(|| "0".to_string(), ToString::to_string);
        let _ = write!(out, "\nMembership written back: {written} contact(s).");
    } else {
        out.push_str("\nThis was a preview — the audience's stored membership was NOT changed. Pass --materialize to write it.");
    }
    out
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::json;

    use super::*;

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    fn input(kind: &str) -> CreateInput {
        CreateInput {
            name: "Hot leads".to_string(),
            kind: kind.to_string(),
            description: None,
            tag_ids: None,
            funnel_id: None,
            stage_id: None,
            created_after: None,
            created_before: None,
            has_email: false,
            has_phone: false,
        }
    }

    #[test]
    fn every_verb_parses() {
        assert!(matches!(Wrap::try_parse_from(["t", "list"]).expect("list").cmd, Cmd::List { json: false, .. }));
        assert!(matches!(
            Wrap::try_parse_from(["t", "list", "--json"]).expect("list json").cmd,
            Cmd::List { json: true, .. }
        ));
        match Wrap::try_parse_from(["t", "create", "--name", "VIPs", "--kind", "static"]).expect("create").cmd {
            Cmd::Create { name, kind, .. } => {
                assert_eq!(name, "VIPs");
                assert_eq!(kind, "static");
            }
            _ => panic!("expected Create"),
        }
        assert!(
            Wrap::try_parse_from(["t", "create", "--name", "x", "--kind", "fuzzy"]).is_err(),
            "unknown kind must be refused"
        );
        match Wrap::try_parse_from(["t", "members", "aud-1", "--limit", "10", "--offset", "20"])
            .expect("members")
            .cmd
        {
            Cmd::Members {
                audience_id, limit, offset, ..
            } => {
                assert_eq!(audience_id, "aud-1");
                assert_eq!(limit, Some(10));
                assert_eq!(offset, Some(20));
            }
            _ => panic!("expected Members"),
        }
        match Wrap::try_parse_from(["t", "add-members", "aud-1", "--contacts", "a,b"])
            .expect("add-members")
            .cmd
        {
            Cmd::AddMembers { contacts, .. } => assert_eq!(contacts, vec!["a", "b"]),
            _ => panic!("expected AddMembers"),
        }
        assert!(Wrap::try_parse_from(["t", "add-members", "aud-1"]).is_err(), "add-members requires --contacts");
        assert!(matches!(
            Wrap::try_parse_from(["t", "resolve", "aud-1"]).expect("resolve").cmd,
            Cmd::Resolve { materialize: false, .. }
        ));
        assert!(matches!(
            Wrap::try_parse_from(["t", "resolve", "aud-1", "--materialize"])
                .expect("resolve materialize")
                .cmd,
            Cmd::Resolve { materialize: true, .. }
        ));
    }

    #[test]
    fn segment_without_filters_is_refused() {
        let err = create_body(&input("segment")).expect_err("empty segment");
        assert!(format!("{err}").contains("EVERY contact"), "{err}");
    }

    #[test]
    fn static_without_filters_is_fine() {
        let body = create_body(&input("static")).expect("static");
        assert_eq!(body, json!({ "name": "Hot leads", "kind": "static" }));
    }

    #[test]
    fn empty_name_is_refused() {
        let mut i = input("static");
        i.name = "  ".to_string();
        assert!(create_body(&i).is_err());
    }

    #[test]
    fn segment_filters_map_to_camel_case() {
        let mut i = input("segment");
        i.description = Some("recent, reachable".to_string());
        i.tag_ids = Some(vec!["t1".to_string(), "t2".to_string()]);
        i.funnel_id = Some("f1".to_string());
        i.created_after = Some("2026-01-01T00:00:00Z".to_string());
        i.has_email = true;
        let body = create_body(&i).expect("segment");
        assert_eq!(body["description"], json!("recent, reachable"));
        assert_eq!(body["filter"]["tagIds"], json!(["t1", "t2"]));
        assert_eq!(body["filter"]["funnelId"], json!("f1"));
        assert_eq!(body["filter"]["createdAfter"], json!("2026-01-01T00:00:00Z"));
        assert_eq!(body["filter"]["hasEmail"], json!(true));
        assert!(body["filter"].get("hasPhone").is_none(), "unset boolean filters stay absent");
    }

    #[test]
    fn add_members_reports_the_gap() {
        let full = render_add_members(&json!({ "addedCount": 2 }), 2, "aud-1");
        assert_eq!(full, "Added 2 of 2 contact(s) to audience aud-1.");
        let partial = render_add_members(&json!({ "addedCount": 1 }), 3, "aud-1");
        assert!(partial.contains("Added 1 of 3"), "{partial}");
        assert!(partial.contains("The other 2"), "{partial}");
    }

    #[test]
    fn resolve_preview_says_nothing_changed() {
        let out = render_resolve(&json!({ "matchedCount": 4, "kind": "segment" }), "aud-1", false);
        assert!(out.contains("matches 4 contact(s)"), "{out}");
        assert!(out.contains("NOT changed"), "{out}");
        let written = render_resolve(&json!({ "matchedCount": 4, "materializedCount": 4 }), "aud-1", true);
        assert!(written.contains("written back: 4"), "{written}");
    }

    #[test]
    fn resolve_reports_id_truncation() {
        let ids: Vec<String> = (0..60).map(|i| format!("c{i}")).collect();
        let out = render_resolve(&json!({ "matchedCount": 60, "contactIds": ids }), "aud-1", false);
        assert!(out.contains("Showing 50 of 60 ids"), "{out}");
    }
}

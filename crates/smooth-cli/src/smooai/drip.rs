//! `smoo drip …` — drip (nurture) sequences: list, inspect, enrollments,
//! enroll/cancel, and a test send to your own inbox. CLI twin of the hosted
//! MCP `drip_*` tools (pearl th-b1f09c).
//!
//! `enroll` takes explicit contact ids only (never a whole segment) and is
//! capped at 25 per call, mirroring the MCP tool: bulk sending is what
//! campaigns are for. Suppression (unsubscribed / opted out) is enforced by
//! the SERVER per contact and reported back — never re-implemented here.

use std::fmt::Write as _;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};

use super::{print_json, require_active_org, require_authed};

/// Most contacts a single `enroll` call will start — sales-sequence sized on
/// purpose, same cap as the hosted MCP tool.
const MAX_ENROLL_COHORT: usize = 25;

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's drip sequences.
    Sequences {
        /// Print the raw JSON instead of the compact list.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// One drip sequence with its ordered steps (send / wait / branch).
    Show {
        /// The sequence id from `smoo drip sequences`.
        sequence_id: String,
        /// Print the raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Who is enrolled in a sequence, with per-status counts.
    Enrollments {
        /// The sequence id from `smoo drip sequences`.
        sequence_id: String,
        /// Only enrollments in this status (e.g. active, completed, stopped).
        #[arg(long)]
        status: Option<String>,
        /// Print the raw JSON instead of the compact list.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Enrol SPECIFIC contacts (by id) into a sequence — real email reaches
    /// real people. Max 25 per call; suppressed contacts are refused by the
    /// server and reported, never silently dropped.
    Enroll {
        /// The sequence id from `smoo drip sequences`.
        sequence_id: String,
        /// Contact ids to enrol, comma-separated (from `smoo crm` / audience members).
        #[arg(long, value_delimiter = ',', required = true)]
        contacts: Vec<String>,
        /// Print the raw JSON response instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Stop ONE contact's enrollment, leaving everyone else's running.
    Cancel {
        /// The sequence id from `smoo drip sequences`.
        sequence_id: String,
        /// The contact whose enrollment should stop.
        contact_id: String,
        /// Print the raw JSON response instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Render drip copy with sample variables and email it to YOUR OWN inbox
    /// only — never to a contact. Requires a signed-in user session.
    TestSend {
        /// The sequence id — supplies the sender identity and sample variables.
        sequence_id: String,
        /// Subject line to render and preview.
        #[arg(long)]
        subject: String,
        /// Email body to render and preview (template variables get sample values).
        #[arg(long)]
        body: String,
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
        Cmd::Sequences { json: as_json, org } => {
            let o = require_active_org(&client, org)?;
            let body = client.get(&format!("/organizations/{o}/drip-sequences")).await.context("GET drip sequences")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            let rows = body.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if rows.is_empty() {
                println!("\nNo drip sequences in this org. This is a confirmed read, not a read failure.\n");
                return Ok(());
            }
            println!();
            for r in &rows {
                let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let channel = r.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                println!("  {id}  {name}  [{status}] {channel}");
            }
            println!("\n{} sequence(s).\n", rows.len());
        }
        Cmd::Show {
            sequence_id,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!("/organizations/{o}/drip-sequences/{}", urlencoding::encode(sequence_id.trim())))
                .await
                .context("GET drip sequence")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            println!("\n{}\n", render_sequence(&body));
        }
        Cmd::Enrollments {
            sequence_id,
            status,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let mut path = format!("/organizations/{o}/drip-sequences/{}/enrollments", urlencoding::encode(sequence_id.trim()));
            if let Some(s) = status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                let _ = write!(path, "?status={}", urlencoding::encode(s));
            }
            let body = client.get(&path).await.context("GET drip enrollments")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            println!("\n{}\n", render_enrollments(&body));
        }
        Cmd::Enroll {
            sequence_id,
            contacts,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            validate_cohort(contacts.len())?;
            let asked = contacts.len();
            let body = client
                .post(
                    &format!("/organizations/{o}/drip-sequences/{}/enroll", urlencoding::encode(sequence_id.trim())),
                    Some(&json!({ "contactIds": contacts })),
                )
                .await
                .context("POST drip enroll")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            println!("\n{}\n", render_enroll(&body, asked));
        }
        Cmd::Cancel {
            sequence_id,
            contact_id,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let body = client
                .post(
                    &format!(
                        "/organizations/{o}/drip-sequences/{}/enrollments/{}/cancel",
                        urlencoding::encode(sequence_id.trim()),
                        urlencoding::encode(contact_id.trim())
                    ),
                    Some(&json!({})),
                )
                .await
                .context("POST drip cancel enrollment")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            // The route distinguishes a real cancel from an idempotent repeat —
            // say which, so a no-op isn't reported as a cancel.
            if body.get("cancelled").and_then(Value::as_bool).unwrap_or(false) {
                println!("\nEnrollment cancelled — that contact receives no further steps. Everyone else is unaffected.\n");
            } else {
                println!("\nThat enrollment was already cancelled — nothing changed.\n");
            }
        }
        Cmd::TestSend {
            sequence_id,
            subject,
            body: email_body,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let body = client
                .post(
                    &format!("/organizations/{o}/drip-sequences/{}/test-send", urlencoding::encode(sequence_id.trim())),
                    Some(&json!({ "subject": subject, "body": email_body })),
                )
                .await
                .context("POST drip test send")?;
            if as_json {
                print_json(&body);
                return Ok(());
            }
            let to = body.get("sentTo").and_then(Value::as_str).unwrap_or("your account email");
            println!("\nTest email sent to {to} (nobody else received it).\n");
        }
    }
    Ok(())
}

/// Refuse an empty or oversized enroll cohort. Refuses rather than truncating:
/// silently enrolling the first 25 of 500 and reporting success would hide the
/// gap from the person who believes the job is done.
fn validate_cohort(count: usize) -> Result<()> {
    if count == 0 {
        bail!("no contact ids given — name the contacts to enrol with --contacts");
    }
    if count > MAX_ENROLL_COHORT {
        bail!(
            "{count} contacts is more than this command will enrol at once (max {MAX_ENROLL_COHORT}). \
             It's for one-off and small-cohort follow-up — use a campaign for a bulk send, or enrol in smaller batches."
        );
    }
    Ok(())
}

/// Human summary of one sequence + its steps.
fn render_sequence(body: &Value) -> String {
    let s = |key: &str| body.get(key).and_then(Value::as_str).unwrap_or("?");
    let mut out = format!("{}  {}  [{}] {}", s("id"), s("name"), s("status"), s("channel"));
    let steps = body.get("steps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if steps.is_empty() {
        out.push_str("\n\nNo steps configured — this sequence would send nothing.");
        return out;
    }
    let _ = write!(out, "\n\nSteps ({}):", steps.len());
    for (i, step) in steps.iter().enumerate() {
        let kind = step.get("kind").and_then(Value::as_str).unwrap_or("?");
        let subject = step.get("subject").and_then(Value::as_str).unwrap_or("");
        let wait = step
            .get("waitDuration")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| step.get("waitSeconds").and_then(Value::as_u64).map(|n| format!("{n}s")));
        let _ = write!(out, "\n{}. {kind}  {subject}", i + 1);
        if let Some(w) = wait {
            let _ = write!(out, "  (wait {w})");
        }
    }
    out
}

/// Human summary of the enrollments list + the per-status counts map (which
/// the paginated rows can't convey).
fn render_enrollments(body: &Value) -> String {
    let rows = body.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut out = if rows.is_empty() {
        "No enrollments matched. This is a confirmed read, not a read failure.".to_string()
    } else {
        let mut s = String::new();
        for r in &rows {
            let email = r.get("contactEmail").and_then(Value::as_str).unwrap_or("?");
            let status = r.get("status").and_then(Value::as_str).unwrap_or("?");
            let contact = r.get("contactId").and_then(Value::as_str).unwrap_or("?");
            let _ = writeln!(s, "  {contact}  {email}  [{status}]");
        }
        let _ = write!(s, "\n{} enrollment(s) shown.", rows.len());
        s
    };
    if let Some(counts) = body.get("counts").and_then(|c| c.as_object()).filter(|c| !c.is_empty()) {
        let summary = counts.iter().map(|(status, n)| format!("{status}: {n}")).collect::<Vec<_>>().join(", ");
        let _ = write!(out, "\nTotals by status — {summary}");
    }
    out
}

/// Human summary of an enroll response. Reports BOTH halves — the server's
/// suppression refusals and per-contact failures are the compliance answer
/// for why someone wasn't contacted.
fn render_enroll(body: &Value, asked: usize) -> String {
    let enrolled = body.get("enrolled").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let skipped = body.get("skipped").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let failed: Vec<&Value> = enrolled.iter().filter(|r| r.get("error").is_some_and(|e| !e.is_null())).collect();
    let started = enrolled.len() - failed.len();

    let mut out = format!("Enrolled {started} of {asked} contact(s).");
    if !skipped.is_empty() {
        let _ = write!(out, "\n\nRefused by the server ({}):", skipped.len());
        for row in &skipped {
            let who = row.get("contactId").and_then(Value::as_str).unwrap_or("?");
            let why = row.get("reason").and_then(Value::as_str).unwrap_or("?");
            let _ = write!(out, "\n- {who}: {why}");
        }
        out.push_str("\n(`unsubscribed` and `opted_out` are suppression rules — they cannot be overridden from here.)");
    }
    if !failed.is_empty() {
        let _ = write!(out, "\n\nFailed to start ({}):", failed.len());
        for row in &failed {
            let who = row.get("contactId").and_then(Value::as_str).unwrap_or("?");
            let err = row.get("error").map(ToString::to_string).unwrap_or_default();
            let _ = write!(out, "\n- {who}: {err}");
        }
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

    #[test]
    fn every_verb_parses() {
        assert!(matches!(
            Wrap::try_parse_from(["t", "sequences"]).expect("sequences").cmd,
            Cmd::Sequences { json: false, .. }
        ));
        assert!(matches!(
            Wrap::try_parse_from(["t", "sequences", "--json"]).expect("sequences json").cmd,
            Cmd::Sequences { json: true, .. }
        ));
        assert!(matches!(Wrap::try_parse_from(["t", "show", "seq-1"]).expect("show").cmd, Cmd::Show { .. }));
        match Wrap::try_parse_from(["t", "enrollments", "seq-1", "--status", "active"])
            .expect("enrollments")
            .cmd
        {
            Cmd::Enrollments { sequence_id, status, .. } => {
                assert_eq!(sequence_id, "seq-1");
                assert_eq!(status.as_deref(), Some("active"));
            }
            _ => panic!("expected Enrollments"),
        }
        match Wrap::try_parse_from(["t", "enroll", "seq-1", "--contacts", "a,b,c"]).expect("enroll").cmd {
            Cmd::Enroll { contacts, .. } => assert_eq!(contacts, vec!["a", "b", "c"]),
            _ => panic!("expected Enroll"),
        }
        assert!(Wrap::try_parse_from(["t", "enroll", "seq-1"]).is_err(), "enroll requires --contacts");
        match Wrap::try_parse_from(["t", "cancel", "seq-1", "contact-1"]).expect("cancel").cmd {
            Cmd::Cancel { sequence_id, contact_id, .. } => {
                assert_eq!(sequence_id, "seq-1");
                assert_eq!(contact_id, "contact-1");
            }
            _ => panic!("expected Cancel"),
        }
        match Wrap::try_parse_from(["t", "test-send", "seq-1", "--subject", "Hi", "--body", "Hello {{name}}"])
            .expect("test-send")
            .cmd
        {
            Cmd::TestSend { subject, body, .. } => {
                assert_eq!(subject, "Hi");
                assert_eq!(body, "Hello {{name}}");
            }
            _ => panic!("expected TestSend"),
        }
        assert!(
            Wrap::try_parse_from(["t", "test-send", "seq-1", "--subject", "Hi"]).is_err(),
            "test-send requires --body"
        );
    }

    #[test]
    fn cohort_guard_refuses_empty_and_oversized() {
        assert!(validate_cohort(0).is_err());
        assert!(validate_cohort(1).is_ok());
        assert!(validate_cohort(MAX_ENROLL_COHORT).is_ok());
        let err = validate_cohort(MAX_ENROLL_COHORT + 1).expect_err("over cap");
        assert!(format!("{err}").contains("max 25"), "{err}");
    }

    #[test]
    fn render_enroll_reports_skips_and_failures() {
        let body = json!({
            "enrolled": [
                { "contactId": "c1" },
                { "contactId": "c2", "error": "boom" },
            ],
            "skipped": [{ "contactId": "c3", "reason": "unsubscribed" }],
        });
        let out = render_enroll(&body, 3);
        assert!(out.contains("Enrolled 1 of 3"), "{out}");
        assert!(out.contains("c3: unsubscribed"), "{out}");
        assert!(out.contains("Failed to start (1)"), "{out}");
        assert!(out.contains("cannot be overridden"), "{out}");
    }

    #[test]
    fn render_enrollments_empty_is_a_real_answer() {
        let out = render_enrollments(&json!({ "data": [], "counts": { "active": 2 } }));
        assert!(out.contains("confirmed read"), "{out}");
        assert!(out.contains("active: 2"), "{out}");
    }

    #[test]
    fn render_sequence_flags_empty_steps() {
        let out = render_sequence(&json!({ "id": "s1", "name": "Welcome", "status": "active", "channel": "email", "steps": [] }));
        assert!(out.contains("would send nothing"), "{out}");
        let with_steps = render_sequence(&json!({
            "id": "s1", "name": "Welcome", "status": "active", "channel": "email",
            "steps": [
                { "kind": "send", "subject": "Hi" },
                { "kind": "wait", "waitSeconds": 3600 },
            ],
        }));
        assert!(with_steps.contains("Steps (2):"), "{with_steps}");
        assert!(with_steps.contains("wait 3600s"), "{with_steps}");
    }
}

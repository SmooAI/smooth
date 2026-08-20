//! `smoo forms …` — Google Forms this org created, and their responses.
//! CLI twin of the hosted MCP `forms_list` tool (pearl th-a5d991).

use anstream::println;
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;

use super::{print_json, require_active_org, require_authed};

/// Responses rendered per call — a popular form is thousands of rows, and the
/// cut is always stated in the output. Mirrors the MCP tool's cap.
const MAX_FORM_RESPONSES: usize = 50;

#[derive(Subcommand)]
pub enum Cmd {
    /// List the Google Forms this org created through Smoo (title, question
    /// count, share link), or pass a `formId` from the list to read that
    /// form's submitted responses with the question titles filled in.
    /// Requires a signed-in user session (not an org API key).
    List {
        /// A Google `formId` from the list — reads that form's responses.
        form_id: Option<String>,
        /// Print raw JSON instead of the compact listing.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List { form_id, json, org } => {
            let o = require_active_org(&client, org)?;
            let Some(form_id) = form_id else {
                let resp = client.get(&format!("/organizations/{o}/google-forms")).await.context("GET google-forms")?;
                if json {
                    print_json(&resp);
                } else {
                    print_forms(&resp);
                }
                return Ok(());
            };
            let resp = client
                .get(&format!("/organizations/{o}/google-forms/{}/responses", urlencoding::encode(&form_id)))
                .await
                .context("GET form responses")?;
            if json {
                print_json(&resp);
            } else {
                print_responses(&resp);
            }
        }
    }
    Ok(())
}

fn print_forms(body: &serde_json::Value) {
    // Enveloped `{items: […]}` or a bare top-level array — the routes answer
    // with either (same contract as the MCP server's `rows()`).
    let Some(items) = body.as_array().or_else(|| body.get("items").and_then(|v| v.as_array())) else {
        print_json(body);
        return;
    };
    println!();
    if items.is_empty() {
        println!("  {} {}", "●".dimmed(), "No forms exist yet.".dimmed());
        println!();
        return;
    }
    for f in items {
        let id = f.get("formId").and_then(|v| v.as_str()).unwrap_or("?");
        let title = f.get("title").and_then(|v| v.as_str()).unwrap_or("(untitled)");
        let questions = f.get("questionCount").and_then(serde_json::Value::as_u64).unwrap_or(0);
        println!(
            "  {} {} {} {}",
            "○".dimmed(),
            id.cyan(),
            title.bold(),
            format!("({questions} questions)").dimmed()
        );
        if let Some(uri) = f.get("responderUri").and_then(|v| v.as_str()) {
            println!("      {}", uri.dimmed());
        }
    }
    if let Some(total) = body.get("total").and_then(serde_json::Value::as_u64) {
        if total > items.len() as u64 {
            println!();
            println!("  Showing {} of {total} forms.", items.len());
        }
    }
    println!();
}

fn print_responses(body: &serde_json::Value) {
    let title = body.get("title").and_then(|v| v.as_str()).unwrap_or("(untitled)");
    let titles = body.get("questionTitles").and_then(|t| t.as_object());
    let responses = body.get("responses").and_then(|r| r.as_array()).map(Vec::as_slice).unwrap_or_default();
    println!();
    if responses.is_empty() {
        println!("  {} — no responses submitted yet.", title.bold());
        println!();
        return;
    }
    println!("  {} — {} response(s)", title.bold(), responses.len());
    for (i, response) in responses.iter().take(MAX_FORM_RESPONSES).enumerate() {
        let submitted = response.get("submittedAt").and_then(|v| v.as_str()).unwrap_or("");
        println!();
        println!("  {}. {}", i + 1, submitted.dimmed());
        let Some(answers) = response.get("answers").and_then(|a| a.as_object()) else {
            continue;
        };
        for (question_id, values) in answers {
            // Fall back to the raw id rather than dropping the answer: an
            // unmapped question (added after form creation) still carries a reply.
            let label = titles
                .and_then(|t| t.get(question_id))
                .and_then(serde_json::Value::as_str)
                .filter(|t| !t.is_empty())
                .unwrap_or(question_id);
            let text = answer_text(values);
            println!("     {label}: {text}");
        }
    }
    if responses.len() > MAX_FORM_RESPONSES {
        println!();
        println!("  Showing {MAX_FORM_RESPONSES} of {} responses.", responses.len());
    }
    println!();
}

/// One answer's values joined — the route sends each answer as an array of strings.
fn answer_text(values: &serde_json::Value) -> String {
    values
        .as_array()
        .map(|vs| vs.iter().filter_map(serde_json::Value::as_str).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::json;

    use super::{answer_text, Cmd};

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[test]
    fn list_parses_bare() {
        let w = Wrap::try_parse_from(["t", "list"]).expect("bare list must parse");
        assert!(matches!(
            w.cmd,
            Cmd::List {
                form_id: None,
                json: false,
                org: None
            }
        ));
    }

    #[test]
    fn list_parses_form_id_positional() {
        let w = Wrap::try_parse_from(["t", "list", "abc123"]).expect("form id must parse");
        match w.cmd {
            Cmd::List { form_id, .. } => assert_eq!(form_id.as_deref(), Some("abc123")),
        }
    }

    #[test]
    fn list_parses_json_and_org_flags() {
        let w = Wrap::try_parse_from(["t", "list", "--json", "--org-id", "o1"]).expect("flags must parse");
        match w.cmd {
            Cmd::List { json, org, .. } => {
                assert!(json);
                assert_eq!(org.as_deref(), Some("o1"));
            }
        }
    }

    #[test]
    fn answer_text_joins_values() {
        assert_eq!(answer_text(&json!(["a", "b"])), "a, b");
        assert_eq!(answer_text(&json!([])), "");
        assert_eq!(answer_text(&json!("not-an-array")), "");
    }
}

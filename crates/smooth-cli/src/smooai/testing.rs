//! `th testing …` — testing platform (deployments / cases /
//! environments / runs).

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Subcommand;
use serde::Deserialize;
use serde_json::{json, Value};
use smooth_api_client::SmoothApiClient;

use super::{print_json, print_list_envelope, read_body, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    Deployments {
        #[command(subcommand)]
        cmd: DeploymentsCmd,
    },
    Cases {
        #[command(subcommand)]
        cmd: CasesCmd,
    },
    Environments {
        #[command(subcommand)]
        cmd: EnvironmentsCmd,
    },
    Runs {
        #[command(subcommand)]
        cmd: RunsCmd,
    },
}

#[derive(Subcommand)]
pub enum DeploymentsCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Show {
        deployment_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Create {
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        deployment_id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        deployment_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum CasesCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Show {
        case_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Create {
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        case_id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        case_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum EnvironmentsCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Create {
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        env_id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        env_id: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RunsCmd {
    List {
        #[arg(long)]
        org: Option<String>,
    },
    Show {
        run_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    Create {
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Update {
        run_id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Delete {
        run_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Submit results for a run. Body is optional JSON.
    Results {
        run_id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    /// High-level: create a run, submit a CTRF (or JUnit) report, and
    /// return the completed run. Reads `<file>` as CTRF JSON, or — with
    /// `--junit` (or a `.xml` extension) — converts a JUnit report (e.g.
    /// cargo-nextest output) to CTRF first. Mirrors `@smooai/testing runs
    /// report`, so CI needs no separate junit-to-ctrf step.
    Report {
        /// Path to a CTRF JSON report (or a JUnit XML report with --junit).
        file: String,
        /// Run name (defaults to the report file's base name).
        #[arg(long)]
        name: Option<String>,
        /// Environment name to associate the run with.
        #[arg(long)]
        environment: Option<String>,
        /// Tool name (defaults to the CTRF report's `results.tool.name`).
        #[arg(long)]
        tool: Option<String>,
        /// Comma-separated tags.
        #[arg(long)]
        tags: Option<String>,
        /// Associate the run with an existing deployment.
        #[arg(long)]
        deployment_id: Option<String>,
        /// Build name to link the run to (defaults to $GITHUB_SHA in CI).
        #[arg(long)]
        build_name: Option<String>,
        /// Build URL to link the run to (defaults to the GitHub Actions run URL).
        #[arg(long)]
        build_url: Option<String>,
        /// Treat `<file>` as JUnit XML and convert it to CTRF before submitting.
        #[arg(long)]
        junit: bool,
        /// Also report to these orgs (comma-separated) in addition to the active org.
        #[arg(long)]
        additional_org_ids: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    let opt_body = |body: Option<String>| -> Result<Option<serde_json::Value>> {
        match body {
            Some(p) => Ok(Some(read_body(&p)?)),
            None => Ok(None),
        }
    };
    match cmd {
        Cmd::Deployments { cmd } => match cmd {
            DeploymentsCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(
                    &client
                        .get(&format!("/organizations/{o}/testing/deployments"))
                        .await
                        .context("GET deployments")?,
                    "deployments",
                );
            }
            DeploymentsCmd::Show { deployment_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .get(&format!("/organizations/{o}/testing/deployments/{deployment_id}"))
                        .await
                        .context("GET deployment")?,
                );
            }
            DeploymentsCmd::Create { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?;
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/testing/deployments"), b.as_ref())
                        .await
                        .context("POST deployment")?,
                );
            }
            DeploymentsCmd::Update { deployment_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?.unwrap_or_else(|| serde_json::json!({}));
                print_json(
                    &client
                        .patch(&format!("/organizations/{o}/testing/deployments/{deployment_id}"), &b)
                        .await
                        .context("PATCH deployment")?,
                );
            }
            DeploymentsCmd::Delete { deployment_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/testing/deployments/{deployment_id}"))
                        .await
                        .context("DELETE deployment")?,
                );
            }
        },
        Cmd::Cases { cmd } => match cmd {
            CasesCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(&client.get(&format!("/organizations/{o}/testing/cases")).await.context("GET cases")?, "cases");
            }
            CasesCmd::Show { case_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(&client.get(&format!("/organizations/{o}/testing/cases/{case_id}")).await.context("GET case")?);
            }
            CasesCmd::Create { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?;
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/testing/cases"), b.as_ref())
                        .await
                        .context("POST case")?,
                );
            }
            CasesCmd::Update { case_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?.unwrap_or_else(|| serde_json::json!({}));
                print_json(
                    &client
                        .patch(&format!("/organizations/{o}/testing/cases/{case_id}"), &b)
                        .await
                        .context("PATCH case")?,
                );
            }
            CasesCmd::Delete { case_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/testing/cases/{case_id}"))
                        .await
                        .context("DELETE case")?,
                );
            }
        },
        Cmd::Environments { cmd } => match cmd {
            EnvironmentsCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(
                    &client
                        .get(&format!("/organizations/{o}/testing/environments"))
                        .await
                        .context("GET test environments")?,
                    "environments",
                );
            }
            EnvironmentsCmd::Create { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?;
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/testing/environments"), b.as_ref())
                        .await
                        .context("POST test environment")?,
                );
            }
            EnvironmentsCmd::Update { env_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?.unwrap_or_else(|| serde_json::json!({}));
                print_json(
                    &client
                        .patch(&format!("/organizations/{o}/testing/environments/{env_id}"), &b)
                        .await
                        .context("PATCH test environment")?,
                );
            }
            EnvironmentsCmd::Delete { env_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/testing/environments/{env_id}"))
                        .await
                        .context("DELETE test environment")?,
                );
            }
        },
        Cmd::Runs { cmd } => match cmd {
            RunsCmd::List { org } => {
                let o = require_active_org(&client, org)?;
                print_list_envelope(&client.get(&format!("/organizations/{o}/testing/runs")).await.context("GET runs")?, "runs");
            }
            RunsCmd::Show { run_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(&client.get(&format!("/organizations/{o}/testing/runs/{run_id}")).await.context("GET run")?);
            }
            RunsCmd::Create { body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?;
                print_json(&client.post(&format!("/organizations/{o}/testing/runs"), b.as_ref()).await.context("POST run")?);
            }
            RunsCmd::Update { run_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?.unwrap_or_else(|| serde_json::json!({}));
                print_json(
                    &client
                        .patch(&format!("/organizations/{o}/testing/runs/{run_id}"), &b)
                        .await
                        .context("PATCH run")?,
                );
            }
            RunsCmd::Delete { run_id, org } => {
                let o = require_active_org(&client, org)?;
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/testing/runs/{run_id}"))
                        .await
                        .context("DELETE run")?,
                );
            }
            RunsCmd::Results { run_id, body, org } => {
                let o = require_active_org(&client, org)?;
                let b = opt_body(body)?;
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/testing/runs/{run_id}/results"), b.as_ref())
                        .await
                        .context("POST run results")?,
                );
            }
            RunsCmd::Report {
                file,
                name,
                environment,
                tool,
                tags,
                deployment_id,
                build_name,
                build_url,
                junit,
                additional_org_ids,
                org,
            } => {
                let primary = require_active_org(&client, org)?;
                let ctrf = load_report(&file, junit)?;

                // Tool defaults to the CTRF report's own tool name; name to the
                // file's base name; build name/url to the GitHub Actions env.
                let tool = tool.or_else(|| ctrf.pointer("/results/tool/name").and_then(|v| v.as_str()).map(String::from));
                let run_name = name.unwrap_or_else(|| default_run_name(&file));
                let build_name = build_name.or_else(|| env_nonempty("GITHUB_SHA"));
                let build_url = build_url.or_else(github_run_url);
                let tag_list = parse_csv(tags.as_deref());

                let mut create = serde_json::Map::new();
                create.insert("name".into(), json!(run_name));
                if let Some(v) = environment {
                    create.insert("environment".into(), json!(v));
                }
                if let Some(v) = deployment_id {
                    create.insert("deploymentId".into(), json!(v));
                }
                if let Some(v) = tool {
                    create.insert("tool".into(), json!(v));
                }
                if let Some(v) = build_name {
                    create.insert("buildName".into(), json!(v));
                }
                if let Some(v) = build_url {
                    create.insert("buildUrl".into(), json!(v));
                }
                if let Some(v) = tag_list {
                    create.insert("tags".into(), json!(v));
                }
                let create_body = Value::Object(create);

                // Active org first, then any --additional-org-ids.
                let mut orgs = vec![primary];
                if let Some(extra) = parse_csv(additional_org_ids.as_deref()) {
                    orgs.extend(extra);
                }
                for org_id in orgs {
                    report_to_org(&client, &org_id, &create_body, &ctrf).await?;
                }
            }
        },
    }
    Ok(())
}

/// Create a run, submit its results, and print the completed run. On a
/// submit failure the run is marked `errored` (so the platform reflects the
/// failed attempt) before the error propagates.
async fn report_to_org(client: &SmoothApiClient, org_id: &str, create_body: &Value, ctrf: &Value) -> Result<()> {
    let run = client
        .post(&format!("/organizations/{org_id}/testing/runs"), Some(create_body))
        .await
        .context("create run")?;
    let run_id = run.get("id").and_then(Value::as_str).context("created run has no id")?.to_string();

    if let Err(err) = client.post(&format!("/organizations/{org_id}/testing/runs/{run_id}/results"), Some(ctrf)).await {
        // Best-effort mark the run errored; surface the original error.
        let _ = client
            .patch(
                &format!("/organizations/{org_id}/testing/runs/{run_id}"),
                &json!({ "status": "errored", "completedAt": Utc::now().to_rfc3339() }),
            )
            .await;
        return Err(err).context("submit results");
    }

    print_json(&client.get(&format!("/organizations/{org_id}/testing/runs/{run_id}")).await.context("GET run")?);
    Ok(())
}

/// Load `<file>` as a CTRF report: parse JSON directly, or convert from JUnit
/// XML when `--junit` is set or the file has a `.xml` extension.
fn load_report(file: &str, junit: bool) -> Result<Value> {
    let is_xml = file.rsplit('.').next().is_some_and(|ext| ext.eq_ignore_ascii_case("xml"));
    if junit || is_xml {
        let raw = std::fs::read_to_string(file).with_context(|| format!("read {file}"))?;
        junit_to_ctrf(&raw).with_context(|| format!("convert JUnit {file} to CTRF"))
    } else {
        read_body(file)
    }
}

/// Run name fallback: the file's base name without its extension.
fn default_run_name(file: &str) -> String {
    std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .map_or_else(|| file.to_string(), String::from)
}

/// Split a comma-separated string into trimmed, non-empty values.
fn parse_csv(raw: Option<&str>) -> Option<Vec<String>> {
    let values: Vec<String> = raw?.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    (!values.is_empty()).then_some(values)
}

/// A trimmed, non-empty env var value.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The GitHub Actions run URL, assembled from the standard env vars.
fn github_run_url() -> Option<String> {
    let server = env_nonempty("GITHUB_SERVER_URL")?;
    let repo = env_nonempty("GITHUB_REPOSITORY")?;
    let run_id = env_nonempty("GITHUB_RUN_ID")?;
    Some(format!("{server}/{repo}/actions/runs/{run_id}"))
}

// ── JUnit → CTRF ──

#[derive(Deserialize)]
struct JunitSuites {
    #[serde(rename = "testsuite", default)]
    testsuites: Vec<JunitSuite>,
}

#[derive(Deserialize)]
struct JunitSuite {
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "testsuite", default)]
    nested: Vec<JunitSuite>,
    #[serde(rename = "testcase", default)]
    testcases: Vec<JunitCase>,
}

#[derive(Deserialize)]
struct JunitCase {
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "@classname", default)]
    classname: String,
    #[serde(rename = "@time", default)]
    time: Option<String>,
    failure: Option<JunitDetail>,
    error: Option<JunitDetail>,
    skipped: Option<JunitSkipped>,
}

#[derive(Deserialize)]
struct JunitDetail {
    #[serde(rename = "@message", default)]
    message: String,
    #[serde(rename = "$text", default)]
    text: String,
}

#[derive(Deserialize)]
struct JunitSkipped {
    #[serde(rename = "@message", default)]
    message: String,
}

/// Convert a JUnit XML report (root `<testsuites>` or a single `<testsuite>`)
/// into a CTRF report value. Handles nextest-style nested suites.
fn junit_to_ctrf(xml: &str) -> Result<Value> {
    // Try the `<testsuites>` root first, then fall back to a single
    // `<testsuite>` root (both shapes are valid JUnit).
    let suites = match quick_xml::de::from_str::<JunitSuites>(xml) {
        Ok(s) if !s.testsuites.is_empty() => s.testsuites,
        _ => vec![quick_xml::de::from_str::<JunitSuite>(xml).context("parse JUnit XML")?],
    };

    let mut tests = Vec::new();
    for suite in &suites {
        collect_suite(suite, &mut tests);
    }

    let (mut passed, mut failed, mut skipped) = (0_u64, 0_u64, 0_u64);
    for t in &tests {
        match t.get("status").and_then(Value::as_str) {
            Some("failed") => failed += 1,
            Some("skipped") => skipped += 1,
            _ => passed += 1,
        }
    }
    // Every test is counted exactly once above, so the total is their sum —
    // avoids a `len() as u64` cast.
    let total = passed + failed + skipped;

    Ok(json!({
        "results": {
            "tool": { "name": "junit" },
            "summary": {
                "tests": total,
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
                "pending": 0,
                "other": 0,
                "start": 0,
                "stop": 0,
            },
            "tests": tests,
        }
    }))
}

/// Flatten a (possibly nested) JUnit suite into CTRF test objects.
fn collect_suite(suite: &JunitSuite, out: &mut Vec<Value>) {
    for case in &suite.testcases {
        let suite_name = if suite.name.is_empty() { case.classname.clone() } else { suite.name.clone() };
        // JUnit reports seconds; CTRF wants milliseconds. Keep it an f64 to
        // avoid a lossy float→int cast (clippy pedantic).
        let duration_ms = case.time.as_deref().and_then(|t| t.parse::<f64>().ok()).map_or(0.0, |secs| secs * 1000.0);

        let (status, message, trace) = if let Some(d) = case.failure.as_ref().or(case.error.as_ref()) {
            ("failed", Some(d.message.clone()), Some(d.text.clone()))
        } else if let Some(s) = case.skipped.as_ref() {
            ("skipped", Some(s.message.clone()), None)
        } else {
            ("passed", None, None)
        };

        let mut test = serde_json::Map::new();
        test.insert("name".into(), json!(case.name));
        test.insert("status".into(), json!(status));
        test.insert("duration".into(), json!(duration_ms));
        if !suite_name.is_empty() {
            test.insert("suite".into(), json!(suite_name));
        }
        if let Some(m) = message.filter(|m| !m.is_empty()) {
            test.insert("message".into(), json!(m));
        }
        if let Some(t) = trace.filter(|t| !t.is_empty()) {
            test.insert("trace".into(), json!(t));
        }
        out.push(Value::Object(test));
    }
    for nested in &suite.nested {
        collect_suite(nested, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEXTEST_JUNIT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="1">
  <testsuite name="mycrate" tests="3" failures="1">
    <testcase name="passes" classname="mycrate::a" time="0.012"/>
    <testcase name="fails" classname="mycrate::b" time="0.034"><failure message="assert failed">left != right</failure></testcase>
    <testcase name="ignored" classname="mycrate::c"><skipped message="not yet"/></testcase>
  </testsuite>
</testsuites>"#;

    #[test]
    fn junit_to_ctrf_maps_status_summary_and_details() {
        let ctrf = junit_to_ctrf(NEXTEST_JUNIT).expect("convert");
        let summary = &ctrf["results"]["summary"];
        assert_eq!(summary["tests"], 3);
        assert_eq!(summary["passed"], 1);
        assert_eq!(summary["failed"], 1);
        assert_eq!(summary["skipped"], 1);
        assert_eq!(ctrf["results"]["tool"]["name"], "junit");

        let tests = ctrf["results"]["tests"].as_array().expect("tests array");
        assert_eq!(tests.len(), 3);
        let fails = tests.iter().find(|t| t["name"] == "fails").expect("fails test");
        assert_eq!(fails["status"], "failed");
        assert_eq!(fails["suite"], "mycrate");
        assert_eq!(fails["message"], "assert failed");
        assert_eq!(fails["trace"], "left != right");
        // 0.034s → 34ms
        assert!((fails["duration"].as_f64().expect("duration") - 34.0).abs() < 0.001);

        let passes = tests.iter().find(|t| t["name"] == "passes").expect("passes test");
        assert_eq!(passes["status"], "passed");
        assert!(passes.get("message").is_none());
    }

    #[test]
    fn junit_to_ctrf_handles_single_suite_root() {
        let xml = r#"<testsuite name="solo" tests="1"><testcase name="ok" classname="solo::x" time="0.001"/></testsuite>"#;
        let ctrf = junit_to_ctrf(xml).expect("convert single suite");
        assert_eq!(ctrf["results"]["summary"]["tests"], 1);
        assert_eq!(ctrf["results"]["tests"][0]["status"], "passed");
    }

    #[test]
    fn parse_csv_trims_and_drops_empties() {
        assert_eq!(parse_csv(Some("a, b ,,c")), Some(vec!["a".into(), "b".into(), "c".into()]));
        assert_eq!(parse_csv(Some("  ,  ")), None);
        assert_eq!(parse_csv(None), None);
    }

    #[test]
    fn default_run_name_strips_dir_and_extension() {
        assert_eq!(default_run_name("path/to/ctrf-report.json"), "ctrf-report");
        assert_eq!(default_run_name("junit.xml"), "junit");
    }
}

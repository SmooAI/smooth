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
    /// Manage deployments — the release targets test runs are associated with.
    #[command(visible_alias = "deployment")]
    Deployments {
        #[command(subcommand)]
        cmd: DeploymentsCmd,
    },
    /// Manage test cases — the individual checks runs report results against.
    #[command(visible_alias = "case")]
    Cases {
        #[command(subcommand)]
        cmd: CasesCmd,
    },
    /// Manage test environments — the named contexts (e.g. staging) runs target.
    #[command(visible_alias = "environment")]
    Environments {
        #[command(subcommand)]
        cmd: EnvironmentsCmd,
    },
    /// Manage test runs — execute, report results, and submit CTRF/JUnit reports.
    #[command(visible_alias = "run")]
    Runs {
        #[command(subcommand)]
        cmd: RunsCmd,
    },
    /// Code coverage — upload parsed LCOV summaries and diff branches
    /// against a baseline (SMOODEV-2721).
    Coverage {
        #[command(subcommand)]
        cmd: CoverageCmd,
    },
}

#[derive(Subcommand)]
pub enum CoverageCmd {
    /// Parse an LCOV file and upload its summary (totals + per-file) for a
    /// scope. LCOV is the interchange every ecosystem's coverage tool emits
    /// (vitest/v8, cargo-llvm-cov, coverage.py, coverlet), so this one
    /// command covers all languages.
    Report {
        /// Path to an lcov.info file.
        file: String,
        /// Scope the coverage belongs to — the package path within the repo
        /// (e.g. `packages/db`, `python`, `rust/api-prime`).
        #[arg(long)]
        scope: String,
        /// Branch name (defaults to $GITHUB_HEAD_REF, then $GITHUB_REF_NAME).
        #[arg(long)]
        branch: Option<String>,
        /// Commit sha (defaults to $GITHUB_SHA).
        #[arg(long)]
        commit: Option<String>,
        /// Associate with an existing test run id.
        #[arg(long)]
        run_id: Option<String>,
        /// Drop per-file detail (totals only). Also applied automatically
        /// above 5000 files (the API's cap).
        #[arg(long)]
        no_files: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Compare a branch's latest per-scope coverage against a base branch
    /// (default `main`). Always exits 0 — the diff is a signal, not a gate.
    Diff {
        /// Head branch (defaults to $GITHUB_HEAD_REF, then $GITHUB_REF_NAME).
        #[arg(long)]
        branch: Option<String>,
        /// Base branch to diff against.
        #[arg(long, default_value = "main")]
        base: String,
        /// Output format: `table` (terminal) or `md` (GitHub-flavored table).
        #[arg(long, default_value = "table")]
        format: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum DeploymentsCmd {
    /// List deployments for the org.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show a single deployment by id.
    Show {
        /// The deployment id from `th testing deployments list`.
        deployment_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a deployment from an optional JSON body.
    Create {
        /// Optional JSON body (file path, or `-` for stdin) with the deployment fields.
        #[arg(long)]
        body: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update a deployment from an optional JSON body.
    Update {
        /// The deployment id from `th testing deployments list`.
        deployment_id: String,
        /// Optional JSON body (file path, or `-` for stdin) with the fields to patch.
        #[arg(long)]
        body: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a deployment by id.
    Delete {
        /// The deployment id from `th testing deployments list`.
        deployment_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum CasesCmd {
    /// List test cases for the org.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show a single test case by id.
    Show {
        /// The case id from `th testing cases list`.
        case_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a test case from an optional JSON body.
    Create {
        /// Optional JSON body (file path, or `-` for stdin) with the case fields.
        #[arg(long)]
        body: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update a test case from an optional JSON body.
    Update {
        /// The case id from `th testing cases list`.
        case_id: String,
        /// Optional JSON body (file path, or `-` for stdin) with the fields to patch.
        #[arg(long)]
        body: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a test case by id.
    Delete {
        /// The case id from `th testing cases list`.
        case_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum EnvironmentsCmd {
    /// List test environments for the org.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a test environment from an optional JSON body.
    Create {
        /// Optional JSON body (file path, or `-` for stdin) with the environment fields.
        #[arg(long)]
        body: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update a test environment from an optional JSON body.
    Update {
        /// The environment id from `th testing environments list`.
        env_id: String,
        /// Optional JSON body (file path, or `-` for stdin) with the fields to patch.
        #[arg(long)]
        body: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a test environment by id.
    Delete {
        /// The environment id from `th testing environments list`.
        env_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RunsCmd {
    /// List test runs for the org.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show a single test run by id.
    Show {
        /// The run id from `th testing runs list`.
        run_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a test run from an optional JSON body.
    Create {
        /// Optional JSON body (file path, or `-` for stdin) with the run fields.
        #[arg(long)]
        body: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update a test run from an optional JSON body.
    Update {
        /// The run id from `th testing runs list`.
        run_id: String,
        /// Optional JSON body (file path, or `-` for stdin) with the fields to patch.
        #[arg(long)]
        body: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a test run by id.
    Delete {
        /// The run id from `th testing runs list`.
        run_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Submit results for a run. Body is optional JSON.
    Results {
        /// The run id from `th testing runs list`.
        run_id: String,
        /// Optional JSON body (file path, or `-` for stdin) with the results payload.
        #[arg(long)]
        body: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
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
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
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
        Cmd::Coverage { cmd } => match cmd {
            CoverageCmd::Report {
                file,
                scope,
                branch,
                commit,
                run_id,
                no_files,
                org,
            } => {
                let o = require_active_org(&client, org)?;
                let branch = branch
                    .or_else(|| env_nonempty("GITHUB_HEAD_REF"))
                    .or_else(|| env_nonempty("GITHUB_REF_NAME"))
                    .context("--branch is required outside GitHub Actions (no GITHUB_HEAD_REF/GITHUB_REF_NAME)")?;
                let commit = commit
                    .or_else(|| env_nonempty("GITHUB_SHA"))
                    .context("--commit is required outside GitHub Actions (no GITHUB_SHA)")?;

                let raw = std::fs::read_to_string(&file).with_context(|| format!("read {file}"))?;
                let cov = parse_lcov(&raw).with_context(|| format!("parse lcov {file}"))?;

                let mut body = serde_json::Map::new();
                body.insert("scope".into(), json!(scope));
                body.insert("branch".into(), json!(branch));
                body.insert("commitSha".into(), json!(commit));
                body.insert("linesCovered".into(), json!(cov.lines_hit));
                body.insert("linesTotal".into(), json!(cov.lines_found));
                if cov.branches_found > 0 {
                    body.insert("branchesCovered".into(), json!(cov.branches_hit));
                    body.insert("branchesTotal".into(), json!(cov.branches_found));
                }
                if cov.functions_found > 0 {
                    body.insert("functionsCovered".into(), json!(cov.functions_hit));
                    body.insert("functionsTotal".into(), json!(cov.functions_found));
                }
                if let Some(v) = run_id {
                    body.insert("testRunId".into(), json!(v));
                }
                // The API caps per-file detail at 5000 entries — drop rather than 400.
                if !no_files && cov.files.len() <= 5000 {
                    let files: serde_json::Map<String, Value> = cov
                        .files
                        .iter()
                        .map(|f| {
                            (
                                f.path.clone(),
                                json!([f.lines_hit, f.lines_found, f.branches_hit, f.branches_found, f.functions_hit, f.functions_found]),
                            )
                        })
                        .collect();
                    body.insert("files".into(), Value::Object(files));
                } else if cov.files.len() > 5000 {
                    eprintln!("coverage report: {} files > 5000 — uploading totals only", cov.files.len());
                }

                print_json(
                    &client
                        .post(&format!("/organizations/{o}/testing/coverage"), Some(&Value::Object(body)))
                        .await
                        .context("POST coverage report")?,
                );
            }
            CoverageCmd::Diff { branch, base, format, org } => {
                let o = require_active_org(&client, org)?;
                let branch = branch
                    .or_else(|| env_nonempty("GITHUB_HEAD_REF"))
                    .or_else(|| env_nonempty("GITHUB_REF_NAME"))
                    .context("--branch is required outside GitHub Actions (no GITHUB_HEAD_REF/GITHUB_REF_NAME)")?;

                let head = fetch_latest(&client, &o, &branch).await?;
                let base_rows = fetch_latest(&client, &o, &base).await?;
                let table = render_coverage_diff(&head, &base_rows, &branch, &base, format == "md");
                println!("{table}");
            }
        },
    }
    Ok(())
}

/// GET the latest-per-scope coverage rows for a branch (the baseline query).
async fn fetch_latest(client: &SmoothApiClient, org: &str, branch: &str) -> Result<Vec<Value>> {
    let v = client
        .get(&format!(
            "/organizations/{org}/testing/coverage?latest=true&branch={}",
            urlencoding::encode(branch)
        ))
        .await
        .with_context(|| format!("GET latest coverage for {branch}"))?;
    Ok(v.as_array().cloned().unwrap_or_default())
}

// ── LCOV parsing (SMOODEV-2721) ──

/// Per-file coverage counters accumulated from one lcov `SF:`…`end_of_record`.
#[derive(Debug, Default, Clone)]
struct LcovFile {
    path: String,
    lines_hit: u64,
    lines_found: u64,
    branches_hit: u64,
    branches_found: u64,
    functions_hit: u64,
    functions_found: u64,
    // DA-derived fallback when LF/LH are absent (some emitters skip them).
    da_found: u64,
    da_hit: u64,
    saw_lf: bool,
}

#[derive(Debug, Default)]
struct LcovSummary {
    files: Vec<LcovFile>,
    lines_hit: u64,
    lines_found: u64,
    branches_hit: u64,
    branches_found: u64,
    functions_hit: u64,
    functions_found: u64,
}

/// Parse an LCOV file into per-file + total counters. Tracks `SF:` records;
/// prefers `LF:`/`LH:` and falls back to counting `DA:` lines when absent.
fn parse_lcov(text: &str) -> Result<LcovSummary> {
    let mut summary = LcovSummary::default();
    let mut current: Option<LcovFile> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix("SF:") {
            current = Some(LcovFile {
                path: path.trim().to_string(),
                ..Default::default()
            });
        } else if let Some(f) = current.as_mut() {
            if let Some(v) = line.strip_prefix("LF:") {
                f.lines_found = v.trim().parse().unwrap_or(0);
                f.saw_lf = true;
            } else if let Some(v) = line.strip_prefix("LH:") {
                f.lines_hit = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("BRF:") {
                f.branches_found = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("BRH:") {
                f.branches_hit = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("FNF:") {
                f.functions_found = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("FNH:") {
                f.functions_hit = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("DA:") {
                f.da_found += 1;
                if v.split(',').nth(1).and_then(|c| c.trim().parse::<u64>().ok()).is_some_and(|c| c > 0) {
                    f.da_hit += 1;
                }
            } else if line == "end_of_record" {
                let mut f = current.take().expect("current file present at end_of_record");
                if !f.saw_lf {
                    f.lines_found = f.da_found;
                    f.lines_hit = f.da_hit;
                }
                summary.lines_hit += f.lines_hit;
                summary.lines_found += f.lines_found;
                summary.branches_hit += f.branches_hit;
                summary.branches_found += f.branches_found;
                summary.functions_hit += f.functions_hit;
                summary.functions_found += f.functions_found;
                summary.files.push(f);
            }
        }
    }
    anyhow::ensure!(!summary.files.is_empty(), "no SF:/end_of_record records found — is this an LCOV file?");
    Ok(summary)
}

/// Render the branch-vs-base coverage table. `md` emits a GitHub-flavored
/// table; otherwise a plain terminal table. Scopes are the union of both
/// sides; a scope missing from base shows `new`, missing from head `gone`.
fn render_coverage_diff(head: &[Value], base: &[Value], branch: &str, base_name: &str, md: bool) -> String {
    fn pct(row: &Value, covered: &str, total: &str) -> Option<f64> {
        let c = row.get(covered)?.as_i64()?;
        let t = row.get(total)?.as_i64()?;
        (t > 0).then(|| c as f64 / t as f64 * 100.0)
    }
    fn fmt(p: Option<f64>) -> String {
        p.map_or("—".to_string(), |v| format!("{v:.1}%"))
    }
    let by_scope = |rows: &[Value]| -> std::collections::BTreeMap<String, Value> {
        rows.iter().filter_map(|r| Some((r.get("scope")?.as_str()?.to_string(), r.clone()))).collect()
    };
    let head_map = by_scope(head);
    let base_map = by_scope(base);
    let scopes: std::collections::BTreeSet<&String> = head_map.keys().chain(base_map.keys()).collect();

    let mut out = String::new();
    if md {
        out.push_str(&format!("**Coverage — `{branch}` vs `{base_name}`**\n\n"));
        out.push_str("| Scope | Lines | Δ vs base | Branches | Functions |\n|---|---:|---:|---:|---:|\n");
    } else {
        out.push_str(&format!("Coverage — {branch} vs {base_name}\n"));
        out.push_str(&format!("{:<40} {:>8} {:>10} {:>9} {:>10}\n", "Scope", "Lines", "Δ", "Branches", "Functions"));
    }
    for scope in scopes {
        let h = head_map.get(scope);
        let b = base_map.get(scope);
        let (lines, delta, branches, functions) = match (h, b) {
            (Some(h), Some(b)) => {
                let hl = pct(h, "linesCovered", "linesTotal");
                let bl = pct(b, "linesCovered", "linesTotal");
                let delta = match (hl, bl) {
                    (Some(hv), Some(bv)) => format!("{:+.1}", hv - bv),
                    _ => "—".to_string(),
                };
                (
                    fmt(hl),
                    delta,
                    fmt(pct(h, "branchesCovered", "branchesTotal")),
                    fmt(pct(h, "functionsCovered", "functionsTotal")),
                )
            }
            (Some(h), None) => (
                fmt(pct(h, "linesCovered", "linesTotal")),
                "new".to_string(),
                fmt(pct(h, "branchesCovered", "branchesTotal")),
                fmt(pct(h, "functionsCovered", "functionsTotal")),
            ),
            (None, Some(b)) => (fmt(pct(b, "linesCovered", "linesTotal")), "gone".to_string(), "—".to_string(), "—".to_string()),
            (None, None) => continue,
        };
        if md {
            out.push_str(&format!("| `{scope}` | {lines} | {delta} | {branches} | {functions} |\n"));
        } else {
            out.push_str(&format!("{scope:<40} {lines:>8} {delta:>10} {branches:>9} {functions:>10}\n"));
        }
    }
    out
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

    const LCOV_FULL: &str =
        "TN:\nSF:src/a.rs\nFNF:4\nFNH:3\nDA:1,1\nDA:2,0\nLF:10\nLH:7\nBRF:6\nBRH:2\nend_of_record\nSF:src/b.rs\nDA:1,5\nDA:2,0\nDA:3,1\nend_of_record\n";

    #[test]
    fn parse_lcov_sums_lf_lh_and_derives_from_da_when_absent() {
        let s = parse_lcov(LCOV_FULL).expect("parse");
        assert_eq!(s.files.len(), 2);
        // a.rs uses LF/LH (10/7) — the DA lines must NOT override them.
        let a = &s.files[0];
        assert_eq!((a.lines_hit, a.lines_found), (7, 10));
        assert_eq!((a.branches_hit, a.branches_found), (2, 6));
        assert_eq!((a.functions_hit, a.functions_found), (3, 4));
        // b.rs has no LF/LH — derived from DA: 3 lines, 2 hit.
        let b = &s.files[1];
        assert_eq!((b.lines_hit, b.lines_found), (2, 3));
        // Totals sum both files.
        assert_eq!((s.lines_hit, s.lines_found), (9, 13));
        assert_eq!((s.branches_hit, s.branches_found), (2, 6));
    }

    #[test]
    fn parse_lcov_rejects_non_lcov_input() {
        assert!(parse_lcov("{\"not\": \"lcov\"}").is_err());
        assert!(parse_lcov("").is_err());
    }

    fn cov_row(scope: &str, covered: i64, total: i64) -> Value {
        json!({ "scope": scope, "linesCovered": covered, "linesTotal": total })
    }

    #[test]
    fn render_coverage_diff_md_marks_deltas_new_and_gone() {
        let head = vec![cov_row("packages/db", 80, 100), cov_row("python", 5, 100)];
        let base = vec![cov_row("packages/db", 70, 100), cov_row("rust/api-prime", 90, 100)];
        let md = render_coverage_diff(&head, &base, "feat", "main", true);
        assert!(md.contains("| `packages/db` | 80.0% | +10.0 |"), "delta row: {md}");
        assert!(md.contains("| `python` | 5.0% | new |"), "new row: {md}");
        assert!(md.contains("| `rust/api-prime` | 90.0% | gone |"), "gone row: {md}");
        assert!(md.starts_with("**Coverage — `feat` vs `main`**"));
    }

    #[test]
    fn render_coverage_diff_handles_zero_totals_as_dash() {
        let head = vec![cov_row("empty", 0, 0)];
        let md = render_coverage_diff(&head, &[], "feat", "main", true);
        assert!(md.contains("| `empty` | — | new |"), "zero-total row: {md}");
    }
}

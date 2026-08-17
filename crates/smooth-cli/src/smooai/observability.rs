//! `th api observability …` — the observability read surface.
//!
//! Two halves:
//!
//! - **Sourcemaps** (SMOODEV-1164) — `sourcemaps-upload <dir>` walks a build
//!   directory, finds every `.js`/`.mjs` paired with a `.map`, registers each
//!   map via the backend API, and PUTs the bytes to the presigned S3 URL the
//!   API returns, so the Error Tracking dashboard can symbolicate stack frames
//!   by joining `error_sourcemaps` on `(release_id, file_path)`.
//! - **Queries** (pearl th-4d4040) — logs, traces, errors, LLM turns/cost, the
//!   pipeline-freshness heartbeat, open monitor incidents, and the platform
//!   audit trail. These are the routes the dashboard
//!   itself reads; before this the CLI covered 1 of 37 while its help claimed
//!   traces and telemetry.
//!
//! Every HTTP call lives in a `pub async fn` returning the raw
//! `serde_json::Value`, with a matching `render_*` summarizer — the `th mcp
//! serve` observability tools (pearl th-5abaa5) call the same pair, so the CLI
//! and an agent asking the same question cannot get different answers.
//!
//! **Two rules the renderers exist to enforce**, both borrowed from the hosted
//! `mcp.smoo.ai` tools:
//!
//! 1. *"No data" and "unavailable" never render the same.* A failed request is
//!    an `Err` all the way out; a successful empty result says so in words. A
//!    monitoring tool that lets "I couldn't see" read as "everything is fine"
//!    is worse than no tool.
//! 2. *Truncation is always reported.* When a page comes back exactly full,
//!    the summary says there may be more rather than implying it saw
//!    everything.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anstream::println;
use anyhow::{bail, Context, Result};
use chrono::{Duration, SecondsFormat, Utc};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};
use smooth_api_client::SmoothApiClient;
use walkdir::WalkDir;

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// Upload all .map files under `<dir>` to the Observability sourcemaps
    /// store, registered against (release, environment) so the Error
    /// Tracking dashboard can symbolicate stack frames.
    SourcemapsUpload {
        /// Directory to walk for .js/.mjs + .map pairs (e.g.
        /// `apps/web/.next/static`, `dist/`, `.open-next/server-function`).
        dir: PathBuf,
        /// Release identifier — must match `service.version` in your
        /// observability config (typically the git sha or semver tag).
        #[arg(long)]
        release: String,
        /// Deployment environment (`production`, `staging`, `development`).
        #[arg(long)]
        environment: String,
        /// Optional git sha; stored on the release row for cross-reference.
        #[arg(long)]
        git_sha: Option<String>,
        /// Strip this prefix from each file path before registering — so
        /// `.next/static/chunks/main.js.map` ends up as `static/chunks/
        /// main.js`. Optional; defaults to the directory you uploaded.
        #[arg(long)]
        strip_prefix: Option<PathBuf>,
        /// Don't upload — just print the file list + computed paths.
        #[arg(long)]
        dry_run: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Emit the raw JSON result instead of the progress lines.
        #[arg(long)]
        json: bool,
    },
    /// List sourcemaps registered for a (release, environment).
    SourcemapsList {
        /// Release identifier to list sourcemaps for (matches `service.version`).
        #[arg(long)]
        release: String,
        /// Deployment environment (`production`, `staging`, `development`).
        #[arg(long)]
        environment: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Emit the raw JSON response instead of the summary.
        #[arg(long)]
        json: bool,
    },
    /// Search structured logs, and discover the fields/facets to filter on.
    #[command(visible_alias = "log")]
    Logs {
        #[command(subcommand)]
        cmd: LogsCmd,
    },
    /// Search distributed traces and inspect one trace's spans.
    #[command(visible_alias = "trace")]
    Traces {
        #[command(subcommand)]
        cmd: TracesCmd,
    },
    /// Error-tracking groups — what is breaking, and the raw events under one group.
    #[command(visible_alias = "error")]
    Errors {
        #[command(subcommand)]
        cmd: ErrorsCmd,
    },
    /// LLM telemetry — agent turns, tool failures, and cost breakdowns.
    Llm {
        #[command(subcommand)]
        cmd: LlmCmd,
    },
    /// Pipeline freshness — when each telemetry pipe last wrote a row.
    ///
    /// This is the "can I trust the other commands right now" check: a stale
    /// pipe means an empty query result may be a broken ingest rather than a
    /// quiet system.
    Health {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Emit the raw JSON response instead of the summary.
        #[arg(long)]
        json: bool,
    },
    /// Website monitors — every uptime incident that is OPEN right now.
    ///
    /// Unlike the `observability/*` verbs this route accepts only a USER
    /// session (`th auth login`); an org M2M token gets a 401.
    #[command(visible_alias = "monitor")]
    Monitors {
        #[command(flatten)]
        common: Common,
    },
    /// Platform audit trail — who did what in the org, and whether it worked.
    ///
    /// This is the tamper-evident org record, not the local `th audit` tool
    /// log. USER session only, same as `monitors`.
    Audit {
        /// Free-text search across the audit event.
        #[arg(long, visible_alias = "q")]
        query: Option<String>,
        /// Filter by action name (repeatable).
        #[arg(long)]
        action: Vec<String>,
        /// Look-back window: `90s`, `45m`, `6h`, `7d`.
        #[arg(long, default_value = "24h")]
        since: String,
        /// Max events.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[command(flatten)]
        common: Common,
    },
}

/// Shared `--org-id` / `--json` pair. Flattened into every query verb so the
/// two flags are spelled identically everywhere.
#[derive(clap::Args, Debug, Default)]
pub struct Common {
    /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
    #[arg(long = "org-id", visible_alias = "org")]
    pub org: Option<String>,
    /// Emit the raw JSON response instead of the summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand)]
pub enum LogsCmd {
    /// Search logs over a time window.
    Query {
        /// Free-text search across the log message.
        #[arg(long, visible_alias = "q")]
        query: Option<String>,
        /// Filter by emitting service (repeatable).
        #[arg(long)]
        service: Vec<String>,
        /// Filter by level — ERROR, WARN, INFO … (repeatable).
        #[arg(long)]
        level: Vec<String>,
        /// Filter by log group (repeatable).
        #[arg(long)]
        log_group: Vec<String>,
        /// Only logs carrying this trace id.
        #[arg(long)]
        trace_id: Option<String>,
        /// Look-back window: `90s`, `45m`, `6h`, `7d`.
        #[arg(long, default_value = DEFAULT_SINCE)]
        since: String,
        /// Max rows (1–1000).
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Skip this many rows (for paging).
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[command(flatten)]
        common: Common,
    },
    /// Level / service / log-group counts over a window — what to filter on.
    Facets {
        /// Window in hours (1–168).
        #[arg(long, default_value_t = 24)]
        hours: u32,
        #[command(flatten)]
        common: Common,
    },
    /// Structured fields discovered in parsed logs, or one field's values.
    Fields {
        /// Show this field's most common VALUES instead of the field list.
        #[arg(long)]
        key: Option<String>,
        /// Look-back window: `90s`, `45m`, `6h`, `7d`.
        #[arg(long, default_value = DEFAULT_SINCE)]
        since: String,
        /// Max entries.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[command(flatten)]
        common: Common,
    },
    /// Every log line carrying one trace id, oldest first.
    Trace {
        /// The trace id to join on.
        trace_id: String,
        #[command(flatten)]
        common: Common,
    },
}

#[derive(Subcommand)]
pub enum TracesCmd {
    /// Search traces over a time window.
    Query {
        /// Free-text search across the root span name.
        #[arg(long, visible_alias = "q")]
        query: Option<String>,
        /// Filter by service (repeatable).
        #[arg(long)]
        service: Vec<String>,
        /// ok | error | unset | any (default any).
        #[arg(long)]
        status: Option<String>,
        /// Only traces at least this slow.
        #[arg(long)]
        min_duration_ms: Option<f64>,
        /// Look-back window: `90s`, `45m`, `6h`, `7d`.
        #[arg(long, default_value = DEFAULT_SINCE)]
        since: String,
        /// Max rows (1–500).
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// `recent` (default) or `slowest`.
        #[arg(long)]
        order_by: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// One trace's spans.
    Show {
        /// The trace id.
        trace_id: String,
        #[command(flatten)]
        common: Common,
    },
}

#[derive(Subcommand)]
pub enum ErrorsCmd {
    /// Error groups, most recently seen first.
    Top {
        /// unresolved | resolved | muted. Omit for all statuses.
        #[arg(long)]
        status: Option<String>,
        /// Filter by deployment environment.
        #[arg(long)]
        environment: Option<String>,
        /// Max groups (1–100).
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[command(flatten)]
        common: Common,
    },
    /// One error group plus its most recent events.
    Show {
        /// The error group id (a UUID, from `errors top`).
        group_id: String,
        /// How many raw events to fetch alongside the group.
        #[arg(long, default_value_t = 10)]
        events: u32,
        #[command(flatten)]
        common: Common,
    },
}

#[derive(Subcommand)]
pub enum LlmCmd {
    /// Recent agent turns (`gen_ai_events`), newest first.
    Turns {
        /// Only turns in this conversation.
        #[arg(long)]
        conversation_id: Option<String>,
        /// Only turns on this model.
        #[arg(long)]
        model: Option<String>,
        /// Only turns from this service.
        #[arg(long)]
        service: Option<String>,
        /// One exact turn — a `chat` span and its `tool` children share a trace id.
        #[arg(long)]
        trace_id: Option<String>,
        /// error | ok | unset | any (default any).
        #[arg(long)]
        status: Option<String>,
        /// Look-back in minutes (1–10080).
        #[arg(long, default_value_t = 60)]
        window_minutes: u32,
        /// Max turns (1–200).
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[command(flatten)]
        common: Common,
    },
    /// Failing tool calls grouped by tool, worst first.
    ToolFailures {
        /// Look-back in minutes (1–10080).
        #[arg(long, default_value_t = 60)]
        window_minutes: u32,
        /// Max groups (1–50).
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[command(flatten)]
        common: Common,
    },
    /// LLM spend broken down by model, agent, or conversation.
    Cost {
        /// model (default) | agent | conversation.
        #[arg(long, default_value = "model")]
        group_by: String,
        /// Look-back in minutes (1–10080).
        #[arg(long, default_value_t = 60)]
        window_minutes: u32,
        /// Max rows (1–50).
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[command(flatten)]
        common: Common,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::SourcemapsUpload {
            dir,
            release,
            environment,
            git_sha,
            strip_prefix,
            dry_run,
            org,
            json,
        } => {
            let org = require_active_org(&client, org)?;
            let dir = dir.canonicalize().with_context(|| format!("canonicalize {}", dir.display()))?;
            let strip = strip_prefix.unwrap_or_else(|| dir.clone());

            let maps = find_sourcemaps(&dir)?;
            if maps.is_empty() {
                if json {
                    print_json(&json!({ "uploaded": 0, "failed": [], "dir": dir.display().to_string(), "reason": "no .map files found" }));
                } else {
                    println!();
                    println!("  {} no .map files found under {}", "●".dimmed(), dir.display().to_string().dimmed());
                    println!();
                }
                return Ok(());
            }

            if !json {
                println!();
                println!("  {} {} sourcemap{}", "●".cyan(), maps.len().bold(), if maps.len() == 1 { "" } else { "s" });
                println!("    {} {}", "release:".dimmed(), release);
                println!("    {} {}", "environment:".dimmed(), environment);
                println!("    {} {}", "dir:".dimmed(), dir.display());
                println!();
            }

            let http = reqwest::Client::new();
            let mut uploaded = 0usize;
            let mut failed: Vec<(PathBuf, String)> = Vec::new();

            for map_path in &maps {
                // Derive the file path the backend stores against. We strip
                // the `.map` suffix so it matches the JS path the runtime
                // sees, then trim `strip_prefix` so the stored path is
                // build-relative (not absolute).
                let js_path = map_path.with_extension("");
                let stored = js_path.strip_prefix(&strip).unwrap_or(js_path.as_path()).to_string_lossy().into_owned();

                if dry_run {
                    if !json {
                        println!("  [dry-run] {}", stored.dimmed());
                    }
                    continue;
                }

                let body = serde_json::json!({
                    "releaseId": release,
                    "environment": environment,
                    "gitSha": git_sha,
                    "filePath": stored,
                });
                let resp = match client.post(&format!("/organizations/{org}/observability/sourcemaps/upload"), Some(&body)).await {
                    Ok(v) => v,
                    Err(e) => {
                        failed.push((map_path.clone(), format!("API: {e}")));
                        continue;
                    }
                };
                let Some(upload_url) = resp.get("uploadUrl").and_then(|v| v.as_str()) else {
                    failed.push((map_path.clone(), "API returned no uploadUrl".to_owned()));
                    continue;
                };

                let bytes = match std::fs::read(map_path) {
                    Ok(b) => b,
                    Err(e) => {
                        failed.push((map_path.clone(), format!("read map: {e}")));
                        continue;
                    }
                };

                let put = http.put(upload_url).header("content-type", "application/json").body(bytes).send().await;
                match put {
                    Ok(r) if r.status().is_success() => {
                        uploaded += 1;
                        if !json {
                            println!("  {} {}", "✓".green(), stored);
                        }
                    }
                    Ok(r) => {
                        let status = r.status();
                        let body = r.text().await.unwrap_or_default();
                        failed.push((map_path.clone(), format!("S3 PUT {status}: {body}")));
                    }
                    Err(e) => failed.push((map_path.clone(), format!("S3 PUT: {e}"))),
                };
            }

            if json {
                print_json(&json!({
                    "uploaded": uploaded,
                    "dryRun": dry_run,
                    "release": release,
                    "environment": environment,
                    "failed": failed.iter().map(|(p, e)| json!({ "path": p.display().to_string(), "error": e })).collect::<Vec<_>>(),
                }));
            } else {
                println!();
                println!(
                    "  {} {} uploaded · {} failed",
                    if failed.is_empty() {
                        "✓".green().to_string()
                    } else {
                        "!".yellow().to_string()
                    },
                    uploaded.bold(),
                    failed.len().bold(),
                );
                for (path, err) in &failed {
                    println!("    {} {} — {}", "✗".red(), path.display().to_string().dimmed(), err);
                }
                println!();
            }

            if !failed.is_empty() {
                anyhow::bail!("{} sourcemap upload(s) failed", failed.len());
            }
        }

        Cmd::SourcemapsList {
            release,
            environment,
            org,
            json,
        } => {
            let org = require_active_org(&client, org)?;
            let path = format!(
                "/organizations/{org}/observability/sourcemaps/list?releaseId={}&environment={}",
                urlencoding::encode(&release),
                urlencoding::encode(&environment),
            );
            let resp = client.get(&path).await.context("GET sourcemaps/list")?;
            if json {
                print_json(&resp);
            } else {
                println!();
                println!("{}", render_sourcemaps(&resp, &release, &environment));
                println!();
            }
        }
        Cmd::Logs { cmd } => logs(cmd, &client).await?,
        Cmd::Traces { cmd } => traces(cmd, &client).await?,
        Cmd::Errors { cmd } => errors(cmd, &client).await?,
        Cmd::Llm { cmd } => llm(cmd, &client).await?,
        Cmd::Health { org, json } => {
            let org = require_active_org(&client, org)?;
            let resp = pipeline_health(&client, &org).await?;
            emit(&resp, json, render_health);
        }
        Cmd::Monitors { common } => {
            let org = require_active_org(&client, common.org)?;
            let resp = open_incidents(&client, &org).await?;
            emit(&resp, common.json, render_incidents);
        }
        Cmd::Audit {
            query,
            action,
            since,
            limit,
            common,
        } => {
            let org = require_active_org(&client, common.org)?;
            let resp = audit_query(&client, &org, query.as_deref(), &action, &since, limit).await?;
            emit(&resp, common.json, |r| render_audit(r, limit as usize));
        }
    }
    Ok(())
}

/// Print a query result: raw JSON on `--json`, otherwise the summary.
fn emit(resp: &Value, as_json: bool, render: impl Fn(&Value) -> String) {
    if as_json {
        print_json(resp);
    } else {
        println!();
        println!("{}", render(resp));
        println!();
    }
}

async fn logs(cmd: LogsCmd, client: &SmoothApiClient) -> Result<()> {
    match cmd {
        LogsCmd::Query {
            query,
            service,
            level,
            log_group,
            trace_id,
            since,
            limit,
            offset,
            common,
        } => {
            let org = require_active_org(client, common.org)?;
            let filters = LogFilters {
                search: query,
                service,
                level,
                log_group,
                trace_id,
                since,
                limit,
                offset,
            };
            let resp = logs_query(client, &org, &filters).await?;
            emit(&resp, common.json, |r| render_logs(r, limit as usize));
        }
        LogsCmd::Facets { hours, common } => {
            let org = require_active_org(client, common.org)?;
            let resp = logs_facets(client, &org, hours).await?;
            emit(&resp, common.json, render_facets);
        }
        LogsCmd::Fields { key, since, limit, common } => {
            let org = require_active_org(client, common.org)?;
            let resp = logs_fields(client, &org, key.as_deref(), &since, limit).await?;
            emit(&resp, common.json, |r| render_fields(r, limit as usize));
        }
        LogsCmd::Trace { trace_id, common } => {
            let org = require_active_org(client, common.org)?;
            let resp = logs_for_trace(client, &org, &trace_id).await?;
            // No caller-supplied limit — the route returns the whole trace.
            emit(&resp, common.json, |r| render_logs(r, usize::MAX));
        }
    }
    Ok(())
}

async fn traces(cmd: TracesCmd, client: &SmoothApiClient) -> Result<()> {
    match cmd {
        TracesCmd::Query {
            query,
            service,
            status,
            min_duration_ms,
            since,
            limit,
            order_by,
            common,
        } => {
            let org = require_active_org(client, common.org)?;
            let filters = TraceFilters {
                search: query,
                service,
                status,
                min_duration_ms,
                since,
                limit,
                order_by,
            };
            let resp = traces_query(client, &org, &filters).await?;
            emit(&resp, common.json, |r| render_traces(r, limit as usize));
        }
        TracesCmd::Show { trace_id, common } => {
            let org = require_active_org(client, common.org)?;
            let resp = trace_spans(client, &org, &trace_id).await?;
            emit(&resp, common.json, |r| render_spans(r, &trace_id));
        }
    }
    Ok(())
}

async fn errors(cmd: ErrorsCmd, client: &SmoothApiClient) -> Result<()> {
    match cmd {
        ErrorsCmd::Top {
            status,
            environment,
            limit,
            common,
        } => {
            let org = require_active_org(client, common.org)?;
            let resp = error_groups(client, &org, status.as_deref(), environment.as_deref(), limit).await?;
            emit(&resp, common.json, |r| render_error_groups(r, limit as usize));
        }
        ErrorsCmd::Show { group_id, events, common } => {
            let org = require_active_org(client, common.org)?;
            let resp = error_group(client, &org, &group_id, events).await?;
            emit(&resp, common.json, |r| render_error_group(r, events as usize));
        }
    }
    Ok(())
}

async fn llm(cmd: LlmCmd, client: &SmoothApiClient) -> Result<()> {
    match cmd {
        LlmCmd::Turns {
            conversation_id,
            model,
            service,
            trace_id,
            status,
            window_minutes,
            limit,
            common,
        } => {
            let org = require_active_org(client, common.org)?;
            let filters = TurnFilters {
                conversation_id,
                model,
                service,
                trace_id,
                status,
                window_minutes,
                limit,
            };
            let resp = llm_turns(client, &org, &filters).await?;
            emit(&resp, common.json, |r| render_llm_turns(r, limit as usize));
        }
        LlmCmd::ToolFailures { window_minutes, limit, common } => {
            let org = require_active_org(client, common.org)?;
            let resp = llm_tool_failures(client, &org, window_minutes, limit).await?;
            emit(&resp, common.json, |r| render_tool_failures(r, limit as usize));
        }
        LlmCmd::Cost {
            group_by,
            window_minutes,
            limit,
            common,
        } => {
            let org = require_active_org(client, common.org)?;
            let resp = llm_cost(client, &org, &group_by, window_minutes, limit).await?;
            emit(&resp, common.json, |r| render_llm_cost(r, limit as usize));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows + query-string plumbing
// ---------------------------------------------------------------------------

/// Default look-back for the log/trace verbs. An hour is the "what just broke"
/// window; anything longer is a deliberate `--since`.
pub const DEFAULT_SINCE: &str = "1h";

/// Parse a relative window — `90s`, `45m`, `6h`, `7d`.
///
/// A bare number is rejected rather than guessed at: `--since 24` meaning
/// hours to one reader and minutes to another is exactly the ambiguity that
/// makes a monitoring answer wrong without looking wrong.
///
/// # Errors
/// Returns an error for an empty string, a missing/unknown unit, a
/// non-numeric magnitude, or a non-positive duration.
pub fn parse_since(since: &str) -> Result<Duration> {
    let raw = since.trim();
    let (digits, unit) = raw.split_at(raw.len().saturating_sub(1));
    let n: i64 = digits
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid --since \"{since}\" — use a number plus s|m|h|d, e.g. 90s, 45m, 6h, 7d"))?;
    if n <= 0 {
        bail!("invalid --since \"{since}\" — the window must be positive");
    }
    match unit {
        "s" => Ok(Duration::seconds(n)),
        "m" => Ok(Duration::minutes(n)),
        "h" => Ok(Duration::hours(n)),
        "d" => Ok(Duration::days(n)),
        other => bail!("unknown --since unit \"{other}\" in \"{since}\" — use s, m, h or d"),
    }
}

/// `since` as an RFC3339 `(start, end)` pair ending now, the shape the
/// `timeRange` bodies and the `start`/`end` query params both want.
///
/// # Errors
/// Propagates [`parse_since`].
pub fn window(since: &str) -> Result<(String, String)> {
    let end = Utc::now();
    let start = end - parse_since(since)?;
    Ok((
        start.to_rfc3339_opts(SecondsFormat::Millis, true),
        end.to_rfc3339_opts(SecondsFormat::Millis, true),
    ))
}

/// Build a `?a=1&b=2` suffix from the pairs whose value is `Some`, each value
/// percent-encoded. Returns `""` when nothing is set, so it is always safe to
/// append.
fn qs(pairs: &[(&str, Option<String>)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={}", urlencoding::encode(v))))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// `Some(v)` unless the string is blank — a blank filter must not become a
/// filter on the empty string.
fn opt(v: Option<&str>) -> Option<String> {
    v.map(str::trim).filter(|s| !s.is_empty()).map(ToString::to_string)
}

/// Drop `None`/empty entries from a request body so an unset filter is absent
/// rather than `null` (the handlers treat the two differently in places).
fn body_from(pairs: Vec<(&str, Value)>) -> Value {
    let obj: serde_json::Map<String, Value> = pairs
        .into_iter()
        .filter(|(_, v)| !v.is_null() && !matches!(v, Value::Array(a) if a.is_empty()))
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    Value::Object(obj)
}

/// A list of strings as a JSON array, or `Value::Null` when empty.
fn list(items: &[String]) -> Value {
    if items.is_empty() {
        Value::Null
    } else {
        json!(items)
    }
}

// ---------------------------------------------------------------------------
// Queries — one function per route, shared by the CLI and `th mcp serve`
// ---------------------------------------------------------------------------

/// Filters for [`logs_query`].
#[derive(Debug, Default)]
pub struct LogFilters {
    pub search: Option<String>,
    pub service: Vec<String>,
    pub level: Vec<String>,
    pub log_group: Vec<String>,
    pub trace_id: Option<String>,
    /// Relative window (see [`parse_since`]); empty falls back to [`DEFAULT_SINCE`].
    pub since: String,
    pub limit: u32,
    pub offset: u32,
}

impl LogFilters {
    fn since_or_default(&self) -> &str {
        if self.since.trim().is_empty() {
            DEFAULT_SINCE
        } else {
            &self.since
        }
    }
}

/// `POST /observability/logs/query` → `{ data: [...], total }`.
///
/// # Errors
/// Bad `since`, or a non-2xx from the API.
pub async fn logs_query(client: &SmoothApiClient, org: &str, f: &LogFilters) -> Result<Value> {
    let (start, end) = window(f.since_or_default())?;
    let body = body_from(vec![
        ("search", opt(f.search.as_deref()).map_or(Value::Null, Value::String)),
        ("serviceName", list(&f.service)),
        ("level", list(&f.level)),
        ("logGroup", list(&f.log_group)),
        ("traceId", opt(f.trace_id.as_deref()).map_or(Value::Null, Value::String)),
        ("timeRange", json!({ "start": start, "end": end })),
        ("limit", json!(f.limit)),
        ("offset", json!(f.offset)),
    ]);
    client
        .post(&format!("/organizations/{org}/observability/logs/query"), Some(&body))
        .await
        .context("POST observability/logs/query")
}

/// `GET /observability/logs/facets?hours=` → level / service / log-group counts.
///
/// # Errors
/// Non-2xx from the API.
pub async fn logs_facets(client: &SmoothApiClient, org: &str, hours: u32) -> Result<Value> {
    client
        .get(&format!("/organizations/{org}/observability/logs/facets?hours={hours}"))
        .await
        .context("GET observability/logs/facets")
}

/// `GET /observability/logs/fields` — discovered field keys, or one key's values.
///
/// # Errors
/// Bad `since`, or a non-2xx from the API.
pub async fn logs_fields(client: &SmoothApiClient, org: &str, key: Option<&str>, since: &str, limit: u32) -> Result<Value> {
    let (start, end) = window(since)?;
    let query = qs(&[
        ("start", Some(start)),
        ("end", Some(end)),
        ("limit", Some(limit.to_string())),
        ("key", opt(key)),
    ]);
    client
        .get(&format!("/organizations/{org}/observability/logs/fields{query}"))
        .await
        .context("GET observability/logs/fields")
}

/// `GET /observability/logs/trace/{trace_id}` → every log line on one trace.
///
/// # Errors
/// Non-2xx from the API.
pub async fn logs_for_trace(client: &SmoothApiClient, org: &str, trace_id: &str) -> Result<Value> {
    client
        .get(&format!("/organizations/{org}/observability/logs/trace/{}", urlencoding::encode(trace_id)))
        .await
        .context("GET observability/logs/trace")
}

/// Filters for [`traces_query`].
#[derive(Debug, Default)]
pub struct TraceFilters {
    pub search: Option<String>,
    pub service: Vec<String>,
    /// `ok` | `error` | `unset` | `any`.
    pub status: Option<String>,
    pub min_duration_ms: Option<f64>,
    /// Relative window (see [`parse_since`]); empty falls back to [`DEFAULT_SINCE`].
    pub since: String,
    pub limit: u32,
    pub order_by: Option<String>,
}

/// `POST /observability/traces/query` → `{ traces: [...], total }`.
///
/// # Errors
/// Bad `since`, or a non-2xx from the API.
pub async fn traces_query(client: &SmoothApiClient, org: &str, f: &TraceFilters) -> Result<Value> {
    let since = if f.since.trim().is_empty() { DEFAULT_SINCE } else { &f.since };
    let (start, end) = window(since)?;
    let body = body_from(vec![
        ("search", opt(f.search.as_deref()).map_or(Value::Null, Value::String)),
        ("service", list(&f.service)),
        ("status", opt(f.status.as_deref()).map_or(Value::Null, Value::String)),
        ("durationMinMs", f.min_duration_ms.map_or(Value::Null, |d| json!(d))),
        ("timeRange", json!({ "start": start, "end": end })),
        ("limit", json!(f.limit)),
        ("orderBy", opt(f.order_by.as_deref()).map_or(Value::Null, Value::String)),
    ]);
    client
        .post(&format!("/organizations/{org}/observability/traces/query"), Some(&body))
        .await
        .context("POST observability/traces/query")
}

/// `GET /observability/traces/{trace_id}` → `{ spans: [...] }`.
///
/// # Errors
/// Non-2xx from the API.
pub async fn trace_spans(client: &SmoothApiClient, org: &str, trace_id: &str) -> Result<Value> {
    client
        .get(&format!("/organizations/{org}/observability/traces/{}", urlencoding::encode(trace_id)))
        .await
        .context("GET observability/traces/{trace_id}")
}

/// `GET /observability/errors` → `{ groups: [...], nextCursor }`.
///
/// # Errors
/// Non-2xx from the API (including a 400 for an unknown `status`).
pub async fn error_groups(client: &SmoothApiClient, org: &str, status: Option<&str>, environment: Option<&str>, limit: u32) -> Result<Value> {
    let query = qs(&[("limit", Some(limit.to_string())), ("status", opt(status)), ("environment", opt(environment))]);
    client
        .get(&format!("/organizations/{org}/observability/errors{query}"))
        .await
        .context("GET observability/errors")
}

/// One error group plus its recent events, merged into
/// `{ group, recentEvents, events }`.
///
/// Two calls because the group route returns only a short `recentEvents`
/// preview; `events` is the caller-sized page from the dedicated events route.
///
/// # Errors
/// Non-2xx from either request.
pub async fn error_group(client: &SmoothApiClient, org: &str, group_id: &str, events: u32) -> Result<Value> {
    let id = urlencoding::encode(group_id);
    let mut group = client
        .get(&format!("/organizations/{org}/observability/errors/{id}"))
        .await
        .context("GET observability/errors/{group_id}")?;
    let ev = client
        .get(&format!("/organizations/{org}/observability/errors/{id}/events?limit={events}"))
        .await
        .context("GET observability/errors/{group_id}/events")?;
    if let Some(obj) = group.as_object_mut() {
        obj.insert("events".to_string(), ev.get("events").cloned().unwrap_or(Value::Null));
        obj.insert("eventsNextCursor".to_string(), ev.get("nextCursor").cloned().unwrap_or(Value::Null));
    }
    Ok(group)
}

/// Filters for [`llm_turns`].
#[derive(Debug, Default)]
pub struct TurnFilters {
    pub conversation_id: Option<String>,
    pub model: Option<String>,
    pub service: Option<String>,
    pub trace_id: Option<String>,
    /// `error` | `ok` | `unset` | `any`.
    pub status: Option<String>,
    pub window_minutes: u32,
    pub limit: u32,
}

/// `GET /observability/llm/turns` → `{ turns, total, windowMinutes }`.
///
/// # Errors
/// Non-2xx from the API.
pub async fn llm_turns(client: &SmoothApiClient, org: &str, f: &TurnFilters) -> Result<Value> {
    let query = qs(&[
        ("windowMinutes", Some(f.window_minutes.to_string())),
        ("limit", Some(f.limit.to_string())),
        ("conversationId", opt(f.conversation_id.as_deref())),
        ("model", opt(f.model.as_deref())),
        ("service", opt(f.service.as_deref())),
        ("traceId", opt(f.trace_id.as_deref())),
        ("status", opt(f.status.as_deref())),
    ]);
    client
        .get(&format!("/organizations/{org}/observability/llm/turns{query}"))
        .await
        .context("GET observability/llm/turns")
}

/// `GET /observability/llm/tool-failures` → `{ toolFailures, windowMinutes }`.
///
/// # Errors
/// Non-2xx from the API.
pub async fn llm_tool_failures(client: &SmoothApiClient, org: &str, window_minutes: u32, limit: u32) -> Result<Value> {
    client
        .get(&format!(
            "/organizations/{org}/observability/llm/tool-failures?windowMinutes={window_minutes}&limit={limit}"
        ))
        .await
        .context("GET observability/llm/tool-failures")
}

/// `GET /observability/llm/cost` → `{ groupBy, rows, costAvailable, windowMinutes }`.
///
/// # Errors
/// Non-2xx from the API — including a 400 for an unknown `group_by`, which the
/// server refuses rather than silently answering along a different axis.
pub async fn llm_cost(client: &SmoothApiClient, org: &str, group_by: &str, window_minutes: u32, limit: u32) -> Result<Value> {
    let query = qs(&[
        ("groupBy", opt(Some(group_by))),
        ("windowMinutes", Some(window_minutes.to_string())),
        ("limit", Some(limit.to_string())),
    ]);
    client
        .get(&format!("/organizations/{org}/observability/llm/cost{query}"))
        .await
        .context("GET observability/llm/cost")
}

/// `GET /observability/health` — when each telemetry pipe last wrote a row.
///
/// # Errors
/// Non-2xx from the API.
pub async fn pipeline_health(client: &SmoothApiClient, org: &str) -> Result<Value> {
    client
        .get(&format!("/organizations/{org}/observability/health"))
        .await
        .context("GET observability/health")
}

/// `GET /website-monitors/incidents` → `{ incidents: [...] }` — every OPEN
/// uptime incident org-wide.
///
/// Unlike its `observability/*` siblings this route is `auth: 'user'` only, so
/// an org M2M token gets a 401 here even though it can read logs and traces.
///
/// # Errors
/// Non-2xx from the API.
pub async fn open_incidents(client: &SmoothApiClient, org: &str) -> Result<Value> {
    client
        .get(&format!("/organizations/{org}/website-monitors/incidents"))
        .await
        .context("GET website-monitors/incidents")
}

/// `POST /audit/query` → `{ data: [...], total }` — the tamper-evident audit
/// trail (who did what), not the local `~/.smooth/audit` tool log.
///
/// Also `auth: 'user'` only; see [`open_incidents`].
///
/// # Errors
/// Bad `since`, or a non-2xx from the API.
pub async fn audit_query(client: &SmoothApiClient, org: &str, search: Option<&str>, actions: &[String], since: &str, limit: u32) -> Result<Value> {
    let (start, end) = window(if since.trim().is_empty() { "24h" } else { since })?;
    let body = body_from(vec![
        ("search", opt(search).map_or(Value::Null, Value::String)),
        ("actions", list(actions)),
        ("timeRange", json!({ "start": start, "end": end })),
        ("limit", json!(limit)),
    ]);
    client
        .post(&format!("/organizations/{org}/audit/query"), Some(&body))
        .await
        .context("POST audit/query")
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

/// The array under `key`, or an empty slice. A missing key and an empty array
/// render identically on purpose — both mean "the query succeeded and matched
/// nothing", which the callers then say in words.
fn rows<'a>(body: &'a Value, key: &str) -> &'a [Value] {
    body.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// First present, non-empty value among `keys`, rendered compactly. Tolerates
/// snake_case/camelCase drift between the ClickHouse projections and the
/// Postgres serializers without a per-route struct.
fn pick(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| match v.get(*k) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    })
}

fn field(v: &Value, keys: &[&str]) -> String {
    pick(v, keys).unwrap_or_else(|| "-".to_string())
}

/// Rule 2: a page that came back exactly full may not be the whole answer, and
/// must say so. Silence here reads as "that's everything".
fn truncation_note(returned: usize, limit: usize, total: Option<u64>) -> String {
    match total {
        Some(t) if t as usize > returned => format!("\n(showing {returned} of {t} — raise --limit or page with --offset for the rest)"),
        _ if limit != usize::MAX && returned >= limit && returned > 0 => {
            format!("\n(showing {returned}, the requested limit — there may be more; raise --limit)")
        }
        _ => String::new(),
    }
}

fn total_of(body: &Value) -> Option<u64> {
    body.get("total").and_then(Value::as_u64)
}

/// Log rows (`logs query` and `logs trace`).
pub fn render_logs(body: &Value, limit: usize) -> String {
    let data = rows(body, "data");
    if data.is_empty() {
        return "No log lines matched. (The query ran and returned zero rows — check `th api observability health` if you expected some.)".to_string();
    }
    let mut out = format!("{} log line(s):\n", data.len());
    for r in data {
        let _ = writeln!(
            out,
            "{}  {:<5}  {}  {}",
            field(r, &["timestamp"]),
            field(r, &["level"]),
            field(r, &["service_name", "serviceName", "log_group", "logGroup"]),
            field(r, &["message"]).replace('\n', " ")
        );
    }
    out.push_str(truncation_note(data.len(), limit, total_of(body)).trim_end_matches('\n'));
    out.trim_end().to_string()
}

/// Log facets (`{ levels, logGroups, ... }` — shape varies, so print counts).
pub fn render_facets(body: &Value) -> String {
    let Some(obj) = body.as_object() else {
        return "Unexpected facets payload (no object).".to_string();
    };
    let mut out = String::new();
    for (name, v) in obj {
        let Some(items) = v.as_array() else { continue };
        if items.is_empty() {
            let _ = writeln!(out, "{name}: none in this window");
            continue;
        }
        let rendered: Vec<String> = items
            .iter()
            .take(15)
            .map(|i| {
                let label = field(i, &["level", "name", "value", "key", "log_group", "logGroup", "service_name", "serviceName"]);
                let count = field(i, &["count", "total"]);
                format!("{label} ({count})")
            })
            .collect();
        let _ = writeln!(out, "{name}: {}", rendered.join(", "));
    }
    if out.is_empty() {
        return "No facets in this window. (The query ran and returned nothing to filter on.)".to_string();
    }
    out.trim_end().to_string()
}

/// Discovered structured fields, or one field's values.
pub fn render_fields(body: &Value, limit: usize) -> String {
    if let Some(key) = body.get("key").and_then(Value::as_str) {
        let values = rows(body, "values");
        if values.is_empty() {
            return format!("No values recorded for field `{key}` in this window. (The query ran and returned zero rows.)");
        }
        let mut out = format!("{} value(s) for `{key}`:\n", values.len());
        for v in values {
            let _ = writeln!(out, "  {}  ({})", field(v, &["value"]), field(v, &["count"]));
        }
        out.push_str(truncation_note(values.len(), limit, None).trim_end_matches('\n'));
        return out.trim_end().to_string();
    }
    let fields = rows(body, "fields");
    if fields.is_empty() {
        return "No structured fields discovered in this window. (The query ran and returned zero rows — only JSON-parsed logs contribute fields.)".to_string();
    }
    let mut out = format!("{} field(s):\n", fields.len());
    for f in fields {
        let _ = writeln!(out, "  {}  ({})", field(f, &["key", "name"]), field(f, &["count"]));
    }
    out.push_str(truncation_note(fields.len(), limit, None).trim_end_matches('\n'));
    out.trim_end().to_string()
}

/// Trace search results.
pub fn render_traces(body: &Value, limit: usize) -> String {
    let data = rows(body, "traces");
    if data.is_empty() {
        return "No traces matched. (The query ran and returned zero rows — check `th api observability health` if you expected some.)".to_string();
    }
    let mut out = format!("{} trace(s):\n", data.len());
    for t in data {
        let errors = pick(t, &["errorSpanCount"]).unwrap_or_else(|| "0".to_string());
        let _ = writeln!(
            out,
            "{}  {:>8}ms  {}  {}  spans={} errors={}  {}",
            field(t, &["startedAt"]),
            field(t, &["durationMs"]),
            field(t, &["serviceName"]),
            field(t, &["statusCode"]),
            field(t, &["spanCount"]),
            errors,
            field(t, &["rootSpanName", "traceId"]),
        );
        if let Some(id) = pick(t, &["traceId"]) {
            let _ = writeln!(out, "    trace {id}");
        }
    }
    out.push_str(truncation_note(data.len(), limit, total_of(body)).trim_end_matches('\n'));
    out.trim_end().to_string()
}

/// One trace's spans.
pub fn render_spans(body: &Value, trace_id: &str) -> String {
    let spans = rows(body, "spans");
    if spans.is_empty() {
        return format!("No spans stored for trace `{trace_id}`. (The lookup succeeded — the trace id may be wrong, or the trace has aged out.)");
    }
    let mut out = format!("{} span(s) in trace {trace_id}:\n", spans.len());
    for s in spans {
        let err = if s.get("hasError").and_then(Value::as_bool) == Some(true) {
            " ERROR"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "{:>8}ms  {}  {}{}",
            field(s, &["durationMs"]),
            field(s, &["serviceName"]),
            field(s, &["name"]),
            err
        );
    }
    out.trim_end().to_string()
}

/// Error-tracking groups.
pub fn render_error_groups(body: &Value, limit: usize) -> String {
    let groups = rows(body, "groups");
    if groups.is_empty() {
        return "No error groups matched. (The query ran and returned zero rows — nothing is being reported, which is different from nothing being wrong.)"
            .to_string();
    }
    let mut out = format!("{} error group(s):\n", groups.len());
    for g in groups {
        let _ = writeln!(
            out,
            "{}  [{}] {} x{}  {}",
            field(g, &["lastSeenAt", "last_seen_at"]),
            field(g, &["status"]),
            field(g, &["level"]),
            field(g, &["eventCount", "event_count"]),
            field(g, &["title"]),
        );
        let _ = writeln!(out, "    {}  group {}", field(g, &["culprit"]), field(g, &["id"]));
    }
    if body.get("nextCursor").and_then(Value::as_str).is_some() {
        out.push_str("(more groups available — page with the `nextCursor` from --json)\n");
    }
    out.push_str(truncation_note(groups.len(), limit, None).trim_end_matches('\n'));
    out.trim_end().to_string()
}

/// One error group + its events.
pub fn render_error_group(body: &Value, limit: usize) -> String {
    let g = body.get("group").unwrap_or(body);
    let mut out = format!(
        "{}\n  culprit: {}\n  status: {} · level: {} · env: {}\n  {} event(s) from {} user(s), {} → {}\n",
        field(g, &["title"]),
        field(g, &["culprit"]),
        field(g, &["status"]),
        field(g, &["level"]),
        field(g, &["environment"]),
        field(g, &["eventCount", "event_count"]),
        field(g, &["userCount", "user_count"]),
        field(g, &["firstSeenAt", "first_seen_at"]),
        field(g, &["lastSeenAt", "last_seen_at"]),
    );
    let events = if body.get("events").and_then(Value::as_array).is_some() {
        rows(body, "events")
    } else {
        rows(body, "recentEvents")
    };
    if events.is_empty() {
        out.push_str("\nNo individual events stored for this group. (The lookup succeeded — the group's events may have aged out.)");
        return out;
    }
    let _ = writeln!(out, "\n{} recent event(s):", events.len());
    for e in events {
        let _ = writeln!(
            out,
            "  {}  {}  {}",
            field(e, &["occurredAt", "occurred_at"]),
            field(e, &["level"]),
            field(e, &["message"]).replace('\n', " "),
        );
    }
    out.push_str(truncation_note(events.len(), limit, None).trim_end_matches('\n'));
    out.trim_end().to_string()
}

/// LLM agent turns.
pub fn render_llm_turns(body: &Value, limit: usize) -> String {
    let turns = rows(body, "turns");
    let window = field(body, &["windowMinutes"]);
    if turns.is_empty() {
        return format!("No LLM turns recorded in the last {window} minute(s). (The query ran and returned zero rows — `gen_ai_events` may simply have no writer for this org yet.)");
    }
    let mut out = format!("{} turn(s) in the last {window} minute(s):\n", turns.len());
    for t in turns {
        let _ = writeln!(
            out,
            "{}  {:>7}ms  {}  {}  in={} out={}  {}",
            field(t, &["startedAt"]),
            field(t, &["durationMs"]),
            field(t, &["responseModel", "requestModel"]),
            field(t, &["statusCode"]),
            field(t, &["inputTokens"]),
            field(t, &["outputTokens"]),
            field(t, &["spanName", "operation"]),
        );
        if let Some(msg) = pick(t, &["statusMessage"]) {
            let _ = writeln!(out, "    {msg}");
        }
    }
    out.push_str(truncation_note(turns.len(), limit, total_of(body)).trim_end_matches('\n'));
    out.trim_end().to_string()
}

/// Failing tool calls, grouped.
pub fn render_tool_failures(body: &Value, limit: usize) -> String {
    let groups = rows(body, "toolFailures");
    let window = field(body, &["windowMinutes"]);
    if groups.is_empty() {
        return format!("No failing tool calls in the last {window} minute(s). (The query ran and returned zero rows.)");
    }
    let mut out = format!("{} failing tool(s) in the last {window} minute(s):\n", groups.len());
    for g in groups {
        let _ = writeln!(
            out,
            "{}  {} failure(s) of {} call(s)  last {}",
            field(g, &["toolName"]),
            field(g, &["failures"]),
            field(g, &["calls"]),
            field(g, &["lastSeenAt"]),
        );
        if let Some(err) = pick(g, &["lastError"]) {
            let _ = writeln!(out, "    {}", err.replace('\n', " "));
        }
    }
    out.push_str(truncation_note(groups.len(), limit, None).trim_end_matches('\n'));
    out.trim_end().to_string()
}

/// LLM cost breakdown.
pub fn render_llm_cost(body: &Value, limit: usize) -> String {
    let data = rows(body, "rows");
    let window = field(body, &["windowMinutes"]);
    let group_by = field(body, &["groupBy"]);
    // FALSE means nothing emits `gen_ai.usage.cost_usd` — render "not
    // measured", never `$0.00`, which reads as free.
    let priced = body.get("costAvailable").and_then(Value::as_bool) == Some(true);
    if data.is_empty() {
        return format!("No LLM usage recorded by {group_by} in the last {window} minute(s). (The query ran and returned zero rows.)");
    }
    let mut out = format!("Usage by {group_by} over the last {window} minute(s):\n");
    for r in data {
        let cost = if priced {
            format!("${}", field(r, &["costUsd"]))
        } else {
            "cost not measured".to_string()
        };
        let _ = writeln!(
            out,
            "{}  {} call(s)  in={} out={} cached={}  {}",
            field(r, &["label"]),
            field(r, &["calls"]),
            field(r, &["inputTokens"]),
            field(r, &["outputTokens"]),
            field(r, &["cachedTokens"]),
            cost,
        );
    }
    if !priced {
        out.push_str(
            "Cost is NOT available for this org yet (nothing emits gen_ai.usage.cost_usd) — token counts are real, dollars are not zero, they are unknown.\n",
        );
    }
    out.push_str(truncation_note(data.len(), limit, None).trim_end_matches('\n'));
    out.trim_end().to_string()
}

/// Pipeline freshness. This is the one renderer where "empty" is genuinely
/// alarming: no pipes means the health endpoint answered without describing a
/// single pipe, which is a broken answer, not a healthy system.
pub fn render_health(body: &Value) -> String {
    let pipes = rows(body, "pipes");
    if pipes.is_empty() {
        return "The health endpoint answered but reported NO pipes. That is not a clean bill of health — treat observability as unverified.".to_string();
    }
    let mut out = format!("Pipeline freshness at {}:\n", field(body, &["checkedAt"]));
    for p in pipes {
        let last = pick(p, &["lastWriteAt"]);
        let state = match (&last, pick(p, &["error"])) {
            (_, Some(err)) => format!("PROBE FAILED: {err}"),
            (Some(ts), None) => format!("last write {ts}"),
            (None, None) => format!("NO writes in the last {} day(s)", field(body, &["horizonDays"])),
        };
        let _ = writeln!(out, "  {:<14} {:<22} {state}", field(p, &["key"]), field(p, &["store"]));
    }
    let open = field(body, &["openIncidentCount"]);
    let _ = write!(out, "Open monitor incidents: {open}");
    out
}

/// Open uptime incidents.
pub fn render_incidents(body: &Value) -> String {
    let incidents = rows(body, "incidents");
    if incidents.is_empty() {
        return "No OPEN uptime incidents. (The query ran and returned zero rows — monitors are configured and none are alerting.)".to_string();
    }
    let mut out = format!("{} OPEN incident(s):\n", incidents.len());
    for i in incidents {
        let _ = writeln!(
            out,
            "{}  {}  since {}\n    {}",
            field(i, &["monitorName"]),
            field(i, &["url"]),
            field(i, &["startedAt"]),
            field(i, &["lastError"]),
        );
    }
    out.trim_end().to_string()
}

/// Platform audit events (who did what).
pub fn render_audit(body: &Value, limit: usize) -> String {
    let data = rows(body, "data");
    if data.is_empty() {
        return "No audit events matched. (The query ran and returned zero rows.)".to_string();
    }
    let mut out = format!("{} audit event(s):\n", data.len());
    for e in data {
        let _ = writeln!(
            out,
            "{}  {}  {}  {}  {}",
            field(e, &["timestamp"]),
            field(e, &["outcome"]),
            field(e, &["action"]),
            field(e, &["resource_type", "resourceType"]),
            field(e, &["actor_id", "actorId", "actor_type", "actorType"]),
        );
    }
    out.push_str(truncation_note(data.len(), limit, total_of(body)).trim_end_matches('\n'));
    out.trim_end().to_string()
}

/// Registered sourcemaps for a (release, environment).
fn render_sourcemaps(body: &Value, release: &str, environment: &str) -> String {
    let maps = if body.get("sourcemaps").is_some() {
        rows(body, "sourcemaps")
    } else {
        rows(body, "data")
    };
    if maps.is_empty() {
        return format!(
            "No sourcemaps registered for release `{release}` in `{environment}`. (The lookup succeeded — nothing has been uploaded for that pair.)"
        );
    }
    let mut out = format!("{} sourcemap(s) for `{release}` in `{environment}`:\n", maps.len());
    for m in maps {
        let _ = writeln!(out, "  {}", field(m, &["filePath", "file_path", "path"]));
    }
    out.trim_end().to_string()
}

/// Walk `root` and return every file ending in `.map` whose stripped
/// extension corresponds to a JavaScript bundle (`.js`, `.mjs`, `.cjs`).
fn find_sourcemaps(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "map" {
            continue;
        }
        let stripped = p.with_extension("");
        let Some(inner_ext) = stripped.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if matches!(inner_ext, "js" | "mjs" | "cjs") {
            out.push(p.to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is the idiom for test assertions")]
mod tests {
    use super::*;

    // ── Window parsing ──────────────────────────────────────────────────────

    #[test]
    fn since_accepts_every_unit() {
        assert_eq!(parse_since("90s").expect("s"), Duration::seconds(90));
        assert_eq!(parse_since("45m").expect("m"), Duration::minutes(45));
        assert_eq!(parse_since("6h").expect("h"), Duration::hours(6));
        assert_eq!(parse_since("7d").expect("d"), Duration::days(7));
        assert_eq!(parse_since(" 2h ").expect("trimmed"), Duration::hours(2));
    }

    /// A bare number is ambiguous (hours? minutes?) and a silently-wrong window
    /// makes a monitoring answer wrong without looking wrong. Refuse it.
    #[test]
    fn since_rejects_ambiguous_and_nonsense_windows() {
        for bad in ["24", "", "h", "-3h", "0h", "3w", "abc", "1.5h"] {
            assert!(parse_since(bad).is_err(), "`{bad}` must be rejected");
        }
    }

    #[test]
    fn window_brackets_now_and_is_rfc3339_utc() {
        let (start, end) = window("1h").expect("window");
        assert!(start.ends_with('Z') && end.ends_with('Z'), "{start} / {end}");
        let s = chrono::DateTime::parse_from_rfc3339(&start).expect("start parses");
        let e = chrono::DateTime::parse_from_rfc3339(&end).expect("end parses");
        assert!(e > s, "end must follow start");
        assert_eq!((e - s).num_minutes(), 60);
    }

    // ── Query-string + body plumbing ────────────────────────────────────────

    #[test]
    fn query_string_skips_none_and_encodes_values() {
        assert_eq!(qs(&[("a", None)]), "");
        assert_eq!(qs(&[("a", Some("1".into())), ("b", None)]), "?a=1");
        assert_eq!(qs(&[("q", Some("a b&c=d".into()))]), "?q=a%20b%26c%3Dd");
    }

    /// A blank filter must be ABSENT, not a filter on the empty string — the
    /// latter matches nothing and reads as "no results".
    #[test]
    fn blank_filters_are_dropped() {
        assert_eq!(opt(Some("  ")), None);
        assert_eq!(opt(None), None);
        assert_eq!(opt(Some(" x ")), Some("x".to_string()));
        assert_eq!(list(&[]), Value::Null);

        let body = body_from(vec![("search", Value::Null), ("level", list(&[])), ("limit", json!(10))]);
        assert_eq!(body, json!({ "limit": 10 }), "null + empty-array fields must not be sent");
    }

    // ── Rule 1: "no data" never reads like "unavailable" (or like "fine") ───

    /// Every empty-result renderer must say the query RAN. A blank or bare
    /// "none" is what lets an agent report "everything is healthy" when it
    /// actually could not see.
    #[test]
    fn empty_results_state_that_the_query_ran() {
        let cases: Vec<String> = vec![
            render_logs(&json!({ "data": [], "total": 0 }), 50),
            render_traces(&json!({ "traces": [] }), 50),
            render_error_groups(&json!({ "groups": [] }), 20),
            render_llm_turns(&json!({ "turns": [], "windowMinutes": 60 }), 50),
            render_tool_failures(&json!({ "toolFailures": [], "windowMinutes": 60 }), 20),
            render_llm_cost(&json!({ "rows": [], "groupBy": "model", "windowMinutes": 60 }), 20),
            render_fields(&json!({ "fields": [] }), 50),
            render_facets(&json!({})),
            render_incidents(&json!({ "incidents": [] })),
            render_audit(&json!({ "data": [] }), 50),
        ];
        for text in cases {
            let lower = text.to_lowercase();
            assert!(!text.trim().is_empty(), "an empty result must still render text");
            assert!(
                lower.contains("no ") || lower.contains("none"),
                "an empty result must say so explicitly: {text}"
            );
            assert!(
                lower.contains("ran") || lower.contains("returned") || lower.contains("succeeded"),
                "an empty result must make clear the query SUCCEEDED, so it can't be read as an outage: {text}"
            );
        }
    }

    /// Zero pipes is a broken health answer, not a healthy one — it must never
    /// render as reassurance.
    #[test]
    fn health_without_pipes_refuses_to_read_as_healthy() {
        let text = render_health(&json!({ "pipes": [], "checkedAt": "2026-08-16T00:00:00Z" }));
        assert!(text.contains("NO pipes"), "{text}");
        assert!(text.to_lowercase().contains("unverified"), "{text}");
    }

    #[test]
    fn health_distinguishes_stale_from_failed_probes() {
        let text = render_health(&json!({
            "checkedAt": "2026-08-16T00:00:00Z",
            "horizonDays": 7,
            "openIncidentCount": 2,
            "pipes": [
                { "key": "logs", "store": "clickhouse", "lastWriteAt": "2026-08-16T00:00:00Z" },
                { "key": "traces", "store": "clickhouse", "lastWriteAt": null },
                { "key": "metrics", "store": "postgres", "lastWriteAt": null, "error": "pool timeout" },
            ],
        }));
        assert!(text.contains("last write 2026-08-16T00:00:00Z"), "{text}");
        assert!(text.contains("NO writes in the last 7 day(s)"), "a dead pipe must be named: {text}");
        assert!(
            text.contains("PROBE FAILED: pool timeout"),
            "a failed probe must NOT look like a quiet pipe: {text}"
        );
        assert!(text.contains("Open monitor incidents: 2"), "{text}");
    }

    /// `costAvailable: false` means "not measured". Rendering `$0.00` there
    /// would report free where the truth is unknown.
    #[test]
    fn unmeasured_cost_never_renders_as_zero_dollars() {
        let text = render_llm_cost(
            &json!({
                "groupBy": "model",
                "windowMinutes": 60,
                "costAvailable": false,
                "rows": [{ "label": "gpt-5", "calls": 3, "inputTokens": 10, "outputTokens": 20, "cachedTokens": 0, "costUsd": 0.0 }],
            }),
            20,
        );
        assert!(text.contains("cost not measured"), "{text}");
        assert!(text.contains("NOT available"), "{text}");
        assert!(!text.contains("$0"), "an unpriced row must not render a dollar figure: {text}");
    }

    // ── Rule 2: truncation is always reported ───────────────────────────────

    #[test]
    fn a_full_page_reports_that_there_may_be_more() {
        let two = json!({ "data": [
            { "timestamp": "t1", "level": "ERROR", "message": "boom" },
            { "timestamp": "t2", "level": "INFO", "message": "ok" },
        ]});
        let capped = render_logs(&two, 2);
        assert!(capped.contains("there may be more"), "a full page must flag truncation: {capped}");

        let roomy = render_logs(&two, 50);
        assert!(!roomy.contains("there may be more"), "a partial page must NOT claim truncation: {roomy}");
    }

    #[test]
    fn a_known_total_larger_than_the_page_is_reported_exactly() {
        let body = json!({ "total": 91, "traces": [{ "traceId": "abc", "durationMs": 12 }] });
        let text = render_traces(&body, 50);
        assert!(text.contains("showing 1 of 91"), "{text}");
    }

    /// `logs trace` has no caller limit, so it must never claim truncation.
    #[test]
    fn unlimited_render_never_claims_truncation() {
        let body = json!({ "data": [{ "timestamp": "t", "level": "INFO", "message": "m" }] });
        assert!(!render_logs(&body, usize::MAX).contains("may be more"));
    }

    // ── Field extraction ────────────────────────────────────────────────────

    #[test]
    fn pick_takes_the_first_present_non_empty_key() {
        let v = json!({ "a": "", "b": "second", "c": 3, "d": true });
        assert_eq!(pick(&v, &["a", "b"]), Some("second".to_string()));
        assert_eq!(pick(&v, &["c"]), Some("3".to_string()));
        assert_eq!(pick(&v, &["d"]), Some("true".to_string()));
        assert_eq!(pick(&v, &["missing"]), None);
        assert_eq!(field(&v, &["missing"]), "-");
    }

    /// The ClickHouse projections speak snake_case and the Postgres serializers
    /// camelCase; both must render.
    #[test]
    fn renderers_tolerate_snake_and_camel_case() {
        let snake = render_error_groups(
            &json!({ "groups": [{ "title": "T", "last_seen_at": "then", "event_count": 4, "status": "unresolved", "level": "error", "id": "g1" }] }),
            20,
        );
        assert!(snake.contains("then") && snake.contains("x4"), "{snake}");
        let camel = render_error_groups(
            &json!({ "groups": [{ "title": "T", "lastSeenAt": "then", "eventCount": 4, "status": "unresolved", "level": "error", "id": "g1" }] }),
            20,
        );
        assert!(camel.contains("then") && camel.contains("x4"), "{camel}");
    }

    #[test]
    fn error_group_renders_group_and_events() {
        let text = render_error_group(
            &json!({
                "group": { "title": "TypeError: x", "culprit": "app.js", "status": "unresolved", "level": "error",
                           "environment": "production", "eventCount": 12, "userCount": 3,
                           "firstSeenAt": "a", "lastSeenAt": "b" },
                "events": [{ "occurredAt": "b", "level": "error", "message": "x is not a function" }],
            }),
            10,
        );
        assert!(text.contains("TypeError: x") && text.contains("app.js"), "{text}");
        assert!(text.contains("x is not a function"), "{text}");
    }

    #[test]
    fn error_group_without_events_says_the_lookup_succeeded() {
        let text = render_error_group(&json!({ "group": { "title": "T" }, "events": [] }), 10);
        assert!(text.contains("No individual events"), "{text}");
        assert!(text.contains("succeeded"), "{text}");
    }

    #[test]
    fn fields_renders_both_the_key_list_and_one_keys_values() {
        let keys = render_fields(&json!({ "fields": [{ "key": "userId", "count": 9 }] }), 50);
        assert!(keys.contains("userId") && keys.contains("(9)"), "{keys}");
        let values = render_fields(&json!({ "key": "userId", "values": [{ "value": "u-42", "count": 2 }] }), 50);
        assert!(values.contains("u-42") && values.contains("`userId`"), "{values}");
    }

    #[test]
    fn spans_render_and_flag_errors() {
        let text = render_spans(
            &json!({ "spans": [
                { "name": "GET /x", "serviceName": "web", "durationMs": 12, "hasError": true },
                { "name": "db.query", "serviceName": "web", "durationMs": 3, "hasError": false },
            ]}),
            "trace-1",
        );
        assert!(text.contains("trace-1"), "{text}");
        assert!(text.contains("GET /x") && text.contains("ERROR"), "{text}");
        assert!(!text.contains("db.query ERROR"), "a clean span must not be flagged: {text}");
    }

    #[test]
    fn missing_spans_blames_the_id_not_the_system() {
        let text = render_spans(&json!({ "spans": [] }), "nope");
        assert!(text.contains("nope") && text.contains("succeeded"), "{text}");
    }

    #[test]
    fn sourcemaps_list_renders_paths_and_an_honest_empty() {
        let empty = render_sourcemaps(&json!({ "sourcemaps": [] }), "v1", "production");
        assert!(empty.contains("No sourcemaps") && empty.contains("succeeded"), "{empty}");
        let one = render_sourcemaps(&json!({ "sourcemaps": [{ "filePath": "static/main.js" }] }), "v1", "production");
        assert!(one.contains("static/main.js"), "{one}");
    }

    #[test]
    fn log_messages_are_flattened_to_one_line_each() {
        let text = render_logs(&json!({ "data": [{ "timestamp": "t", "level": "ERROR", "message": "a\nb" }] }), 50);
        assert!(text.contains("a b"), "a multi-line message must not break the table: {text}");
    }

    // ── clap wiring ─────────────────────────────────────────────────────────

    #[test]
    fn every_query_verb_parses_with_json() {
        use clap::{CommandFactory, Parser};

        #[derive(Parser)]
        struct Harness {
            #[command(subcommand)]
            cmd: Cmd,
        }
        Harness::command().debug_assert();

        for argv in [
            vec!["th", "logs", "query", "--query", "boom", "--level", "ERROR", "--since", "2h", "--json"],
            vec!["th", "logs", "facets", "--hours", "6", "--json"],
            vec!["th", "logs", "fields", "--key", "userId", "--json"],
            vec!["th", "logs", "trace", "abc123", "--json"],
            vec!["th", "traces", "query", "--status", "error", "--json"],
            vec!["th", "traces", "show", "abc123", "--json"],
            vec!["th", "errors", "top", "--status", "unresolved", "--json"],
            vec!["th", "errors", "show", "g-1", "--events", "5", "--json"],
            vec!["th", "llm", "turns", "--model", "gpt-5", "--json"],
            vec!["th", "llm", "tool-failures", "--json"],
            vec!["th", "llm", "cost", "--group-by", "agent", "--json"],
            vec!["th", "health", "--json"],
            vec!["th", "monitors", "--json"],
            vec!["th", "monitor", "--json"],
            vec![
                "th",
                "audit",
                "--query",
                "login",
                "--action",
                "member.invite",
                "--since",
                "7d",
                "--limit",
                "10",
                "--json",
            ],
            vec!["th", "audit"],
            vec!["th", "sourcemaps-list", "--release", "v1", "--environment", "production", "--json"],
        ] {
            Harness::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
        }
    }

    /// The audit window default is load-bearing: a silently wider or narrower
    /// one makes "nothing happened" mean something different than it says.
    #[test]
    fn audit_defaults_to_a_24h_window() {
        use clap::Parser;

        #[derive(Parser)]
        struct Harness {
            #[command(subcommand)]
            cmd: Cmd,
        }
        let Cmd::Audit {
            since, limit, query, action, ..
        } = Harness::try_parse_from(["th", "audit"]).expect("parses").cmd
        else {
            panic!("expected the audit verb");
        };
        assert_eq!(since, "24h");
        assert_eq!(limit, 50);
        assert_eq!(query, None);
        assert!(action.is_empty());
    }
}

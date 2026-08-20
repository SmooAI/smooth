//! `smoo analytics …` — the analytics warehouse + GA4 read surface.
//!
//! CLI parity with the hosted `mcp.smoo.ai` tools `analytics_catalog`,
//! `analytics_query` and `analytics_report` (pearl th-739bb1). Same routes,
//! same rules:
//!
//! 1. *"No data" and "unavailable" never render the same.* A failed request is
//!    an `Err` all the way out; a successful empty result says so in words.
//! 2. *Truncation is always reported.* When fewer rows print than the server
//!    counted, the summary says so.

use std::fmt::Write as _;
use std::path::PathBuf;

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use smooth_api_client::SmoothApiClient;

use super::observability::Common;
use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// The preset queries and custom data sources available to this org.
    ///
    /// Run this before `query` to pick a preset; pass a data-source id to see
    /// that source's columns (the grounding you need to write ad-hoc SQL).
    Catalog {
        /// A custom data source id (from a bare `catalog` call) to describe
        /// its columns instead of listing the catalog.
        data_source_id: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Run a warehouse query: a preset key, or ad-hoc ClickHouse SELECT.
    ///
    /// Ad-hoc SQL is SELECT-only, a single statement, and MUST reference the
    /// bound `{orgId: String}` parameter — the warehouse is multi-tenant and
    /// the query is not rewritten for you. `{startDate: DateTime64(3, 'UTC')}`
    /// and `{endDate: …}` are bound too. All guards (SELECT-only, org scoping,
    /// cost pre-flight) run server-side.
    Query {
        /// Ad-hoc SELECT SQL. Alternatively `--file` or `--preset`.
        sql: Option<String>,
        /// Read the SQL from a file instead of the command line.
        #[arg(long, conflicts_with = "sql")]
        file: Option<PathBuf>,
        /// A preset key from `catalog`. Wins over SQL if both are given.
        #[arg(long)]
        preset: Option<String>,
        /// Window start, ISO-8601 (bound as `{startDate}`; default 30 days ago).
        #[arg(long)]
        start_date: Option<String>,
        /// Window end, ISO-8601 (bound as `{endDate}`; default now).
        #[arg(long)]
        end_date: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Google Analytics 4 for the org's connected Google account.
    ///
    /// `properties` lists the GA4 properties you can report on — start there.
    /// Then `overview`, `top-pages` or `traffic` with `--property-id`.
    Report {
        /// properties | overview | top-pages | traffic.
        #[arg(default_value = "overview")]
        report: String,
        /// The GA4 property, from `report properties`. Required for every
        /// report except `properties`.
        #[arg(long)]
        property_id: Option<String>,
        /// Trailing window in days (default 30). GA4 serves fixed windows, so
        /// this SNAPS up: 1–7 → 7d, 8–30 → 30d, larger → 90d.
        #[arg(long)]
        days: Option<u64>,
        /// Rows for `top-pages` (1–50, default 20).
        #[arg(long)]
        limit: Option<u64>,
        #[command(flatten)]
        common: Common,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Catalog { data_source_id, common } => {
            let org = require_active_org(&client, common.org)?;
            if let Some(id) = data_source_id {
                let resp = data_source_catalog(&client, &org, &id).await?;
                emit(&resp, common.json, render_data_source);
            } else {
                let resp = catalog(&client, &org).await?;
                emit(&resp, common.json, render_catalog);
            }
        }
        Cmd::Query {
            sql,
            file,
            preset,
            start_date,
            end_date,
            common,
        } => {
            let org = require_active_org(&client, common.org)?;
            let sql = sql_from(sql, file)?;
            if sql.is_none() && preset.is_none() {
                bail!("provide SELECT sql (positional or --file), or --preset <key> from `smoo analytics catalog`");
            }
            let resp = query(&client, &org, preset.as_deref(), sql.as_deref(), start_date.as_deref(), end_date.as_deref()).await?;
            emit(&resp, common.json, render_query);
        }
        Cmd::Report {
            report,
            property_id,
            days,
            limit,
            common,
        } => {
            let org = require_active_org(&client, common.org)?;
            let kind = report_kind(&report, property_id.as_deref())?;
            let resp = ga_report(&client, &org, kind, property_id.as_deref(), days, limit).await?;
            emit(&resp, common.json, |r| render_report(r, kind));
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

/// The SQL text: positional wins by clap `conflicts_with`, `--file` reads a
/// file. `None` when neither was given.
fn sql_from(sql: Option<String>, file: Option<PathBuf>) -> Result<Option<String>> {
    match (sql, file) {
        (Some(s), _) => Ok(Some(s)),
        (None, Some(path)) => {
            let text = std::fs::read_to_string(&path).with_context(|| format!("read SQL from {}", path.display()))?;
            if text.trim().is_empty() {
                bail!("{} is empty — nothing to run", path.display());
            }
            Ok(Some(text))
        }
        (None, None) => Ok(None),
    }
}

/// Validate the GA4 report kind — mirrors the hosted MCP tool exactly:
/// `properties` is the discovery step so it cannot need a property id;
/// everything else does.
fn report_kind<'a>(report: &'a str, property_id: Option<&str>) -> Result<&'a str> {
    match report {
        "properties" => return Ok("properties"),
        "overview" | "top-pages" | "traffic" => {}
        other => bail!("unknown report `{other}` — use properties, overview, top-pages or traffic"),
    }
    if property_id.unwrap_or_default().trim().is_empty() {
        bail!("`{report}` needs --property-id — run `smoo analytics report properties` to list them");
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// Queries — one function per route, mirroring the hosted MCP tools
// ---------------------------------------------------------------------------

/// The preset catalog + custom data sources, merged into
/// `{ presets, dataSources, dataSourcesUnavailable? }`.
///
/// Custom data sources are a separate product (`analyticsCustom`), so an org
/// without it gets a 403 there while its presets are fine — that renders as
/// "none", not as a failure. Any OTHER data-sources error surfaces: a timeout
/// rendering as "no custom data sources" would be a confident lie (th-ed81e4).
///
/// # Errors
/// Non-2xx from the presets route, or a non-403 from the data-sources route.
pub async fn catalog(client: &SmoothApiClient, org: &str) -> Result<Value> {
    let presets = client
        .get(&format!("/organizations/{org}/analytics/preset-queries"))
        .await
        .context("GET analytics/preset-queries")?;
    let mut out = json!({ "presets": unwrap_list(presets, "presets") });
    match client.get(&format!("/organizations/{org}/analytics/data-sources")).await {
        Ok(sources) => {
            out["dataSources"] = unwrap_list(sources, "dataSources");
        }
        // The client's error format is "{method} {path} returned HTTP {status}: …".
        Err(e) if e.to_string().contains("returned HTTP 403") => {
            out["dataSources"] = json!([]);
            out["dataSourcesUnavailable"] = json!("this org does not have the custom data sources product");
        }
        Err(e) => return Err(e).context("GET analytics/data-sources"),
    }
    Ok(out)
}

/// The API's list routes answer either as a bare array or as an object keyed
/// by `key` (both conventions exist upstream) — normalize to the array.
fn unwrap_list(body: Value, key: &str) -> Value {
    if body.is_array() {
        body
    } else {
        body.get(key).cloned().unwrap_or_else(|| json!([]))
    }
}

/// `GET /analytics/data-sources/{id}/catalog` — one source's columns.
///
/// # Errors
/// Non-2xx from the API.
pub async fn data_source_catalog(client: &SmoothApiClient, org: &str, id: &str) -> Result<Value> {
    client
        .get(&format!("/organizations/{org}/analytics/data-sources/{}/catalog", urlencoding::encode(id)))
        .await
        .context("GET analytics/data-sources catalog")
}

/// `POST /analytics/query` → `{ columns, rows, rowCount }`.
///
/// # Errors
/// Non-2xx from the API — including the server-side SELECT-only / org-scoping
/// / cost-pre-flight refusals, which come back verbatim.
pub async fn query(
    client: &SmoothApiClient,
    org: &str,
    preset_key: Option<&str>,
    sql: Option<&str>,
    start_date: Option<&str>,
    end_date: Option<&str>,
) -> Result<Value> {
    let mut body = serde_json::Map::new();
    for (key, value) in [("presetKey", preset_key), ("sql", sql), ("startDate", start_date), ("endDate", end_date)] {
        if let Some(v) = value.map(str::trim).filter(|s| !s.is_empty()) {
            body.insert(key.to_string(), json!(v));
        }
    }
    client
        .post(&format!("/organizations/{org}/analytics/query"), Some(&Value::Object(body)))
        .await
        .context("POST analytics/query")
}

/// GA4 reads: `GET /analytics/google/properties` for the discovery step, else
/// `GET /analytics/google/{overview|top-pages|traffic}?propertyId=&days=&limit=`.
///
/// # Errors
/// Non-2xx from the API (e.g. no Google account connected).
pub async fn ga_report(client: &SmoothApiClient, org: &str, kind: &str, property_id: Option<&str>, days: Option<u64>, limit: Option<u64>) -> Result<Value> {
    if kind == "properties" {
        return client
            .get(&format!("/organizations/{org}/analytics/google/properties"))
            .await
            .context("GET analytics/google/properties");
    }
    let mut qs = format!("?propertyId={}", urlencoding::encode(property_id.unwrap_or_default()));
    if let Some(d) = days {
        let _ = write!(qs, "&days={d}");
    }
    if let Some(l) = limit {
        let _ = write!(qs, "&limit={l}");
    }
    client
        .get(&format!("/organizations/{org}/analytics/google/{kind}{qs}"))
        .await
        .with_context(|| format!("GET analytics/google/{kind}"))
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

/// The array under `key` — or the body itself when the route answers as a
/// bare array (both conventions exist upstream). Missing and empty both mean
/// "the query succeeded and matched nothing", said in words by the callers.
fn rows<'a>(body: &'a Value, key: &str) -> &'a [Value] {
    body.as_array().or_else(|| body.get(key).and_then(Value::as_array)).map_or(&[], Vec::as_slice)
}

/// A value compact enough for one table cell.
fn cell(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "-".to_string(),
        other => other.to_string(),
    }
}

fn field(v: &Value, key: &str) -> String {
    v.get(key).map_or_else(|| "-".to_string(), cell)
}

/// The preset catalog + data sources.
pub fn render_catalog(body: &Value) -> String {
    let presets = rows(body, "presets");
    let mut out = String::new();
    if presets.is_empty() {
        out.push_str("No preset queries on this org. (The catalog read succeeded and returned zero presets.)\n");
    } else {
        let _ = writeln!(out, "{} preset quer(ies) — pass a key to `smoo analytics query --preset`:", presets.len());
        for p in presets {
            let _ = writeln!(out, "  {}  [{}]  {}", field(p, "key"), field(p, "domain"), field(p, "description"));
        }
    }
    out.push('\n');
    if let Some(reason) = body.get("dataSourcesUnavailable").and_then(Value::as_str) {
        let _ = write!(out, "No custom data sources — {reason}.");
    } else {
        let sources = rows(body, "dataSources");
        if sources.is_empty() {
            out.push_str("No custom data sources on this org. (The read succeeded and returned zero sources.)");
        } else {
            let _ = writeln!(out, "{} custom data source(s) — pass an id back to `catalog` for its columns:", sources.len());
            for s in sources {
                let _ = writeln!(
                    out,
                    "  {}  [{}]  {}  {}",
                    field(s, "id"),
                    field(s, "status"),
                    field(s, "name"),
                    field(s, "tableName")
                );
            }
        }
    }
    out.trim_end().to_string()
}

/// One data source's columns.
pub fn render_data_source(body: &Value) -> String {
    let mut out = format!(
        "{}  (table {}, {} row(s))\n{}\n",
        field(body, "name"),
        field(body, "tableName"),
        field(body, "rowCount"),
        field(body, "description"),
    );
    let columns = rows(body, "columns");
    if columns.is_empty() {
        out.push_str("\nNo columns described. (The read succeeded — the source may still be ingesting.)");
        return out.trim_end().to_string();
    }
    let _ = writeln!(out, "\n{} column(s):", columns.len());
    for c in columns {
        let _ = writeln!(
            out,
            "  {}  {}  ({})  nullable={}",
            field(c, "name"),
            field(c, "clickhouseType"),
            field(c, "semanticType"),
            field(c, "nullable"),
        );
    }
    out.trim_end().to_string()
}

/// Warehouse query rows, in the route's own column order.
pub fn render_query(body: &Value) -> String {
    let data = rows(body, "rows");
    if data.is_empty() {
        return "The query ran and matched no rows. (That is a real answer, not a failure — widen the date window or check the preset/SQL.)".to_string();
    }
    // The route's projection order; fall back to the first row's own keys so a
    // missing `columns` never renders every row blank.
    let mut columns: Vec<String> = body
        .get("columns")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if columns.is_empty() {
        columns = data.first().and_then(Value::as_object).map(|o| o.keys().cloned().collect()).unwrap_or_default();
    }
    let mut out = format!("{} row(s):\n", data.len());
    for r in data {
        let line: Vec<String> = columns.iter().map(|c| format!("{c}={}", field(r, c))).collect();
        let _ = writeln!(out, "  {}", line.join("  "));
    }
    if let Some(total) = body.get("rowCount").and_then(Value::as_u64) {
        // A total too big for usize is certainly bigger than the page.
        if usize::try_from(total).unwrap_or(usize::MAX) > data.len() {
            let _ = write!(out, "(showing {} of {total} rows — the server truncated the page)", data.len());
        }
    }
    out.trim_end().to_string()
}

/// One GA4 report, shaped per kind.
pub fn render_report(body: &Value, kind: &str) -> String {
    match kind {
        "properties" => {
            let props = rows(body, "properties");
            if props.is_empty() {
                return "No GA4 properties visible. (The read succeeded — the connected Google account may not have Analytics access, or no account is connected.)".to_string();
            }
            let mut out = format!("{} GA4 propert(ies) — pass one to --property-id:\n", props.len());
            for p in props {
                let _ = writeln!(out, "  {}  {}  ({})", field(p, "propertyId"), field(p, "displayName"), field(p, "accountName"));
            }
            out.trim_end().to_string()
        }
        // A single object of totals, not a list.
        "overview" => format!(
            "sessions={}  users={}  pageviews={}  bounceRate={}",
            field(body, "sessions"),
            field(body, "users"),
            field(body, "pageviews"),
            field(body, "bounceRate"),
        ),
        "top-pages" => {
            let pages = rows(body, "pages");
            if pages.is_empty() {
                return "No pages in this window. (The report ran and returned zero rows.)".to_string();
            }
            let mut out = format!("{} page(s):\n", pages.len());
            for p in pages {
                let _ = writeln!(
                    out,
                    "  {:>8} views  {:>7} users  {}  {}",
                    field(p, "pageviews"),
                    field(p, "users"),
                    field(p, "path"),
                    field(p, "title"),
                );
            }
            out.trim_end().to_string()
        }
        _ => {
            let days = rows(body, "traffic");
            if days.is_empty() {
                return "No traffic in this window. (The report ran and returned zero rows.)".to_string();
            }
            let mut out = format!("{} day(s) of traffic:\n", days.len());
            for d in days {
                let _ = writeln!(
                    out,
                    "  {}  sessions={}  users={}  pageviews={}",
                    field(d, "date"),
                    field(d, "sessions"),
                    field(d, "users"),
                    field(d, "pageviews"),
                );
            }
            out.trim_end().to_string()
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is the idiom for test assertions")]
mod tests {
    use super::*;

    // ── clap wiring ─────────────────────────────────────────────────────────

    #[test]
    fn every_verb_parses_with_json() {
        use clap::{CommandFactory, Parser};

        #[derive(Parser)]
        struct Harness {
            #[command(subcommand)]
            cmd: Cmd,
        }
        Harness::command().debug_assert();

        for argv in [
            vec!["smoo", "catalog", "--json"],
            vec!["smoo", "catalog", "ds-123", "--json"],
            vec!["smoo", "query", "SELECT 1", "--json"],
            vec!["smoo", "query", "--file", "q.sql", "--json"],
            vec![
                "smoo",
                "query",
                "--preset",
                "conversations-by-day",
                "--start-date",
                "2026-08-01",
                "--end-date",
                "2026-08-20",
            ],
            vec!["smoo", "report", "properties", "--json"],
            vec!["smoo", "report", "overview", "--property-id", "123", "--days", "7"],
            vec!["smoo", "report", "top-pages", "--property-id", "123", "--limit", "10", "--json"],
            vec!["smoo", "report", "--property-id", "123"],
        ] {
            Harness::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
        }
    }

    /// Positional SQL and `--file` are the same argument twice — refuse both.
    #[test]
    fn query_refuses_sql_and_file_together() {
        use clap::Parser;

        #[derive(Parser)]
        struct Harness {
            #[command(subcommand)]
            cmd: Cmd,
        }
        assert!(Harness::try_parse_from(["smoo", "query", "SELECT 1", "--file", "q.sql"]).is_err());
    }

    // ── SQL sources ─────────────────────────────────────────────────────────

    #[test]
    fn sql_from_prefers_positional_reads_file_and_rejects_empty() {
        assert_eq!(sql_from(Some("SELECT 1".into()), None).expect("positional"), Some("SELECT 1".to_string()));
        assert_eq!(sql_from(None, None).expect("neither"), None);

        let dir = std::env::temp_dir().join(format!("smoo-analytics-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("q.sql");
        std::fs::write(&path, "SELECT count() FROM conversations WHERE org_id = {orgId: String}").expect("write");
        let from_file = sql_from(None, Some(path)).expect("file").expect("some");
        assert!(from_file.contains("{orgId: String}"));

        let empty = dir.join("empty.sql");
        std::fs::write(&empty, "  \n").expect("write");
        assert!(sql_from(None, Some(empty)).is_err(), "an empty file must be an error, not an empty query");
        assert!(sql_from(None, Some(dir.join("missing.sql"))).is_err());
    }

    // ── report-kind validation (mirrors the hosted MCP tool) ────────────────

    #[test]
    fn report_kind_gates_property_id_like_the_mcp_tool() {
        assert_eq!(report_kind("properties", None).expect("discovery step"), "properties");
        assert_eq!(report_kind("overview", Some("123")).expect("with id"), "overview");
        let err = report_kind("overview", None).expect_err("overview needs an id").to_string();
        assert!(err.contains("--property-id") && err.contains("properties"), "must say where to get one: {err}");
        assert!(report_kind("traffic", Some("  ")).is_err(), "a blank id is no id");
        assert!(report_kind("bogus", Some("123")).is_err());
    }

    // ── Rule 1: "no data" never reads like "unavailable" ────────────────────

    #[test]
    fn empty_results_state_that_the_query_ran() {
        let cases = vec![
            render_query(&serde_json::json!({ "rows": [], "columns": ["a"] })),
            render_report(&serde_json::json!({ "properties": [] }), "properties"),
            render_report(&serde_json::json!({ "pages": [] }), "top-pages"),
            render_report(&serde_json::json!({ "traffic": [] }), "traffic"),
            render_catalog(&serde_json::json!({ "presets": [], "dataSources": [] })),
        ];
        for text in cases {
            let lower = text.to_lowercase();
            assert!(!text.trim().is_empty(), "an empty result must still render text");
            assert!(lower.contains("no ") || lower.contains("none"), "must say it is empty: {text}");
            assert!(
                lower.contains("ran") || lower.contains("returned") || lower.contains("succeeded"),
                "must make clear the query SUCCEEDED: {text}"
            );
        }
    }

    /// A 403 on custom data sources is a product gate, not an outage — it
    /// renders as "none, because …", while presets still print.
    #[test]
    fn gated_data_sources_render_as_none_with_the_reason() {
        let text = render_catalog(&serde_json::json!({
            "presets": [{ "key": "conv-by-day", "name": "Conversations by day", "description": "d", "domain": "conversations" }],
            "dataSources": [],
            "dataSourcesUnavailable": "this org does not have the custom data sources product",
        }));
        assert!(text.contains("conv-by-day"), "{text}");
        assert!(text.contains("does not have"), "{text}");
    }

    // ── Rule 2: truncation is always reported ───────────────────────────────

    #[test]
    fn a_truncated_query_page_says_so() {
        let body = serde_json::json!({ "columns": ["day", "n"], "rows": [{ "day": "2026-08-19", "n": 4 }], "rowCount": 91 });
        let text = render_query(&body);
        assert!(text.contains("showing 1 of 91"), "{text}");

        let full = serde_json::json!({ "columns": ["day"], "rows": [{ "day": "d" }], "rowCount": 1 });
        assert!(!render_query(&full).contains("showing"), "a complete page must not claim truncation");
    }

    // ── Rendering details ───────────────────────────────────────────────────

    /// Rows print in the route's own column order, not object-key order, and a
    /// missing `columns` falls back to the row's keys rather than blank lines.
    #[test]
    fn query_rows_follow_the_columns_projection() {
        let body = serde_json::json!({
            "columns": ["b", "a"],
            "rows": [{ "a": 1, "b": "x" }],
        });
        let text = render_query(&body);
        let b = text.find("b=x").expect("b renders");
        let a = text.find("a=1").expect("a renders");
        assert!(b < a, "projection order must win: {text}");

        let no_columns = serde_json::json!({ "rows": [{ "a": 1 }] });
        assert!(render_query(&no_columns).contains("a=1"), "missing columns must fall back to row keys");
    }

    /// Upstream list routes answer either `{key: [...]}` or a bare array —
    /// both must render, and both must normalize in the catalog merge.
    #[test]
    fn list_shapes_tolerate_keyed_and_bare_arrays() {
        let item = serde_json::json!({ "propertyId": "1", "displayName": "d", "accountName": "a" });
        let keyed = serde_json::json!({ "properties": [item.clone()] });
        let bare = serde_json::json!([item]);
        assert!(render_report(&keyed, "properties").contains("d"));
        assert!(render_report(&bare, "properties").contains("d"));

        assert_eq!(unwrap_list(serde_json::json!([1, 2]), "presets"), serde_json::json!([1, 2]));
        assert_eq!(unwrap_list(serde_json::json!({ "presets": [3] }), "presets"), serde_json::json!([3]));
        assert_eq!(unwrap_list(serde_json::json!({}), "presets"), serde_json::json!([]));
    }

    #[test]
    fn overview_renders_totals_and_properties_render_ids() {
        let overview = render_report(
            &serde_json::json!({ "sessions": 10, "users": 5, "pageviews": 30, "bounceRate": 0.4 }),
            "overview",
        );
        assert!(overview.contains("sessions=10") && overview.contains("bounceRate=0.4"), "{overview}");

        let props = render_report(
            &serde_json::json!({ "properties": [{ "propertyId": "123", "displayName": "smoo.ai", "accountName": "Smoo" }] }),
            "properties",
        );
        assert!(props.contains("123") && props.contains("smoo.ai"), "{props}");
    }
}

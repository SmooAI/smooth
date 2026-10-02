//! `th smoo dashboards …` — the org's analytics dashboards and their widgets
//! (SMOODEV-3610). CLI twin of the copilot's `dashboards.*` tools and the hosted
//! MCP `dashboards_*` tools.
//!
//! | verb            | route                                                              |
//! |-----------------|--------------------------------------------------------------------|
//! | `list`          | `GET    /organizations/{org}/analytics/dashboards`                 |
//! | `create`        | `POST   /organizations/{org}/analytics/dashboards` (idempotent)    |
//! | `add-widget`    | `POST   /organizations/{org}/analytics/dashboards/{id}/widgets`    |
//! | `update-widget` | `PATCH  /organizations/{org}/analytics/widgets/{id}`               |
//! | `remove-widget` | `DELETE /organizations/{org}/analytics/widgets/{id}`               |
//!
//! Upstream gates reads on `analytics.read`, writes on `analytics.write`, and
//! everything on the `analytics` product.
//!
//! ## No raw SQL
//! A widget's data source is a preset key (`--preset`, from `th smoo analytics
//! catalog`) or one of the org's saved queries (`--saved-query`). There is no
//! SQL flag: the saved query's STORED sql is copied into `customQuery`, the same
//! thing the web `buildWidgetPayload` and the copilot do. Changing what a widget
//! queries means removing it and adding a new one; `update-widget` has no
//! source flags on purpose.
//!
//! Every write is destructive in the copilot (it reshapes a dashboard other
//! people use), so each one goes through [`crate::destructive`]: it prints the
//! target, `--dry-run` stops there, and a non-interactive run needs `--yes`.

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};

use super::{print_json, require_authed};
use crate::destructive::{Confirm, Severity, Target};

/// The `analytics_widget_type` Postgres enum values (mirrors `WIDGET_TYPES` in
/// smooai `rust/smooth-operator-agent/src/tools_dashboards.rs`).
pub const WIDGET_TYPES: &[&str] = &[
    "kpi_card",
    "line_chart",
    "area_chart",
    "bar_chart",
    "pie_chart",
    "donut_chart",
    "funnel_chart",
    "table",
];

const DEFAULT_WIDTH: i64 = 6;
const DEFAULT_HEIGHT: i64 = 4;

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's analytics dashboards.
    List {
        /// Print raw JSON instead of the listing.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a dashboard. A dashboard with the same name (case-insensitive)
    /// is reused rather than duplicated.
    Create {
        /// Dashboard name.
        #[arg(long)]
        name: String,
        /// Optional one-line description.
        #[arg(long)]
        description: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Add a widget to a dashboard. The source is a preset key OR a saved
    /// query — never raw SQL.
    AddWidget {
        /// Target dashboard id (from `list`).
        dashboard_id: String,
        /// Widget title.
        #[arg(long)]
        title: String,
        /// Chart type: kpi_card | line_chart | area_chart | bar_chart | pie_chart | donut_chart | funnel_chart | table.
        #[arg(long = "type")]
        widget_type: String,
        /// Preset key from `th smoo analytics catalog`.
        #[arg(long, conflicts_with = "saved_query")]
        preset: Option<String>,
        /// Id of one of the org's saved analytics queries.
        #[arg(long)]
        saved_query: Option<String>,
        /// Grid column (12-column grid; default 0).
        #[arg(long)]
        col: Option<i64>,
        /// Grid row (default: the next free row).
        #[arg(long)]
        row: Option<i64>,
        /// Width in columns (default 6).
        #[arg(long)]
        width: Option<i64>,
        /// Height in rows (default 4).
        #[arg(long)]
        height: Option<i64>,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Retitle, retype, move/resize a widget, or replace its display options.
    UpdateWidget {
        /// Widget id.
        widget_id: String,
        #[arg(long)]
        title: Option<String>,
        /// New chart type (see `add-widget --type`).
        #[arg(long = "type")]
        widget_type: Option<String>,
        #[arg(long)]
        col: Option<i64>,
        #[arg(long)]
        row: Option<i64>,
        #[arg(long)]
        width: Option<i64>,
        #[arg(long)]
        height: Option<i64>,
        /// Replacement display options, as a JSON object.
        #[arg(long)]
        display_config: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete a widget (its preset / saved query is untouched).
    RemoveWidget {
        /// Widget id.
        widget_id: String,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List { json, org } => {
            let org = crate::active_org::resolve(org)?;
            let body = client.get(&dashboards_path(&org)).await.context("GET analytics/dashboards")?;
            if json {
                print_json(&body);
            } else {
                render_dashboards(&body);
            }
            Ok(())
        }
        Cmd::Create {
            name,
            description,
            confirm,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let name = name.trim().to_string();
            if name.is_empty() {
                bail!("--name must not be empty");
            }
            let existing = client.get(&dashboards_path(&org)).await.context("GET analytics/dashboards")?;
            if let Some(found) = find_by_name(&existing, &name) {
                let id = found.get("id").and_then(Value::as_str).unwrap_or("?");
                println!(
                    "  {} a dashboard named {} already exists ({}) — reusing it",
                    "=".cyan(),
                    name.bold(),
                    id.dimmed()
                );
                return Ok(());
            }
            if !gate(&org, "create", "analytics dashboard", &name, confirm)? {
                return Ok(());
            }
            let body = create_body(&name, description.as_deref());
            let created = client.post(&dashboards_path(&org), Some(&body)).await.context("POST analytics/dashboards")?;
            let id = created.get("id").and_then(Value::as_str).unwrap_or("?");
            println!("  {} created dashboard {} {}", "✚".green(), name.bold(), id.dimmed());
            Ok(())
        }
        Cmd::AddWidget {
            dashboard_id,
            title,
            widget_type,
            preset,
            saved_query,
            col,
            row,
            width,
            height,
            confirm,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            check_widget_type(&widget_type)?;
            let source = pick_source(preset, saved_query)?;
            // Resolve the saved query's STORED sql (never user-typed SQL).
            let source = match source {
                Source::Preset(k) => WidgetSource::Preset(k),
                Source::SavedQuery(id) => {
                    let q = client
                        .get(&format!("/organizations/{org}/analytics/saved-queries/{id}"))
                        .await
                        .context("GET analytics/saved-queries/{id}")?;
                    WidgetSource::Sql(saved_query_sql(&q)?)
                }
            };
            let row = match row {
                Some(r) => r,
                None => {
                    let dash = client
                        .get(&format!("{}/{dashboard_id}", dashboards_path(&org)))
                        .await
                        .context("GET analytics/dashboards/{id}")?;
                    next_free_row(&dash)
                }
            };
            let body = widget_create_body(
                &title,
                &widget_type,
                &source,
                Grid {
                    col,
                    row: Some(row),
                    width,
                    height,
                },
            )?;
            if !gate(&org, "add", "widget to dashboard", &format!("{title} → {dashboard_id}"), confirm)? {
                return Ok(());
            }
            let created = client
                .post(&format!("{}/{dashboard_id}/widgets", dashboards_path(&org)), Some(&body))
                .await
                .context("POST analytics/dashboards/{id}/widgets")?;
            let id = created.get("id").and_then(Value::as_str).unwrap_or("?");
            println!("  {} added {} {} ({widget_type}) on row {row}", "✚".green(), title.bold(), id.dimmed());
            Ok(())
        }
        Cmd::UpdateWidget {
            widget_id,
            title,
            widget_type,
            col,
            row,
            width,
            height,
            display_config,
            confirm,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let display = display_config
                .map(|s| serde_json::from_str::<Value>(&s).context("--display-config must be a JSON object"))
                .transpose()?;
            let body = widget_patch_body(title.as_deref(), widget_type.as_deref(), Grid { col, row, width, height }, display)?;
            if !gate(&org, "update", "widget", &widget_id, confirm)? {
                return Ok(());
            }
            client
                .patch(&format!("/organizations/{org}/analytics/widgets/{widget_id}"), &body)
                .await
                .context("PATCH analytics/widgets/{id}")?;
            println!("  {} updated widget {}", "✓".green(), widget_id.dimmed());
            Ok(())
        }
        Cmd::RemoveWidget { widget_id, confirm, org } => {
            let org = crate::active_org::resolve(org)?;
            if !gate(&org, "remove", "widget", &widget_id, confirm)? {
                return Ok(());
            }
            client
                .delete(&format!("/organizations/{org}/analytics/widgets/{widget_id}"))
                .await
                .context("DELETE analytics/widgets/{id}")?;
            println!("  {} removed widget {}", "🗑".red(), widget_id.dimmed());
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

fn dashboards_path(org: &str) -> String {
    format!("/organizations/{org}/analytics/dashboards")
}

/// Rows of a list response: a bare array, or `{data: [...]}` / `{dashboards: [...]}`.
fn rows(body: &Value) -> Vec<&Value> {
    body.as_array()
        .or_else(|| body.get("data").and_then(Value::as_array))
        .or_else(|| body.get("dashboards").and_then(Value::as_array))
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

/// The existing dashboard whose name matches `name` (trimmed, case-insensitive).
fn find_by_name<'a>(body: &'a Value, name: &str) -> Option<&'a Value> {
    let want = name.trim().to_lowercase();
    rows(body)
        .into_iter()
        .find(|d| d.get("name").and_then(Value::as_str).is_some_and(|n| n.trim().to_lowercase() == want))
}

fn create_body(name: &str, description: Option<&str>) -> Value {
    let mut body = json!({ "name": name });
    if let Some(d) = description.map(str::trim).filter(|d| !d.is_empty()) {
        body["description"] = json!(d);
    }
    body
}

fn check_widget_type(t: &str) -> Result<()> {
    if WIDGET_TYPES.contains(&t) {
        Ok(())
    } else {
        bail!("unknown --type \"{t}\" — use one of: {}", WIDGET_TYPES.join(", "))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Source {
    Preset(String),
    SavedQuery(String),
}

/// A resolved widget data source. `Sql` is ONLY ever a saved query's stored sql.
#[derive(Debug, PartialEq, Eq)]
enum WidgetSource {
    Preset(String),
    Sql(String),
}

/// Exactly one of `--preset` / `--saved-query`.
fn pick_source(preset: Option<String>, saved_query: Option<String>) -> Result<Source> {
    let preset = preset.filter(|s| !s.trim().is_empty());
    let saved = saved_query.filter(|s| !s.trim().is_empty());
    match (preset, saved) {
        (Some(p), None) => Ok(Source::Preset(p)),
        (None, Some(q)) => Ok(Source::SavedQuery(q)),
        (Some(_), Some(_)) => bail!("pass --preset OR --saved-query, not both"),
        (None, None) => bail!("a data source is required: --preset <key> (from `th smoo analytics catalog`) or --saved-query <id>"),
    }
}

/// The stored sql of a saved-query row (`query`).
fn saved_query_sql(row: &Value) -> Result<String> {
    row.get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .context("that saved query has no stored SQL")
}

/// First row below every existing widget: `max(gridRow + gridHeight)`, or 0.
fn next_free_row(dashboard: &Value) -> i64 {
    dashboard
        .get("widgets")
        .and_then(Value::as_array)
        .map(|ws| {
            ws.iter()
                .map(|w| w.get("gridRow").and_then(Value::as_i64).unwrap_or(0) + w.get("gridHeight").and_then(Value::as_i64).unwrap_or(DEFAULT_HEIGHT))
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, Default)]
struct Grid {
    col: Option<i64>,
    row: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
}

fn check_grid(g: Grid) -> Result<()> {
    if let Some(c) = g.col {
        if !(0..=11).contains(&c) {
            bail!("--col must be 0–11 (12-column grid)");
        }
    }
    if let Some(r) = g.row {
        if r < 0 {
            bail!("--row must be ≥ 0");
        }
    }
    if let Some(w) = g.width {
        if !(1..=12).contains(&w) {
            bail!("--width must be 1–12");
        }
    }
    if let Some(h) = g.height {
        if h < 1 {
            bail!("--height must be ≥ 1");
        }
    }
    Ok(())
}

fn widget_create_body(title: &str, widget_type: &str, source: &WidgetSource, grid: Grid) -> Result<Value> {
    if title.trim().is_empty() {
        bail!("--title must not be empty");
    }
    check_widget_type(widget_type)?;
    check_grid(grid)?;
    let mut body = json!({
        "title": title.trim(),
        "widgetType": widget_type,
        "gridCol": grid.col.unwrap_or(0),
        "gridRow": grid.row.unwrap_or(0),
        "gridWidth": grid.width.unwrap_or(DEFAULT_WIDTH),
        "gridHeight": grid.height.unwrap_or(DEFAULT_HEIGHT),
    });
    match source {
        WidgetSource::Preset(k) => body["presetQueryKey"] = json!(k),
        WidgetSource::Sql(sql) => body["customQuery"] = json!(sql),
    }
    Ok(body)
}

/// The PATCH body: only the fields given. Never carries a data source.
fn widget_patch_body(title: Option<&str>, widget_type: Option<&str>, grid: Grid, display: Option<Value>) -> Result<Value> {
    check_grid(grid)?;
    let mut m = Map::new();
    if let Some(t) = title.map(str::trim) {
        if t.is_empty() {
            bail!("--title must not be empty");
        }
        m.insert("title".into(), json!(t));
    }
    if let Some(t) = widget_type {
        check_widget_type(t)?;
        m.insert("widgetType".into(), json!(t));
    }
    for (key, v) in [
        ("gridCol", grid.col),
        ("gridRow", grid.row),
        ("gridWidth", grid.width),
        ("gridHeight", grid.height),
    ] {
        if let Some(v) = v {
            m.insert(key.into(), json!(v));
        }
    }
    if let Some(d) = display {
        if !d.is_object() {
            bail!("--display-config must be a JSON object");
        }
        m.insert("displayConfig".into(), d);
    }
    if m.is_empty() {
        bail!("nothing to update — pass at least one of --title, --type, --col, --row, --width, --height, --display-config");
    }
    Ok(Value::Object(m))
}

fn render_dashboards(body: &Value) {
    let ds = rows(body);
    println!();
    println!("  {} {}", "Dashboards".bold(), format!("({})", ds.len()).dimmed());
    if ds.is_empty() {
        println!("\n  {}\n", "none yet — `th smoo dashboards create --name …`".dimmed());
        return;
    }
    println!();
    for d in ds {
        let name = d.get("name").and_then(Value::as_str).unwrap_or("—");
        let id = d.get("id").and_then(Value::as_str).unwrap_or("");
        let default = if d.get("isDefault").and_then(Value::as_bool) == Some(true) {
            " [default]"
        } else {
            ""
        };
        println!("  {}{}  {}", name.bold(), default.cyan(), id.dimmed());
        if let Some(desc) = d.get("description").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            println!("    {}", desc.dimmed());
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_source_is_required() {
        assert_eq!(pick_source(Some("k".into()), None).unwrap(), Source::Preset("k".into()));
        assert_eq!(pick_source(None, Some("q".into())).unwrap(), Source::SavedQuery("q".into()));
        assert!(pick_source(Some("k".into()), Some("q".into())).is_err());
        assert!(pick_source(None, None).is_err());
        // Blank strings count as absent.
        assert!(pick_source(Some("  ".into()), None).is_err());
    }

    #[test]
    fn create_body_is_preset_or_stored_sql_with_grid_defaults() {
        let b = widget_create_body("Revenue", "kpi_card", &WidgetSource::Preset("mrr".into()), Grid::default()).unwrap();
        assert_eq!(
            b,
            json!({ "title": "Revenue", "widgetType": "kpi_card", "gridCol": 0, "gridRow": 0, "gridWidth": 6, "gridHeight": 4, "presetQueryKey": "mrr" })
        );
        assert!(b.get("customQuery").is_none());

        let b = widget_create_body(
            "Deals",
            "table",
            &WidgetSource::Sql("SELECT 1".into()),
            Grid {
                col: Some(6),
                row: Some(8),
                width: Some(6),
                height: Some(3),
            },
        )
        .unwrap();
        assert_eq!(b["customQuery"], "SELECT 1");
        assert_eq!(b["gridRow"], 8);
        assert!(b.get("presetQueryKey").is_none());
    }

    #[test]
    fn create_body_rejects_bad_type_grid_and_title() {
        let p = WidgetSource::Preset("k".into());
        assert!(widget_create_body("t", "sparkline", &p, Grid::default()).is_err());
        assert!(widget_create_body(" ", "table", &p, Grid::default()).is_err());
        let bad_col = Grid {
            col: Some(12),
            ..Grid::default()
        };
        assert!(widget_create_body("t", "table", &p, bad_col).is_err());
        let bad_w = Grid {
            width: Some(13),
            ..Grid::default()
        };
        assert!(widget_create_body("t", "table", &p, bad_w).is_err());
    }

    #[test]
    fn saved_query_sql_reads_the_stored_query() {
        assert_eq!(saved_query_sql(&json!({ "id": "q", "query": " SELECT 1 " })).unwrap(), "SELECT 1");
        assert!(saved_query_sql(&json!({ "id": "q", "query": "" })).is_err());
        assert!(saved_query_sql(&json!({ "id": "q" })).is_err());
    }

    #[test]
    fn next_free_row_is_below_every_widget() {
        assert_eq!(next_free_row(&json!({ "widgets": [] })), 0);
        assert_eq!(next_free_row(&json!({})), 0);
        let d = json!({ "widgets": [
            { "gridRow": 0, "gridHeight": 4 },
            { "gridRow": 4, "gridHeight": 2 },
            { "gridRow": 1, "gridHeight": 3 },
        ]});
        assert_eq!(next_free_row(&d), 6);
    }

    #[test]
    fn patch_body_carries_only_given_fields_and_never_a_source() {
        let b = widget_patch_body(
            Some("New"),
            None,
            Grid {
                width: Some(12),
                ..Grid::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(b, json!({ "title": "New", "gridWidth": 12 }));
        let b = widget_patch_body(None, Some("bar_chart"), Grid::default(), Some(json!({ "color": "teal" }))).unwrap();
        assert_eq!(b, json!({ "widgetType": "bar_chart", "displayConfig": { "color": "teal" } }));
        for key in ["presetQueryKey", "customQuery"] {
            assert!(b.get(key).is_none());
        }
    }

    #[test]
    fn patch_body_refuses_nothing_and_junk() {
        assert!(widget_patch_body(None, None, Grid::default(), None).is_err());
        assert!(widget_patch_body(None, Some("nope"), Grid::default(), None).is_err());
        assert!(widget_patch_body(None, None, Grid::default(), Some(json!([1]))).is_err());
        assert!(widget_patch_body(
            None,
            None,
            Grid {
                row: Some(-1),
                ..Grid::default()
            },
            None
        )
        .is_err());
    }

    #[test]
    fn create_is_idempotent_on_name() {
        let list = json!([{ "id": "d1", "name": "Sales Pipeline" }, { "id": "d2", "name": "Support" }]);
        assert_eq!(find_by_name(&list, "  sales pipeline ").and_then(|d| d["id"].as_str()), Some("d1"));
        assert!(find_by_name(&list, "Marketing").is_none());
        // Envelope shapes are tolerated too.
        assert!(find_by_name(&json!({ "data": [{ "id": "x", "name": "A" }] }), "a").is_some());
        assert_eq!(create_body("Ops", Some("  ")), json!({ "name": "Ops" }));
        assert_eq!(create_body("Ops", Some("Daily")), json!({ "name": "Ops", "description": "Daily" }));
    }

    #[test]
    fn render_does_not_panic() {
        render_dashboards(&json!([]));
        render_dashboards(&json!([{ "id": "d", "name": "A", "isDefault": true, "description": "x" }]));
    }
}

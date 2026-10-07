//! `th api dashboard …` — the user's main-dashboard widget layout.
//!
//! Layouts are per (user, org, dashboard type) and served by api-prime:
//!
//! | verb   | route                                        |
//! |--------|----------------------------------------------|
//! | GET    | `/organizations/{org}/dashboard/layout?type=` |
//! | PUT    | `/organizations/{org}/dashboard/layout`       |
//!
//! Rows are keyed by user id, so this rides the user JWT ([`UserClient`]),
//! not M2M. `layout add` / `layout remove` are read-modify-write over the
//! same GET/PUT the web dashboard uses — the server validates widget ids
//! against its registry, so an unknown id fails loudly rather than saving
//! a dead tile. SMOODEV-2753 (dogfood: `th api dashboard layout add
//! aws_cost_forecast`). `layout move` and `layout add --at top` place a widget
//! at the top or bottom of the grid (SMOODEV-3697).

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};

use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// Read or edit the widget layout.
    #[command(subcommand)]
    Layout(LayoutCmd),
}

#[derive(Subcommand)]
pub enum LayoutCmd {
    /// Print the saved layout (or the org's default when none is saved).
    Get {
        /// Dashboard type (defaults to `main`).
        #[arg(long = "type", default_value = "main")]
        dashboard_type: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Add a widget to the layout (below the current grid, or `--at top`).
    Add {
        /// Widget id from the registry, e.g. `aws_cost_forecast` (see `th widgets list`).
        widget_id: String,
        /// Apple-style size: small | medium | large | full.
        #[arg(long, default_value = "medium")]
        size: String,
        /// Where to put it: `bottom` (below the current grid) or `top` (first
        /// row; every other widget shifts down).
        #[arg(long, default_value = "bottom")]
        at: Placement,
        /// Dashboard type (defaults to `main`).
        #[arg(long = "type", default_value = "main")]
        dashboard_type: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Move a widget that is already on the layout to the top or bottom of the grid.
    Move {
        /// Widget id to move.
        widget_id: String,
        /// `top` (first row; every other widget shifts down) or `bottom`.
        #[arg(long, default_value = "top")]
        to: Placement,
        /// Dashboard type (defaults to `main`).
        #[arg(long = "type", default_value = "main")]
        dashboard_type: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove a widget from the layout. Prints the target (org + host) and
    /// confirms before acting; refuses when not attached to a terminal.
    Remove {
        /// Widget id to remove.
        widget_id: String,
        /// Dashboard type (defaults to `main`).
        #[arg(long = "type", default_value = "main")]
        dashboard_type: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: crate::destructive::Confirm,
    },
}

/// Where a widget lands in the 12-column grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Placement {
    Top,
    Bottom,
}

fn num(w: &Value, key: &str) -> i64 {
    w.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn id_of(w: &Value) -> Option<&str> {
    w.get("widgetId").and_then(Value::as_str)
}

/// First row below every widget except `skip`.
fn bottom_y(widgets: &[Value], skip: Option<&str>) -> i64 {
    widgets
        .iter()
        .filter(|w| skip.is_none() || id_of(w) != skip)
        .map(|w| num(w, "y") + num(w, "h"))
        .max()
        .unwrap_or(0)
}

/// Put `widget_id` (already in `widgets`) at `placement`.
///
/// The web dashboard is a sortable FLOW grid (`dashboard-grid.tsx`, dnd-kit): it
/// renders widgets in ARRAY order and recomputes x/y itself (`reflowWidgets`), so
/// the array position is what actually moves a widget; x/y are kept consistent for
/// any reader that does use them. Top: first in the array, x 0, y 0, every other
/// widget down by its height. Bottom: last, below everything else. Errors when the
/// widget is absent.
fn place(widgets: &mut Vec<Value>, widget_id: &str, placement: Placement) -> Result<()> {
    let idx = widgets
        .iter()
        .position(|w| id_of(w) == Some(widget_id))
        .ok_or_else(|| anyhow::anyhow!("widget `{widget_id}` is not on the dashboard"))?;
    match placement {
        Placement::Top => {
            let h = num(&widgets[idx], "h").max(1);
            for (i, w) in widgets.iter_mut().enumerate() {
                if i != idx {
                    let y = num(w, "y");
                    w["y"] = json!(y + h);
                }
            }
            widgets[idx]["x"] = json!(0);
            widgets[idx]["y"] = json!(0);
            let w = widgets.remove(idx);
            widgets.insert(0, w);
        }
        Placement::Bottom => {
            let y = bottom_y(widgets, Some(widget_id));
            widgets[idx]["x"] = json!(0);
            widgets[idx]["y"] = json!(y);
            let w = widgets.remove(idx);
            widgets.push(w);
        }
    }
    Ok(())
}

fn resolve_org(override_org: Option<String>) -> Result<String> {
    if let Some(o) = override_org.filter(|s| !s.trim().is_empty()) {
        return Ok(o);
    }
    if let Ok(o) = std::env::var("SMOOAI_ORG_ID") {
        if !o.trim().is_empty() {
            return Ok(o);
        }
    }
    anyhow::bail!("no org specified — pass `--org <id>` or set SMOOAI_ORG_ID")
}

/// Grid width per size, mirroring the web dashboard's size→span mapping
/// (12-column grid: small 3, medium 6, large 9, full 12).
fn width_for(size: &str) -> Result<i64> {
    match size {
        "small" => Ok(3),
        "medium" => Ok(6),
        "large" => Ok(9),
        "full" => Ok(12),
        other => anyhow::bail!("unknown size `{other}` — use small | medium | large | full"),
    }
}

async fn fetch_layout(client: &UserClient, org: &str, dashboard_type: &str) -> Result<Value> {
    client
        .get(&format!("/organizations/{org}/dashboard/layout?type={dashboard_type}"))
        .await
        .context("GET dashboard layout")
}

fn widgets_of(layout: &Value) -> Vec<Value> {
    layout.get("widgets").and_then(Value::as_array).cloned().unwrap_or_default()
}

async fn save_layout(client: &UserClient, org: &str, dashboard_type: &str, widgets: &[Value]) -> Result<Value> {
    client
        .put(
            &format!("/organizations/{org}/dashboard/layout"),
            &json!({ "widgets": widgets, "dashboardType": dashboard_type }),
        )
        .await
        .context("PUT dashboard layout")
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let Cmd::Layout(cmd) = cmd;
    let client = UserClient::from_user_session().await?;
    match cmd {
        LayoutCmd::Get { dashboard_type, org } => {
            let o = resolve_org(org)?;
            let layout = fetch_layout(&client, &o, &dashboard_type).await?;
            println!("{}", serde_json::to_string_pretty(&layout)?);
        }
        LayoutCmd::Add {
            widget_id,
            size,
            at,
            dashboard_type,
            org,
        } => {
            let o = resolve_org(org)?;
            let w = width_for(&size)?;
            let layout = fetch_layout(&client, &o, &dashboard_type).await?;
            let mut widgets = widgets_of(&layout);
            if widgets.iter().any(|x| x.get("widgetId").and_then(Value::as_str) == Some(widget_id.as_str())) {
                anyhow::bail!("widget `{widget_id}` is already on the {dashboard_type} dashboard");
            }
            // Stack below the current grid — the same placement the web
            // dashboard uses for newly merged default widgets — then lift it to
            // the top when asked.
            let y = bottom_y(&widgets, None);
            widgets.push(json!({ "widgetId": widget_id, "x": 0, "y": y, "w": w, "h": 4, "size": size }));
            place(&mut widgets, &widget_id, at)?;
            save_layout(&client, &o, &dashboard_type, &widgets).await?;
            let at = if at == Placement::Top { "top" } else { "bottom" };
            println!(
                "✓ added `{widget_id}` ({size}) at the {at} of the {dashboard_type} dashboard ({} widgets)",
                widgets.len()
            );
        }
        LayoutCmd::Move {
            widget_id,
            to,
            dashboard_type,
            org,
        } => {
            let o = resolve_org(org)?;
            let layout = fetch_layout(&client, &o, &dashboard_type).await?;
            let mut widgets = widgets_of(&layout);
            place(&mut widgets, &widget_id, to)?;
            save_layout(&client, &o, &dashboard_type, &widgets).await?;
            let to = if to == Placement::Top { "top" } else { "bottom" };
            println!("✓ moved `{widget_id}` to the {to} of the {dashboard_type} dashboard");
        }
        LayoutCmd::Remove {
            widget_id,
            dashboard_type,
            org,
            confirm,
        } => {
            let o = resolve_org(org)?;
            let layout = fetch_layout(&client, &o, &dashboard_type).await?;
            let mut widgets = widgets_of(&layout);
            let before = widgets.len();
            widgets.retain(|x| x.get("widgetId").and_then(Value::as_str) != Some(widget_id.as_str()));
            if widgets.len() == before {
                anyhow::bail!("widget `{widget_id}` is not on the {dashboard_type} dashboard");
            }
            let proceed = crate::destructive::gate_with(
                &crate::destructive::Target {
                    verb: "remove",
                    noun: "dashboard widget",
                    id: &widget_id,
                    org: &o,
                    severity: crate::destructive::Severity::Standard,
                },
                confirm,
            )?;
            if !proceed {
                return Ok(());
            }
            save_layout(&client, &o, &dashboard_type, &widgets).await?;
            println!("✓ removed `{widget_id}` from the {dashboard_type} dashboard ({} widgets)", widgets.len());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_maps_sizes_and_rejects_junk() {
        assert_eq!(width_for("small").unwrap(), 3);
        assert_eq!(width_for("medium").unwrap(), 6);
        assert_eq!(width_for("large").unwrap(), 9);
        assert_eq!(width_for("full").unwrap(), 12);
        assert!(width_for("jumbo").is_err());
    }

    fn grid() -> Vec<Value> {
        vec![
            json!({ "widgetId": "a", "x": 0, "y": 0, "w": 12, "h": 2 }),
            json!({ "widgetId": "b", "x": 0, "y": 2, "w": 8, "h": 3 }),
            json!({ "widgetId": "c", "x": 0, "y": 35, "w": 9, "h": 4 }),
        ]
    }

    #[test]
    fn move_to_top_puts_it_first_and_shifts_the_rest_down_by_its_height() {
        let mut w = grid();
        place(&mut w, "c", Placement::Top).unwrap();
        // The web grid renders in array order, so first in the array is what counts.
        assert_eq!(id_of(&w[0]), Some("c"));
        assert_eq!((num(&w[0], "x"), num(&w[0], "y")), (0, 0));
        assert_eq!((id_of(&w[1]), num(&w[1], "y")), (Some("a"), 4));
        assert_eq!((id_of(&w[2]), num(&w[2], "y")), (Some("b"), 6));
        // Nothing overlaps the moved widget's rows.
        assert!(w.iter().filter(|x| id_of(x) != Some("c")).all(|x| num(x, "y") >= 4));
    }

    #[test]
    fn move_to_bottom_goes_below_everything_else() {
        let mut w = grid();
        place(&mut w, "a", Placement::Bottom).unwrap();
        assert_eq!(id_of(&w[2]), Some("a"));
        assert_eq!(num(&w[2], "y"), 39);
        assert_eq!((id_of(&w[0]), num(&w[0], "y")), (Some("b"), 2), "others stay put");
    }

    #[test]
    fn moving_a_widget_that_is_not_there_fails_loudly() {
        let mut w = grid();
        assert!(place(&mut w, "missing", Placement::Top).is_err());
    }

    #[test]
    fn widgets_of_tolerates_missing_or_malformed() {
        assert!(widgets_of(&json!({})).is_empty());
        assert!(widgets_of(&json!({ "widgets": "nope" })).is_empty());
        assert_eq!(widgets_of(&json!({ "widgets": [{ "widgetId": "a" }] })).len(), 1);
    }
}

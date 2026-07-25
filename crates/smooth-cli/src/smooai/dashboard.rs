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
//! aws_cost_forecast`).

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
    /// Add a widget to the layout (appended below the current grid).
    Add {
        /// Widget id from the registry, e.g. `aws_cost_forecast` (see `th widgets list`).
        widget_id: String,
        /// Apple-style size: small | medium | large | full.
        #[arg(long, default_value = "medium")]
        size: String,
        /// Dashboard type (defaults to `main`).
        #[arg(long = "type", default_value = "main")]
        dashboard_type: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove a widget from the layout.
    Remove {
        /// Widget id to remove.
        widget_id: String,
        /// Dashboard type (defaults to `main`).
        #[arg(long = "type", default_value = "main")]
        dashboard_type: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
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
            // dashboard uses for newly merged default widgets.
            let y = widgets
                .iter()
                .map(|x| x.get("y").and_then(Value::as_i64).unwrap_or(0) + x.get("h").and_then(Value::as_i64).unwrap_or(0))
                .max()
                .unwrap_or(0);
            widgets.push(json!({ "widgetId": widget_id, "x": 0, "y": y, "w": w, "h": 4, "size": size }));
            save_layout(&client, &o, &dashboard_type, &widgets).await?;
            println!("✓ added `{widget_id}` ({size}) to the {dashboard_type} dashboard ({} widgets)", widgets.len());
        }
        LayoutCmd::Remove {
            widget_id,
            dashboard_type,
            org,
        } => {
            let o = resolve_org(org)?;
            let layout = fetch_layout(&client, &o, &dashboard_type).await?;
            let mut widgets = widgets_of(&layout);
            let before = widgets.len();
            widgets.retain(|x| x.get("widgetId").and_then(Value::as_str) != Some(widget_id.as_str()));
            if widgets.len() == before {
                anyhow::bail!("widget `{widget_id}` is not on the {dashboard_type} dashboard");
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

    #[test]
    fn widgets_of_tolerates_missing_or_malformed() {
        assert!(widgets_of(&json!({})).is_empty());
        assert!(widgets_of(&json!({ "widgets": "nope" })).is_empty());
        assert_eq!(widgets_of(&json!({ "widgets": [{ "widgetId": "a" }] })).len(), 1);
    }
}

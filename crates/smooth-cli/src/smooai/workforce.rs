//! `smoo workforce …` — the org's people directory.
//! CLI twin of the hosted MCP `workforce_directory` tool (pearl th-a5d991).

use anstream::println;
use anyhow::{Context, Result};
use clap::{Subcommand, ValueEnum};
use owo_colors::OwoColorize;

use super::{print_json, require_active_org, require_authed};

/// Which slice of the directory to read. Mirrors the MCP tool's `view` arg.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, ValueEnum)]
pub enum View {
    /// Name, email, title, department, manager.
    #[default]
    Employees,
    /// The department tree.
    OrgUnits,
    /// Employees + units + the manager → report edge count.
    OrgChart,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Read this org's people directory: `employees` (default), `org-units`
    /// (the department tree), or `org-chart` (both plus the manager → report
    /// edges). Use for "who works here", "who reports to X".
    Directory {
        /// Which view to read.
        #[arg(value_enum, default_value_t = View::Employees)]
        view: View,
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
        Cmd::Directory { view, json, org } => {
            let o = require_active_org(&client, org)?;
            match view {
                View::Employees => {
                    let resp = client
                        .get(&format!("/organizations/{o}/workforce/employees"))
                        .await
                        .context("GET workforce employees")?;
                    if json {
                        print_json(&resp);
                    } else {
                        print_employees(&resp, "data");
                        println!();
                    }
                }
                View::OrgUnits => {
                    let resp = client
                        .get(&format!("/organizations/{o}/workforce/org-units"))
                        .await
                        .context("GET workforce org-units")?;
                    if json {
                        print_json(&resp);
                    } else {
                        print_org_units(&resp, "data");
                        println!();
                    }
                }
                View::OrgChart => {
                    let resp = client
                        .get(&format!("/organizations/{o}/workforce/org-chart"))
                        .await
                        .context("GET workforce org-chart")?;
                    if json {
                        print_json(&resp);
                    } else {
                        print_employees(&resp, "employees");
                        print_org_units(&resp, "units");
                        // The edges are implied by each employee's managerEmployeeId,
                        // so report the count rather than repeating every pair.
                        let edges = resp.get("edges").and_then(|v| v.as_array()).map_or(0, Vec::len);
                        println!();
                        println!("  {edges} manager → report edge(s).");
                        println!();
                    }
                }
            }
        }
    }
    Ok(())
}

/// Rows under `key`, accepting the enveloped shape or a bare top-level array —
/// the routes answer with either (same contract as the MCP server's `rows()`).
fn rows<'a>(body: &'a serde_json::Value, key: &str) -> Option<&'a Vec<serde_json::Value>> {
    body.as_array().or_else(|| body.get(key).and_then(|v| v.as_array()))
}

fn print_employees(body: &serde_json::Value, key: &str) {
    let Some(rows) = rows(body, key) else {
        return;
    };
    println!();
    if rows.is_empty() {
        println!("  {} {}", "●".dimmed(), "No employees in the directory yet.".dimmed());
        return;
    }
    for e in rows {
        let name = e.get("fullName").and_then(|v| v.as_str()).unwrap_or("(unnamed)");
        let email = e.get("primaryEmail").and_then(|v| v.as_str()).unwrap_or("");
        let title = e.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let dept = e.get("department").and_then(|v| v.as_str()).unwrap_or("");
        let role = [title, dept].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(", ");
        let status = e.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let suffix = if status.is_empty() { String::new() } else { format!(" [{status}]") };
        println!("  {} {} {} {}{}", "○".dimmed(), name.bold(), email.cyan(), role.dimmed(), suffix.dimmed());
    }
}

fn print_org_units(body: &serde_json::Value, key: &str) {
    let Some(rows) = rows(body, key) else {
        return;
    };
    println!();
    if rows.is_empty() {
        println!("  {} {}", "●".dimmed(), "No org units defined yet.".dimmed());
        return;
    }
    for u in rows {
        let name = u.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)");
        let path = u.get("orgUnitPath").and_then(|v| v.as_str()).unwrap_or("");
        println!("  {} {} {}", "▸".cyan(), name.bold(), path.dimmed());
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cmd, View};

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[test]
    fn directory_defaults_to_employees() {
        let w = Wrap::try_parse_from(["t", "directory"]).expect("bare directory must parse");
        assert!(matches!(
            w.cmd,
            Cmd::Directory {
                view: View::Employees,
                json: false,
                org: None
            }
        ));
    }

    #[test]
    fn directory_parses_each_view() {
        for (arg, want) in [("employees", View::Employees), ("org-units", View::OrgUnits), ("org-chart", View::OrgChart)] {
            let w = Wrap::try_parse_from(["t", "directory", arg]).expect("view must parse");
            match w.cmd {
                Cmd::Directory { view, .. } => assert_eq!(view, want),
            }
        }
    }

    #[test]
    fn directory_rejects_unknown_view() {
        assert!(Wrap::try_parse_from(["t", "directory", "robots"]).is_err());
    }

    #[test]
    fn directory_parses_json_and_org() {
        let w = Wrap::try_parse_from(["t", "directory", "org-chart", "--json", "--org-id", "o1"]).expect("flags must parse");
        match w.cmd {
            Cmd::Directory { view, json, org } => {
                assert_eq!(view, View::OrgChart);
                assert!(json);
                assert_eq!(org.as_deref(), Some("o1"));
            }
        }
    }
}

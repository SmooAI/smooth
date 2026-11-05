//! Tab groups across every window — the Claude in Chrome ones included, not just Smooth's.
//! Selection and the safety rules (never a group holding a window's active tab; `--idle` spares
//! groups in recent use) live in the extension's `groups.js`, so the plan is made against the
//! live browser at the moment it acts. This side parses selectors and formats the answer.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};

pub const COLORS: [&str; 9] = ["grey", "blue", "red", "yellow", "green", "pink", "purple", "cyan", "orange"];

#[derive(Subcommand, Debug)]
pub enum GroupCommands {
    /// Close every tab in the selected groups
    Close(Selector),
    /// Remove the grouping and keep the tabs
    Ungroup(Selector),
    /// Retitle a group
    Rename { group: i64, title: String },
    /// Recolor a group
    Color {
        group: i64,
        #[arg(value_parser = COLORS)]
        color: String,
    },
    /// Collapse groups in the tab strip
    Collapse {
        #[arg(required = true)]
        groups: Vec<i64>,
    },
    /// Expand collapsed groups
    Expand {
        #[arg(required = true)]
        groups: Vec<i64>,
    },
}

/// Which groups a close/ungroup touches. Ids and `--title` add candidates; `--all`, or `--idle`
/// alone, means every group; `--idle` then keeps only the ones unused for that long.
#[derive(Args, Debug, Default, Clone)]
pub struct Selector {
    /// Group ids from `th chrome browse groups`
    pub ids: Vec<i64>,
    /// Exact title, or a glob with * and ? (case-insensitive)
    #[arg(long)]
    pub title: Option<String>,
    /// Only groups whose every tab was last used longer ago than this (30m, 2h, 1d, 1h30m)
    #[arg(long)]
    pub idle: Option<String>,
    /// Every group
    #[arg(long)]
    pub all: bool,
    /// Show what would happen and change nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Print the result as JSON
    #[arg(long)]
    pub json: bool,
}

impl Selector {
    /// The `groups.apply` params.
    ///
    /// # Errors
    /// No selector given, or an `--idle` that isn't a duration.
    pub fn params(&self, action: &str, dry_run: bool) -> Result<Value> {
        if self.ids.is_empty() && self.title.is_none() && self.idle.is_none() && !self.all {
            bail!("say which groups: give group ids, --title, --idle or --all (see `th chrome browse groups`)");
        }
        let idle_ms = self.idle.as_deref().map(parse_duration).transpose()?;
        let mut p = json!({"action": action, "ids": self.ids, "all": self.all, "dryRun": dry_run});
        if let Some(t) = &self.title {
            p["title"] = json!(t);
        }
        if let Some(ms) = idle_ms {
            p["idleMs"] = json!(ms);
        }
        Ok(p)
    }
}

/// `30m`, `2h`, `1d`, `1h30m`, `45s`, `1w` → milliseconds.
///
/// # Errors
/// Empty, a bare number, an unknown unit, zero, or overflow.
pub fn parse_duration(s: &str) -> Result<u64> {
    let s = s.trim();
    let bad = || format!("'{s}' isn't a duration: use a number and a unit, like 30m, 2h, 1d or 1h30m");
    if s.is_empty() {
        bail!(bad());
    }
    let mut total: u64 = 0;
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit: u64 = match c.to_ascii_lowercase() {
            's' => 1_000,
            'm' => 60_000,
            'h' => 3_600_000,
            'd' => 86_400_000,
            'w' => 604_800_000,
            _ => bail!(bad()),
        };
        let n: u64 = digits.parse().with_context(bad)?;
        digits.clear();
        total = n.checked_mul(unit).and_then(|v| total.checked_add(v)).with_context(bad)?;
    }
    if !digits.is_empty() {
        bail!(bad());
    }
    if total == 0 {
        bail!("the idle time must be more than zero");
    }
    Ok(total)
}

fn now_ms() -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "ms since the epoch fit an f64 exactly")]
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64)
}

/// `5m ago`, `3h ago`, `2d ago`; `unknown` when Chrome reports no time.
pub fn ago(last: Option<f64>, now: f64) -> String {
    let Some(last) = last else { return "unknown".into() };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to ≥ 0 seconds")]
    let secs = ((now - last) / 1000.0).max(0.0) as u64;
    match secs {
        0..=59 => "just now".into(),
        60..=3_599 => format!("{}m ago", secs / 60),
        3_600..=86_399 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn title(g: &Value) -> &str {
    g["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("(untitled)")
}

fn plural(n: u64, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// The groups table.
pub fn format_list(v: &Value, now: f64) -> String {
    let rows = v.as_array().map(Vec::as_slice).unwrap_or_default();
    if rows.is_empty() {
        return "No tab groups.".into();
    }
    let mut out = format!(
        "{:<11} {:>4}  {:<10} {:<7} {:<10} {:<11} {}\n",
        "ID", "TABS", "LAST USED", "COLOR", "WINDOW", "STATE", "TITLE"
    );
    for g in rows {
        let state = match (g["focused"] == true, g["active"] == true, g["collapsed"] == true) {
            (true, ..) => "in use",
            (false, true, _) => "active tab",
            (false, false, true) => "collapsed",
            _ => "",
        };
        let _ = writeln!(
            out,
            "{:<11} {:>4}  {:<10} {:<7} {:<10} {:<11} {}",
            g["id"],
            g["tabs"],
            ago(g["lastAccessed"].as_f64(), now),
            g["color"].as_str().unwrap_or_default(),
            g["windowId"],
            state,
            title(g)
        );
    }
    let _ = write!(
        out,
        "\n{}. \"in use\" holds the active tab of the focused window; groups holding any window's active tab are never closed or ungrouped.",
        plural(rows.len() as u64, "group")
    );
    out
}

fn why(skip: &Value, now: f64) -> String {
    let g = &skip["group"];
    match skip["reason"].as_str() {
        Some("active") => format!("holds the active tab of window {}", g["windowId"]),
        Some("recent") => format!("used {}, more recently than --idle", ago(g["lastAccessed"].as_f64(), now)),
        Some("unknown") => "Chrome doesn't say when its tabs were last used".into(),
        other => other.unwrap_or("skipped").into(),
    }
}

/// What a close/ungroup did (or, dry run, would do), and what it left alone and why.
pub fn format_apply(v: &Value, now: f64) -> String {
    let close = v["action"] == "close";
    let dry = v["dryRun"] == true;
    let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
    let (selected, skipped, missing, failed) = (list("selected"), list("skipped"), list("missing"), list("failed"));
    let mut out = String::new();
    if selected.is_empty() {
        out.push_str("No groups to change.\n");
    } else {
        let tabs: u64 = selected.iter().filter_map(|g| g["tabs"].as_u64()).sum();
        let verb = match (dry, close) {
            (true, true) => "Would close",
            (true, false) => "Would ungroup",
            (false, true) => "Closed",
            (false, false) => "Ungrouped",
        };
        let _ = writeln!(out, "{verb} {} ({}):", plural(selected.len() as u64, "group"), plural(tabs, "tab"));
        for g in &selected {
            let _ = writeln!(
                out,
                "  {}  {}  {}, last used {}",
                g["id"],
                title(g),
                plural(g["tabs"].as_u64().unwrap_or(0), "tab"),
                ago(g["lastAccessed"].as_f64(), now)
            );
        }
    }
    if !skipped.is_empty() {
        let _ = writeln!(out, "Left alone ({}):", skipped.len());
        for s in &skipped {
            let _ = writeln!(out, "  {}  {}  {}", s["group"]["id"], title(&s["group"]), why(s, now));
        }
    }
    if !missing.is_empty() {
        let ids: Vec<String> = missing.iter().map(ToString::to_string).collect();
        let _ = writeln!(out, "No such group: {}", ids.join(", "));
    }
    if !failed.is_empty() {
        let _ = writeln!(out, "Failed ({}):", failed.len());
        for f in &failed {
            let _ = writeln!(
                out,
                "  {}  {}  {}",
                f["group"]["id"],
                title(&f["group"]),
                f["error"].as_str().unwrap_or_default()
            );
        }
    }
    if dry && !selected.is_empty() {
        out.push_str("Dry run: nothing changed.\n");
    }
    out
}

/// An extension from before tab groups answers "unknown method"; say how to update it.
fn call(inst: Option<&str>, method: &str, params: Value) -> Result<Value> {
    super::browse(inst, method, params).map_err(|e| {
        if e.to_string().contains("unknown method") {
            anyhow::anyhow!("the Smooth extension predates tab groups: run `th chrome install`, then press reload on it in chrome://extensions")
        } else {
            e
        }
    })
}

/// `th chrome browse groups` / browse_groups list.
///
/// # Errors
/// No extension connected, browsing off, or an old extension.
pub fn list(inst: Option<&str>, as_json: bool) -> Result<String> {
    let v = call(inst, "groups.list", Value::Null)?;
    if as_json {
        return Ok(serde_json::to_string_pretty(&v)?);
    }
    Ok(format_list(&v, now_ms()))
}

/// Close or ungroup the selected groups (`action` is `close` or `ungroup`).
///
/// # Errors
/// No selector, a bad `--idle`, or the extension's error.
pub fn apply(inst: Option<&str>, action: &str, sel: &Selector, dry_run: bool, as_json: bool) -> Result<String> {
    let v = call(inst, "groups.apply", sel.params(action, dry_run)?)?;
    if as_json {
        return Ok(serde_json::to_string_pretty(&v)?);
    }
    Ok(format_apply(&v, now_ms()))
}

/// The `groups.update` params for rename/color/collapse/expand.
pub fn update_params(cmd: &GroupCommands) -> Vec<Value> {
    match cmd {
        GroupCommands::Rename { group, title } => vec![json!({"groupId": group, "title": title})],
        GroupCommands::Color { group, color } => vec![json!({"groupId": group, "color": color})],
        GroupCommands::Collapse { groups } => groups.iter().map(|g| json!({"groupId": g, "collapsed": true})).collect(),
        GroupCommands::Expand { groups } => groups.iter().map(|g| json!({"groupId": g, "collapsed": false})).collect(),
        GroupCommands::Close(_) | GroupCommands::Ungroup(_) => Vec::new(),
    }
}

/// `th chrome browse groups [--json] [close|ungroup|rename|color|collapse|expand …]`.
///
/// # Errors
/// Whatever the op reports.
pub fn cli(inst: Option<&str>, cmd: Option<GroupCommands>, as_json: bool) -> Result<String> {
    let Some(cmd) = cmd else { return list(inst, as_json) };
    match &cmd {
        GroupCommands::Close(sel) => apply(inst, "close", sel, sel.dry_run, sel.json || as_json),
        GroupCommands::Ungroup(sel) => apply(inst, "ungroup", sel, sel.dry_run, sel.json || as_json),
        _ => {
            let mut out = String::new();
            for p in update_params(&cmd) {
                let g = call(inst, "groups.update", p)?;
                let _ = writeln!(
                    out,
                    "{}  {}  {}{}",
                    g["id"],
                    title(&g),
                    g["color"].as_str().unwrap_or_default(),
                    if g["collapsed"] == true { ", collapsed" } else { "" }
                );
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser, Debug)]
    struct Cli {
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<GroupCommands>,
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("groups").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn durations_take_units_and_compounds() {
        assert_eq!(parse_duration("45s").unwrap(), 45_000);
        assert_eq!(parse_duration("30m").unwrap(), 1_800_000);
        assert_eq!(parse_duration("2h").unwrap(), 7_200_000);
        assert_eq!(parse_duration("1h30m").unwrap(), 5_400_000);
        assert_eq!(parse_duration(" 1D ").unwrap(), 86_400_000);
        assert_eq!(parse_duration("1w").unwrap(), 604_800_000);
        for bad in ["", "30", "m", "2x", "1h30", "-5m", "1.5h", "99999999999999999999h"] {
            assert!(parse_duration(bad).is_err(), "{bad} should be rejected");
        }
        assert!(parse_duration("0m").unwrap_err().to_string().contains("more than zero"));
    }

    #[test]
    fn a_selector_is_required() {
        let err = Selector::default().params("close", false).unwrap_err().to_string();
        assert!(err.contains("say which groups"), "{err}");
        assert!(Selector {
            dry_run: true,
            json: true,
            ..Selector::default()
        }
        .params("close", true)
        .is_err());
    }

    #[test]
    fn params_carry_every_selector() {
        let sel = Selector {
            ids: vec![3, 9],
            title: Some("Claude*".into()),
            idle: Some("2h".into()),
            ..Selector::default()
        };
        assert_eq!(
            sel.params("close", true).unwrap(),
            json!({"action": "close", "ids": [3, 9], "all": false, "dryRun": true, "title": "Claude*", "idleMs": 7_200_000})
        );
        let all = Selector {
            all: true,
            ..Selector::default()
        };
        assert_eq!(
            all.params("ungroup", false).unwrap(),
            json!({"action": "ungroup", "ids": [], "all": true, "dryRun": false})
        );
        let bad = Selector {
            idle: Some("soon".into()),
            ..Selector::default()
        };
        assert!(bad.params("close", false).unwrap_err().to_string().contains("isn't a duration"));
    }

    #[test]
    fn cli_parses_list_and_actions() {
        let c = parse(&[]);
        assert!(c.cmd.is_none() && !c.json);
        assert!(parse(&["--json"]).json);
        let Some(GroupCommands::Close(sel)) = parse(&["close", "12", "34", "--title", "Claude", "--idle", "2h", "--dry-run"]).cmd else {
            panic!("expected close")
        };
        assert_eq!(sel.ids, vec![12, 34]);
        assert_eq!(sel.title.as_deref(), Some("Claude"));
        assert_eq!(sel.idle.as_deref(), Some("2h"));
        assert!(sel.dry_run && !sel.all);
        let Some(GroupCommands::Ungroup(sel)) = parse(&["ungroup", "--all"]).cmd else {
            panic!("expected ungroup")
        };
        assert!(sel.all && sel.ids.is_empty());
        assert!(Cli::try_parse_from(["groups", "color", "1", "mauve"]).is_err());
        assert!(Cli::try_parse_from(["groups", "collapse"]).is_err());
    }

    #[test]
    fn updates_become_one_call_per_group() {
        assert_eq!(
            update_params(&parse(&["rename", "5", "Work"]).cmd.unwrap()),
            vec![json!({"groupId": 5, "title": "Work"})]
        );
        assert_eq!(
            update_params(&parse(&["color", "5", "cyan"]).cmd.unwrap()),
            vec![json!({"groupId": 5, "color": "cyan"})]
        );
        assert_eq!(
            update_params(&parse(&["collapse", "5", "6"]).cmd.unwrap()),
            vec![json!({"groupId": 5, "collapsed": true}), json!({"groupId": 6, "collapsed": true})]
        );
        assert_eq!(
            update_params(&parse(&["expand", "7"]).cmd.unwrap()),
            vec![json!({"groupId": 7, "collapsed": false})]
        );
        assert!(update_params(&parse(&["close", "--all"]).cmd.unwrap()).is_empty());
    }

    #[test]
    fn ago_reads_naturally() {
        let now = 10_000_000_000.0;
        assert_eq!(ago(None, now), "unknown");
        assert_eq!(ago(Some(now - 5_000.0), now), "just now");
        assert_eq!(ago(Some(now + 5_000.0), now), "just now");
        assert_eq!(ago(Some(now - 300_000.0), now), "5m ago");
        assert_eq!(ago(Some(now - 7_200_000.0), now), "2h ago");
        assert_eq!(ago(Some(now - 259_200_000.0), now), "3d ago");
    }

    const NOW: f64 = 1_000_000_000_000.0;

    fn group(id: i64, title: &str, tabs: u64, mins_ago: f64) -> Value {
        json!({"id": id, "title": title, "color": "orange", "collapsed": false, "windowId": 1, "tabs": tabs, "lastAccessed": mins_ago.mul_add(-60_000.0, NOW), "active": false, "focused": false})
    }

    #[test]
    fn list_is_a_table_with_state() {
        let mut busy = group(2, "Smooth", 1, 0.0);
        busy["active"] = json!(true);
        busy["focused"] = json!(true);
        let mut collapsed = group(3, "", 2, 60.0);
        collapsed["collapsed"] = json!(true);
        collapsed["lastAccessed"] = Value::Null;
        let out = format_list(&json!([group(1, "Claude", 3, 180.0), busy, collapsed]), NOW);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("ID") && lines[0].ends_with("TITLE"));
        assert!(
            lines[1].starts_with("1 ") && lines[1].contains("3h ago") && lines[1].ends_with("Claude"),
            "{}",
            lines[1]
        );
        assert!(lines[2].contains("in use") && lines[2].contains("just now"), "{}", lines[2]);
        assert!(
            lines[3].contains("collapsed") && lines[3].contains("unknown") && lines[3].ends_with("(untitled)"),
            "{}",
            lines[3]
        );
        assert!(out.contains("3 groups."));
        assert_eq!(format_list(&json!([]), NOW), "No tab groups.");
    }

    #[test]
    fn apply_reports_done_skipped_missing_and_failed() {
        let v = json!({
            "action": "close", "dryRun": false,
            "selected": [group(1, "Claude", 3, 180.0), group(4, "Claude", 1, 600.0)],
            "skipped": [
                {"group": group(2, "Claude", 1, 0.0), "reason": "active"},
                {"group": group(5, "Claude", 2, 10.0), "reason": "recent"},
                {"group": group(6, "Claude", 2, 10.0), "reason": "unknown"},
            ],
            "missing": [99],
            "failed": [{"group": group(7, "Claude", 1, 900.0), "error": "No tab with id: 3."}],
        });
        let out = format_apply(&v, NOW);
        assert!(out.starts_with("Closed 2 groups (4 tabs):\n  1  Claude  3 tabs, last used 3h ago\n"), "{out}");
        assert!(out.contains("Left alone (3):\n  2  Claude  holds the active tab of window 1\n"));
        assert!(out.contains("  5  Claude  used 10m ago, more recently than --idle\n"));
        assert!(out.contains("  6  Claude  Chrome doesn't say when its tabs were last used\n"));
        assert!(out.contains("No such group: 99\n"));
        assert!(out.contains("Failed (1):\n  7  Claude  No tab with id: 3.\n"));
        assert!(!out.contains("Dry run"));
    }

    #[test]
    fn dry_runs_say_so() {
        let v = json!({"action": "ungroup", "dryRun": true, "selected": [group(1, "Claude", 1, 180.0)], "skipped": [], "missing": [], "failed": []});
        let out = format_apply(&v, NOW);
        assert!(out.starts_with("Would ungroup 1 group (1 tab):"), "{out}");
        assert!(out.ends_with("Dry run: nothing changed.\n"));
        let none = format_apply(
            &json!({"action": "close", "dryRun": true, "selected": [], "skipped": [], "missing": [], "failed": []}),
            NOW,
        );
        assert_eq!(none, "No groups to change.\n");
    }
}

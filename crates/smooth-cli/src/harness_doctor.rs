//! `th harness doctor [name]` (pearl th-3cabf6): prints what
//! [`smooth_flow::doctor`] finds. The checks themselves live in the flow
//! crate so the daemon can run them too, and every SmoothFlow picker can mark
//! a degraded harness (th-51bf88).

use std::path::Path;

use anstream::println;
use anyhow::Result;
use owo_colors::OwoColorize;
use smooth_flow::harness::Registry;

pub use smooth_flow::doctor::*;

fn print_human(rows: &[Diagnosis], verbose: bool) {
    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(8).max(8);
    for r in rows {
        let head = format!("{} {:<width$}  {:<13}", r.verdict.glyph(), r.name, r.verdict.label());
        let tail = [r.version.clone(), r.binary.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        if r.verdict == Verdict::NotInstalled {
            println!("{}", head.dimmed());
        } else {
            println!("{}  {}", head.bold(), tail.dimmed());
        }
        for c in &r.checks {
            let show = verbose || matches!(c.level, Level::Fail | Level::Warn) || c.id == "cmux_shim";
            if !show {
                continue;
            }
            let glyph = match c.level {
                Level::Ok => "●",
                Level::Info => "·",
                Level::Warn => "◐",
                Level::Fail => "○",
            };
            println!("    {glyph} {}: {}", c.id, c.detail);
            if let Some(fix) = &c.fix {
                if c.level != Level::Ok {
                    println!("      fix: {}", fix.cyan());
                }
            }
        }
    }
    let count = |v: Verdict| rows.iter().filter(|r| r.verdict == v).count();
    println!(
        "\n{} works · {} degraded · {} not installed  {}",
        count(Verdict::Works),
        count(Verdict::Degraded),
        count(Verdict::NotInstalled),
        "(read-only: doctor changed nothing)".dimmed()
    );
}

/// `th harness doctor [name] [--json] [--verbose]`.
///
/// # Errors
/// When `only` names no manifest.
pub fn run(home: &Path, only: Option<&str>, json: bool, verbose: bool) -> Result<()> {
    let project = std::env::current_dir().ok().map(|d| smooth_flow::engine::project_root(&d));
    let registry = Registry::load(home, project.as_deref());
    let machine = Machine::current(home.to_path_buf());
    let rows = diagnose_all(&registry, &machine, only)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "harnesses": rows, "app_path": machine.app_path.map(|p| p.to_string_lossy().into_owned()) }))?
        );
    } else {
        print_human(&rows, verbose);
    }
    Ok(())
}

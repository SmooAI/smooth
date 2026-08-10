//! `th doctor --setup-statusline` — wire the smooth statusline into Claude Code.
//!
//! Pearl th-2f33b6. The statusline (`claude-plugins/smooth-agent/hooks/
//! smooth-statusline.sh`) shows which th-mail agent a session is and how much
//! mail is waiting for it — the handle otherwise scrolls off the top of the
//! transcript within a minute and agents start guessing it.
//!
//! Claude Code allows exactly ONE `statusLine` command, so the only interesting
//! case is the one where the user already has one. We **never** overwrite it:
//! ponytail's installer has the same "only if absent" rule, and two installers
//! that both silently claim the key means whoever ran second wins and the other
//! just vanishes. When the slot is taken we print the wrapper that renders both
//! and let the user decide.

use std::path::{Path, PathBuf};

use anstream::println;
use anyhow::{Context, Result};
use owo_colors::OwoColorize;

/// Where Claude Code keeps user settings.
fn settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

/// Our statusline scripts, wherever the plugin is installed. `None` when the
/// plugin isn't on this machine.
fn plugin_hooks_dir(home: &Path) -> Option<PathBuf> {
    // Newest cached version of the plugin wins; the cache keeps every version it
    // has ever seen, and directory order is by name, not recency.
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let cache = home.join(".claude").join("plugins").join("cache").join("smooth").join("smooth-agent");
    for entry in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
        let hooks = entry.path().join("hooks");
        if !hooks.join("smooth-statusline.sh").is_file() {
            continue;
        }
        let when = entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(b, _)| when > *b) {
            best = Some((when, hooks));
        }
    }
    best.map(|(_, p)| p)
}

/// What `--setup-statusline` decided to do. Split out from the printing so the
/// decision is testable without a `$HOME` or a terminal.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// No `statusLine` set — write ours.
    Install,
    /// `statusLine` already points at one of our scripts.
    AlreadyOurs,
    /// Something else owns the slot; the string is the command it runs.
    Occupied(String),
}

/// Decide what to do, given the current settings document.
fn plan(settings: &serde_json::Value) -> Plan {
    let current = settings.get("statusLine").and_then(|s| s.get("command")).and_then(|c| c.as_str());
    match current {
        None => Plan::Install,
        Some(c) if c.contains("smooth-statusline") => Plan::AlreadyOurs,
        Some(c) => Plan::Occupied(c.to_string()),
    }
}

/// Read `~/.claude/settings.json`, tolerating absent/empty but never malformed —
/// clobbering settings we failed to parse would eat the user's whole config.
fn load_settings(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&raw).with_context(|| format!("parse {} as JSON — fix it, then re-run", path.display()))
}

/// Write the `statusLine` entry pointing at `command`, preserving every other
/// key (and, thanks to serde_json's `preserve_order`, their order).
fn write_statusline(path: &Path, mut settings: serde_json::Value, command: &str) -> Result<()> {
    let root = settings
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} is not a JSON object", path.display()))?;
    root.insert(
        "statusLine".to_string(),
        serde_json::json!({ "type": "command", "command": command, "padding": 0 }),
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(&settings)?)).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Run the setup.
///
/// # Errors
/// Returns an error if the home directory can't be resolved, the plugin isn't
/// installed, or settings can't be read/parsed/written.
pub fn run() -> Result<()> {
    let home = crate::mcp_install::harness_home()?;
    let hooks = plugin_hooks_dir(&home)
        .context("the smooth-agent plugin isn't installed — add the `smooth` marketplace and enable `smooth-agent@smooth` in .claude/settings.json first")?;
    let ours = hooks.join("smooth-statusline.sh").display().to_string();
    let wrapper = hooks.join("smooth-statusline-with-ponytail.sh").display().to_string();
    let path = settings_path(&home);
    let settings = load_settings(&path)?;

    println!();
    match plan(&settings) {
        Plan::AlreadyOurs => {
            println!("  {} statusline already set up → {}", "·".dimmed(), path.display().to_string().dimmed());
        }
        Plan::Install => {
            write_statusline(&path, settings, &ours)?;
            println!("  {} statusline installed → {}", "✓".green().bold(), path.display().to_string().dimmed());
            println!("    {}", "shows this session's th-mail handle and unread count".dimmed());
            println!("\n  {} start a new Claude Code session to see it.", "→".dimmed());
        }
        Plan::Occupied(existing) => {
            println!("  {} you already have a statusline — leaving it alone:", "!".yellow().bold());
            println!("    {}", existing.dimmed());
            let both = if existing.contains("ponytail") {
                "Both fit on one line — that is exactly what the wrapper is for."
            } else {
                "The wrapper runs ponytail's statusline then ours; point it at yours only if yours reads stdin and prints one line."
            };
            println!("\n  {both}\n  To run both, set {} in {} to:\n", "statusLine.command".bold(), path.display());
            println!("    {wrapper}\n");
            println!("  {} to take the slot outright, clear `statusLine` and re-run this.", "→".dimmed());
        }
    }
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn plan_reads_the_current_owner() {
        assert_eq!(plan(&serde_json::json!({})), Plan::Install);
        assert_eq!(plan(&serde_json::json!({ "model": "opus" })), Plan::Install);
        assert_eq!(
            plan(&serde_json::json!({ "statusLine": { "type": "command", "command": "/x/smooth-statusline.sh" } })),
            Plan::AlreadyOurs
        );
        // The wrapper counts as ours too — re-running must not fight itself.
        assert_eq!(
            plan(&serde_json::json!({ "statusLine": { "command": "/x/smooth-statusline-with-ponytail.sh" } })),
            Plan::AlreadyOurs
        );
        assert_eq!(
            plan(&serde_json::json!({ "statusLine": { "command": "/p/ponytail-statusline.sh" } })),
            Plan::Occupied("/p/ponytail-statusline.sh".to_string())
        );
    }

    #[test]
    fn writing_preserves_every_other_setting() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, r#"{"model":"opus","enabledPlugins":{"smooth-agent@smooth":true}}"#).unwrap();

        let settings = load_settings(&path).unwrap();
        write_statusline(&path, settings, "/x/smooth-statusline.sh").unwrap();

        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["statusLine"]["type"], "command");
        assert_eq!(doc["statusLine"]["command"], "/x/smooth-statusline.sh");
        assert_eq!(doc["model"], "opus");
        assert_eq!(doc["enabledPlugins"]["smooth-agent@smooth"], true);
        // Second pass is a no-op decision, not another write.
        assert_eq!(plan(&doc), Plan::AlreadyOurs);
    }

    #[test]
    fn a_missing_or_empty_settings_file_is_empty_not_an_error() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope.json");
        assert_eq!(load_settings(&missing).unwrap(), serde_json::json!({}));
        let blank = tmp.path().join("blank.json");
        std::fs::write(&blank, "\n  \n").unwrap();
        assert_eq!(load_settings(&blank).unwrap(), serde_json::json!({}));
    }

    #[test]
    fn malformed_settings_error_instead_of_being_replaced() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_settings(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json", "must not be touched");
    }

    #[test]
    fn plugin_hooks_dir_picks_the_newest_install() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join(".claude/plugins/cache/smooth/smooth-agent");
        assert!(plugin_hooks_dir(tmp.path()).is_none(), "no plugin, no dir");

        // Create the name-wise LATER version first, so the newest on disk is the
        // one a plain name-ordered glob would skip.
        for v in ["0.31.0", "0.9.0"] {
            let hooks = base.join(v).join("hooks");
            std::fs::create_dir_all(&hooks).unwrap();
            std::fs::write(hooks.join("smooth-statusline.sh"), "#!/bin/bash\n").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let picked = plugin_hooks_dir(tmp.path()).expect("a version is installed");
        assert_eq!(picked, base.join("0.9.0").join("hooks"), "newest install must win, not the highest name");

        // A version directory without the script is not a candidate.
        std::fs::create_dir_all(base.join("1.0.0")).unwrap();
        assert!(plugin_hooks_dir(tmp.path()).unwrap().join("smooth-statusline.sh").is_file());
    }
}

//! `th harness` — one idempotent setup/update/status command per coding
//! harness (Claude Code, Codex, OpenCode). Pearl th-19dac1 / EPIC th-1945b9.
//!
//! `enable` is also the update command: re-run it after upgrading `th` or the
//! smooth-agent plugin, the way `tsx agents enable <provider>` works in the
//! TSX toolbox this is modeled on. Every step is idempotent and preserving:
//! user-owned config survives untouched, and `disable` only ever removes what
//! smooth wrote — the MCP entry, and symlinks that resolve into smooth-owned
//! sources. There is no ownership manifest; ownership IS "the symlink points
//! into the smooth-agent plugin checkout".
//!
//! What each harness gets:
//!
//! | Harness | MCP (`th mcp serve`) | Skills | Extras |
//! |---|---|---|---|
//! | claude-code | `~/.claude.json` | via the smooth-agent marketplace plugin | plugin install/update via the `claude` CLI; statusline check |
//! | codex | `~/.codex/config.toml` | via the smooth-agent plugin (Codex copies real dirs fine) | plugin state detection + instructions |
//! | opencode | `~/.config/opencode/opencode.json` | symlinks into `~/.opencode/skills/` | lifecycle plugin is pearl th-cc50cd |

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;

use crate::mcp_install::{self, Harness, Outcome};

#[derive(Subcommand)]
pub enum Cmd {
    /// Set up (or update) a harness: register the `th mcp serve` MCP server,
    /// install/update the smooth-agent plugin where the harness has a plugin
    /// system, and link the shared skills where it doesn't.
    ///
    /// Idempotent — re-run after upgrading `th` or the plugin. This is the
    /// install AND the update command.
    Enable {
        /// claude-code | codex | opencode | all
        provider: String,
    },
    /// Show, per harness: installed?, MCP entry state, plugin/skills state,
    /// and (Claude Code) whether a statusline is wired.
    Status,
    /// Remove what smooth wrote for a harness: the MCP entry and any skill
    /// symlinks that resolve into smooth-owned sources. Never touches
    /// user-owned config; plugin uninstall stays with the harness's own CLI.
    Disable {
        /// claude-code | codex | opencode | all
        provider: String,
    },
}

/// # Errors
/// Returns an error when the provider name is unknown or a config file is
/// malformed (never silently clobbered).
pub fn cmd(cmd: Cmd) -> Result<()> {
    let home = mcp_install::harness_home()?;
    match cmd {
        Cmd::Enable { provider } => {
            for h in providers(&provider)? {
                enable(h, &home);
            }
            Ok(())
        }
        Cmd::Status => {
            for h in Harness::ALL {
                status(h, &home);
            }
            Ok(())
        }
        Cmd::Disable { provider } => {
            for h in providers(&provider)? {
                disable(h, &home)?;
            }
            Ok(())
        }
    }
}

fn providers(spec: &str) -> Result<Vec<Harness>> {
    if spec.trim().eq_ignore_ascii_case("all") {
        Ok(Harness::ALL.to_vec())
    } else {
        Ok(vec![Harness::parse(spec)?])
    }
}

fn enable(h: Harness, home: &Path) {
    println!("{}", format!("== {h}").bold().bright_cyan());
    if !h.marker_dir(home).is_dir() {
        println!("   not installed on this machine (no {}) — skipped", h.marker_dir(home).display());
        return;
    }

    // 1. MCP server — the shared mailbox/pearl surface, all harnesses.
    match mcp_install::install_into(h, home, false) {
        Ok(o) => println!("   mcp: {}", describe_mcp(&o)),
        Err(e) => println!("   mcp: {} {e:#}", "FAILED".bright_red()),
    }

    // 2. Per-harness extras.
    match h {
        Harness::ClaudeCode => {
            claude_plugin_step(home);
            statusline_step(home);
        }
        Harness::Codex => codex_plugin_step(home),
        Harness::OpenCode => match link_skills(home) {
            Ok((added, kept)) => println!("   skills: {added} linked, {kept} already current → {}", opencode_skills_dir(home).display()),
            Err(e) => println!("   skills: {} {e:#}", "FAILED".bright_red()),
        },
    }
}

fn describe_mcp(o: &Outcome) -> String {
    match o {
        Outcome::Added => "registered `th mcp serve`".to_string(),
        Outcome::AlreadyPresent => "already registered".to_string(),
        Outcome::Updated => "repointed a stale entry at `th mcp serve`".to_string(),
        Outcome::NotInstalled => "harness not installed".to_string(),
    }
}

/// Install or update the smooth-agent plugin through the `claude` CLI.
/// Network + external binary — failures are reported, never fatal, because
/// the rest of enable (MCP, statusline) is still worth doing.
fn claude_plugin_step(home: &Path) {
    if which("claude").is_none() {
        println!("   plugin: `claude` not on PATH — install the plugin from a Claude Code session: /plugin install smooth-agent@smooth");
        return;
    }
    if claude_plugin_cache(home).is_some() {
        run_step("plugin", "claude", &["plugin", "update", "smooth-agent@smooth"]);
    } else {
        // Marketplace add is idempotent-ish; an "already exists" failure is fine
        // because the install right after is the step that matters.
        let _ = Command::new("claude").args(["plugin", "marketplace", "add", "SmooAI/smooth"]).output();
        run_step("plugin", "claude", &["plugin", "install", "smooth-agent@smooth"]);
    }
}

fn run_step(label: &str, bin: &str, args: &[&str]) {
    match Command::new(bin).args(args).output() {
        Ok(out) if out.status.success() => println!("   {label}: {} {}", bin, args.join(" ")),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            println!(
                "   {label}: {} `{bin} {}` — {}",
                "FAILED".bright_red(),
                args.join(" "),
                err.trim().lines().next().unwrap_or("(no output)")
            );
        }
        Err(e) => println!("   {label}: {} could not run {bin}: {e}", "FAILED".bright_red()),
    }
}

/// Codex installs plugins through its own session/plugin system; detect and
/// instruct rather than editing its plugin state behind its back.
fn codex_plugin_step(home: &Path) {
    match codex_plugin_enabled(home) {
        Ok(true) => println!("   plugin: smooth-agent@smooth enabled"),
        Ok(false) => println!(
            "   plugin: not installed — in a Codex session run: /plugin install smooth-agent@smooth (marketplace: https://github.com/SmooAI/smooth.git)"
        ),
        Err(e) => println!("   plugin: could not read codex config — {e:#}"),
    }
}

fn statusline_step(home: &Path) {
    if claude_statusline_wired(home) {
        println!("   statusline: wired (settings.json has a statusLine entry — left alone)");
    } else {
        println!("   statusline: not set — run `th doctor --setup-statusline` for the th-mail handle + unread count line");
    }
}

fn status(h: Harness, home: &Path) {
    println!("{}", format!("== {h}").bold().bright_cyan());
    if !h.marker_dir(home).is_dir() {
        println!("   not installed");
        return;
    }
    // dry_run classifies without writing: AlreadyPresent = current,
    // Added = missing, Updated = stale entry pointing elsewhere.
    match mcp_install::install_into(h, home, true) {
        Ok(Outcome::AlreadyPresent) => println!("   mcp: ok"),
        Ok(Outcome::Added) => println!("   mcp: missing — run `th harness enable {h}`"),
        Ok(Outcome::Updated) => println!("   mcp: stale (points elsewhere) — run `th harness enable {h}`"),
        Ok(Outcome::NotInstalled) => println!("   mcp: harness not installed"),
        Err(e) => println!("   mcp: unreadable config — {e:#}"),
    }
    match h {
        Harness::ClaudeCode => {
            match claude_plugin_cache(home) {
                Some(v) => println!("   plugin: smooth-agent {v}"),
                None => println!("   plugin: not installed — `th harness enable claude-code`"),
            }
            statusline_step(home);
        }
        Harness::Codex => match codex_plugin_enabled(home) {
            Ok(true) => println!("   plugin: smooth-agent@smooth enabled"),
            Ok(false) => println!("   plugin: not installed"),
            Err(e) => println!("   plugin: could not read codex config — {e:#}"),
        },
        Harness::OpenCode => {
            let n = smooth_skill_links(home).map(|v| v.len()).unwrap_or(0);
            if n == 0 {
                println!("   skills: none linked — `th harness enable opencode`");
            } else {
                println!("   skills: {n} linked");
            }
        }
    }
}

fn disable(h: Harness, home: &Path) -> Result<()> {
    println!("{}", format!("== {h}").bold().bright_cyan());
    let removed = remove_mcp_entry(h, home)?;
    println!("   mcp: {}", if removed { "entry removed" } else { "no entry (nothing to remove)" });
    if h == Harness::OpenCode {
        let links = smooth_skill_links(home)?;
        for l in &links {
            std::fs::remove_file(l).with_context(|| format!("remove {}", l.display()))?;
        }
        println!("   skills: {} smooth-owned links removed", links.len());
    }
    if h == Harness::ClaudeCode {
        println!("   plugin: left installed — remove with `claude plugin uninstall smooth-agent@smooth` if you mean it");
    }
    Ok(())
}

// ---------------------------------------------------------------- skills ----

/// Where the canonical smooth-agent skills live on this machine: the newest
/// installed Claude plugin cache, else the marketplace checkout.
fn skill_source(home: &Path) -> Option<PathBuf> {
    if let Some(version) = claude_plugin_cache(home) {
        let p = home.join(".claude/plugins/cache/smooth/smooth-agent").join(version).join("skills");
        if p.is_dir() {
            return Some(p);
        }
    }
    let market = home.join(".claude/plugins/marketplaces/smooth/claude-plugins/smooth-agent/skills");
    market.is_dir().then_some(market)
}

/// Newest version directory in the Claude plugin cache, by semver-ish sort.
fn claude_plugin_cache(home: &Path) -> Option<String> {
    let dir = home.join(".claude/plugins/cache/smooth/smooth-agent");
    let mut versions: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    versions.sort_by_key(|v| v.split('.').filter_map(|p| p.parse::<u64>().ok()).collect::<Vec<_>>());
    versions.pop()
}

/// The directory OpenCode-side skills are linked into. `~/.opencode/skills/`
/// is what `th skills` already discovers.
// ponytail: if OpenCode's own skill scan turns out to live elsewhere, the
// lifecycle plugin work (th-cc50cd) is where that gets reconciled.
fn opencode_skills_dir(home: &Path) -> PathBuf {
    home.join(".opencode").join("skills")
}

/// Symlink every canonical skill into the OpenCode skills dir. Never clobbers
/// a real file/dir; repairs stale or dead symlinks. Returns (added, kept).
fn link_skills(home: &Path) -> Result<(usize, usize)> {
    let source = skill_source(home).context("no smooth-agent skills found — enable claude-code first (the plugin checkout is the canonical source)")?;
    let target_dir = opencode_skills_dir(home);
    std::fs::create_dir_all(&target_dir).with_context(|| format!("create {}", target_dir.display()))?;
    let (mut added, mut kept) = (0usize, 0usize);
    for entry in std::fs::read_dir(&source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let link = target_dir.join(entry.file_name());
        match std::fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if std::fs::read_link(&link).is_ok_and(|t| t == entry.path()) {
                    kept += 1;
                    continue;
                }
                std::fs::remove_file(&link)?;
            }
            Ok(_) => {
                // A real file or directory the user owns — never replaced.
                kept += 1;
                continue;
            }
            Err(_) => {}
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(entry.path(), &link).with_context(|| format!("link {}", link.display()))?;
        #[cfg(not(unix))]
        anyhow::bail!("skill links need symlink support; on Windows copy {} manually", source.display());
        #[cfg(unix)]
        {
            added += 1;
        }
    }
    Ok((added, kept))
}

/// Symlinks in the OpenCode skills dir that resolve into smooth-owned sources
/// (the plugin cache or marketplace checkout) — the only things disable removes.
fn smooth_skill_links(home: &Path) -> Result<Vec<PathBuf>> {
    let dir = opencode_skills_dir(home);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(out) };
    let owned_root = home.join(".claude").join("plugins");
    for entry in entries.flatten() {
        let p = entry.path();
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) && std::fs::read_link(&p).is_ok_and(|t| t.starts_with(&owned_root)) {
            out.push(p);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------- detection ----

fn codex_plugin_enabled(home: &Path) -> Result<bool> {
    let path = Harness::Codex.config_path(home);
    if !path.exists() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let doc: toml_edit::DocumentMut = raw.parse().with_context(|| format!("parse {}", path.display()))?;
    Ok(doc
        .get("plugins")
        .and_then(|p| p.get("smooth-agent@smooth"))
        .and_then(|e| e.get("enabled"))
        .and_then(toml_edit::Item::as_bool)
        .unwrap_or(false))
}

fn claude_statusline_wired(home: &Path) -> bool {
    let settings = home.join(".claude").join("settings.json");
    std::fs::read_to_string(settings)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .is_some_and(|v| v.get("statusLine").is_some())
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

// -------------------------------------------------------------- removal ----

/// Remove the `smooth` MCP entry from a harness config. Preserving edits only
/// — same guarantees as the installers in `mcp_install`.
fn remove_mcp_entry(h: Harness, home: &Path) -> Result<bool> {
    let path = h.config_path(home);
    if !path.exists() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    match h {
        Harness::Codex => {
            let mut doc: toml_edit::DocumentMut = raw.parse().with_context(|| format!("parse {}", path.display()))?;
            let removed = doc
                .get_mut("mcp_servers")
                .and_then(toml_edit::Item::as_table_mut)
                .is_some_and(|t| t.remove(mcp_install::SERVER_NAME).is_some());
            if removed {
                std::fs::write(&path, doc.to_string())?;
            }
            Ok(removed)
        }
        Harness::ClaudeCode | Harness::OpenCode => {
            if raw.trim().is_empty() {
                return Ok(false);
            }
            let mut doc: serde_json::Value = serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
            let key = if h == Harness::ClaudeCode { "mcpServers" } else { "mcp" };
            let removed = doc
                .get_mut(key)
                .and_then(serde_json::Value::as_object_mut)
                .is_some_and(|m| m.remove(mcp_install::SERVER_NAME).is_some());
            if removed {
                std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&doc)?))?;
            }
            Ok(removed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A fake home with all marker dirs and a canonical skills source
    /// (marketplace layout) holding two skills.
    fn home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        for h in Harness::ALL {
            std::fs::create_dir_all(h.marker_dir(tmp.path())).unwrap();
        }
        for skill in ["agent-comms", "pearls-flow"] {
            let d = tmp
                .path()
                .join(".claude/plugins/marketplaces/smooth/claude-plugins/smooth-agent/skills")
                .join(skill);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("SKILL.md"), "x").unwrap();
        }
        tmp
    }

    #[test]
    fn skill_source_prefers_the_newest_cache_version_over_the_marketplace() {
        let tmp = home();
        assert!(skill_source(tmp.path())
            .unwrap()
            .ends_with("marketplaces/smooth/claude-plugins/smooth-agent/skills"));
        for v in ["0.4.0", "0.31.1"] {
            std::fs::create_dir_all(tmp.path().join(".claude/plugins/cache/smooth/smooth-agent").join(v).join("skills")).unwrap();
        }
        // 0.31.1 > 0.4.0 numerically even though it sorts lower lexically.
        assert!(skill_source(tmp.path()).unwrap().to_string_lossy().contains("0.31.1"));
    }

    #[test]
    #[cfg(unix)] // exercises real symlinks; on Windows link_skills refuses with guidance instead
    fn link_skills_links_repairs_and_never_clobbers() {
        let tmp = home();
        let (added, kept) = link_skills(tmp.path()).unwrap();
        assert_eq!((added, kept), (2, 0));
        // Idempotent.
        assert_eq!(link_skills(tmp.path()).unwrap(), (0, 2));

        // A user-owned REAL directory of the same name is never replaced.
        let user_dir = opencode_skills_dir(tmp.path()).join("agent-comms");
        std::fs::remove_file(&user_dir).unwrap();
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("SKILL.md"), "mine").unwrap();
        link_skills(tmp.path()).unwrap();
        assert!(!std::fs::symlink_metadata(&user_dir).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(user_dir.join("SKILL.md")).unwrap(), "mine");

        // A dead/stale symlink is repaired.
        let stale = opencode_skills_dir(tmp.path()).join("pearls-flow");
        std::fs::remove_file(&stale).unwrap();
        std::os::unix::fs::symlink("/nonexistent/target", &stale).unwrap();
        let (added, _) = link_skills(tmp.path()).unwrap();
        assert_eq!(added, 1);
        assert!(std::fs::read_link(&stale).unwrap().starts_with(tmp.path()));
    }

    #[test]
    #[cfg(unix)] // exercises real symlinks; on Windows link_skills refuses with guidance instead
    fn disable_removes_only_smooth_owned_links_and_the_mcp_entry() {
        let tmp = home();
        link_skills(tmp.path()).unwrap();
        // User adds their own skill dir + their own MCP server alongside ours.
        let user_dir = opencode_skills_dir(tmp.path()).join("my-own-skill");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(
            Harness::OpenCode.config_path(tmp.path()),
            r#"{"mcp":{"smooth":{"type":"local","command":["th","mcp","serve"]},"other":{"type":"local","command":["x"]}}}"#,
        )
        .unwrap();

        disable(Harness::OpenCode, tmp.path()).unwrap();

        assert!(user_dir.is_dir(), "user skill dir must survive");
        assert!(smooth_skill_links(tmp.path()).unwrap().is_empty());
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(Harness::OpenCode.config_path(tmp.path())).unwrap()).unwrap();
        assert!(doc["mcp"].get("smooth").is_none());
        assert_eq!(doc["mcp"]["other"]["command"], serde_json::json!(["x"]));
    }

    #[test]
    fn remove_mcp_entry_handles_all_three_formats_and_missing_entries() {
        let tmp = home();
        for h in Harness::ALL {
            assert!(!remove_mcp_entry(h, tmp.path()).unwrap(), "{h}: nothing to remove yet");
            mcp_install::install_into(h, tmp.path(), false).unwrap();
            assert!(remove_mcp_entry(h, tmp.path()).unwrap(), "{h}: entry should be removed");
            assert!(!remove_mcp_entry(h, tmp.path()).unwrap(), "{h}: second remove is a no-op");
        }
        // Codex file keeps its other content.
        std::fs::write(
            Harness::Codex.config_path(tmp.path()),
            "model = \"gpt-5.5\"\n[mcp_servers.smooth]\ncommand = \"th\"\n",
        )
        .unwrap();
        assert!(remove_mcp_entry(Harness::Codex, tmp.path()).unwrap());
        let raw = std::fs::read_to_string(Harness::Codex.config_path(tmp.path())).unwrap();
        assert!(raw.contains("model = \"gpt-5.5\""));
        assert!(!raw.contains("mcp_servers.smooth"));
    }

    #[test]
    fn codex_plugin_and_statusline_detection() {
        let tmp = home();
        assert!(!codex_plugin_enabled(tmp.path()).unwrap());
        std::fs::write(Harness::Codex.config_path(tmp.path()), "[plugins.\"smooth-agent@smooth\"]\nenabled = true\n").unwrap();
        assert!(codex_plugin_enabled(tmp.path()).unwrap());

        assert!(!claude_statusline_wired(tmp.path()));
        std::fs::write(tmp.path().join(".claude/settings.json"), r#"{"statusLine":{"type":"command","command":"x"}}"#).unwrap();
        assert!(claude_statusline_wired(tmp.path()));
    }

    #[test]
    fn providers_expands_all_and_rejects_junk() {
        assert_eq!(providers("all").unwrap(), Harness::ALL.to_vec());
        assert_eq!(providers("claude").unwrap(), vec![Harness::ClaudeCode]);
        assert!(providers("cursor").is_err());
    }
}

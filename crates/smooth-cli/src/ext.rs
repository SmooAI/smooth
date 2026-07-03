//! `th ext` — manage SEP (Smooth Extension Protocol) extensions.
//!
//! Install a local extension directory into `~/.smooth/extensions` (global) or
//! `<repo>/.smooth/extensions` (project), list what's installed with its trust
//! state, trust one, or remove it. The trust store + content hashing live in
//! [`smooth_code::sep_host`] so the TUI host reads exactly what this writes.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use smooth_code::sep_host::{hash_extension, TrustStore};
use smooth_operator::extension::manifest::{default_global_dir, project_dir};
use smooth_operator::extension::{Capabilities, ExtensionManifest};

#[derive(Subcommand)]
pub enum ExtCommands {
    /// Install a local extension directory (one containing `extension.toml`)
    /// into the global (`~/.smooth/extensions`) or project (`--project`) store,
    /// then prompt to trust it.
    Install {
        /// Path to the extension directory (must contain `extension.toml`).
        path: PathBuf,
        /// Install into the current repo's `.smooth/extensions` instead of the
        /// global store.
        #[arg(long)]
        project: bool,
        /// Trust without prompting (for scripts / CI).
        #[arg(long)]
        trust: bool,
    },
    /// List installed extensions (global + project) with their trust state.
    List,
    /// Trust an installed extension by name (records its current content hash).
    Trust {
        name: String,
        #[arg(long)]
        project: bool,
    },
    /// Remove an installed extension and its trust record.
    Remove {
        name: String,
        #[arg(long)]
        project: bool,
    },
    /// Re-validate an installed extension after editing it: re-parse its
    /// manifest, re-hash it, and (if the manifest changed) re-confirm trust so
    /// the next host start picks up the new version. A live host reloads it
    /// in-session over the daemon relay (SEP Phase 6).
    Reload {
        name: String,
        #[arg(long)]
        project: bool,
        /// Re-trust without prompting (for scripts / CI).
        #[arg(long)]
        trust: bool,
    },
}

pub fn dispatch(cmd: ExtCommands) -> Result<()> {
    match cmd {
        ExtCommands::Install { path, project, trust } => install(&path, project, trust),
        ExtCommands::List => list(),
        ExtCommands::Trust { name, project } => trust_cmd(&name, project),
        ExtCommands::Remove { name, project } => remove(&name, project),
        ExtCommands::Reload { name, project, trust } => reload(&name, project, trust),
    }
}

/// The extensions dir for the requested scope.
fn scope_dir(project: bool) -> Result<PathBuf> {
    if project {
        let cwd = std::env::current_dir().context("get current dir")?;
        Ok(project_dir(&cwd))
    } else {
        default_global_dir().context("no home dir for the global extensions store")
    }
}

fn install(src: &Path, project: bool, auto_trust: bool) -> Result<()> {
    let manifest = ExtensionManifest::load_dir(src).with_context(|| format!("no valid extension.toml in {}", src.display()))?;
    let name = manifest.name.clone();

    let dest = scope_dir(project)?.join(&name);
    if dest.exists() {
        bail!(
            "extension `{name}` is already installed at {} — remove it first (`th ext remove {name}{}`)",
            dest.display(),
            if project { " --project" } else { "" }
        );
    }
    copy_dir(src, &dest).with_context(|| format!("copy {} → {}", src.display(), dest.display()))?;

    let scope = if project { "project" } else { "global" };
    println!(
        "\n  {} Installed {} {} → {}",
        "✓".green().bold(),
        name.bold(),
        format!("({scope})").dimmed(),
        dest.display().to_string().dimmed()
    );
    print_capabilities(&manifest.capabilities);

    // First-run trust: hash the installed copy and record the decision.
    let hash = hash_extension(&dest)?;
    let trusted = auto_trust || prompt_trust(&name)?;
    let mut store = TrustStore::load();
    store.set(&name, &dest.to_string_lossy(), &hash, trusted);
    store.save()?;

    if trusted {
        println!("  {} Trusted. It will load on the next `th code` session.\n", "✓".green().bold());
    } else {
        println!(
            "  {} Not trusted — it will NOT load. Run {} to enable it.\n",
            "⚠".yellow(),
            format!("th ext trust {name}").cyan()
        );
    }
    Ok(())
}

fn list() -> Result<()> {
    let store = TrustStore::load();
    let mut any = false;
    for (project, label) in [(false, "global"), (true, "project")] {
        let Ok(dir) = scope_dir(project) else { continue };
        let mut entries = installed_in(&dir);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        if entries.is_empty() {
            continue;
        }
        any = true;
        println!("\n  {} {}", label.cyan().bold(), format!("({})", dir.display()).dimmed());
        for (name, root) in entries {
            let hash = hash_extension(&root).unwrap_or_default();
            let (marker, tag) = if store.is_trusted(&name, &hash) {
                ("✓".green().bold().to_string(), "[trusted]".dimmed().to_string())
            } else if store.extensions.contains_key(&name) {
                // Present but hash changed or explicitly distrusted.
                ("⚠".yellow().to_string(), "[untrusted — re-run `th ext trust`]".yellow().to_string())
            } else {
                ("○".dimmed().to_string(), "[untrusted]".yellow().to_string())
            };
            println!("    {} {:<20} {}", marker, name.bold(), tag);
        }
    }
    if any {
        println!();
    } else {
        println!(
            "\n  {} No extensions installed. {} {}\n",
            "ℹ".cyan(),
            "Install one:".dimmed(),
            "th ext install ./path".cyan()
        );
    }
    Ok(())
}

fn trust_cmd(name: &str, project: bool) -> Result<()> {
    let dir = scope_dir(project)?.join(name);
    if !dir.join("extension.toml").exists() {
        bail!("extension `{name}` is not installed{}", if project { " (project)" } else { "" });
    }
    let hash = hash_extension(&dir)?;
    let mut store = TrustStore::load();
    store.set(name, &dir.to_string_lossy(), &hash, true);
    store.save()?;
    println!("  {} Trusted {}.", "✓".green().bold(), name.bold());
    Ok(())
}

fn reload(name: &str, project: bool, auto_trust: bool) -> Result<()> {
    let dir = scope_dir(project)?.join(name);
    // Re-parse the manifest — surfaces a syntax error the edit may have introduced.
    let manifest = ExtensionManifest::load_dir(&dir).with_context(|| format!("extension `{name}` is not installed or its extension.toml is invalid"))?;

    let hash = hash_extension(&dir)?;
    let mut store = TrustStore::load();
    let record = store.extensions.get(name).cloned();
    let changed = record.as_ref().map(|r| r.hash != hash).unwrap_or(true);

    println!(
        "\n  {} Reloading {} {}",
        "↻".cyan().bold(),
        name.bold(),
        format!("v{}", manifest.version).dimmed()
    );
    print_capabilities(&manifest.capabilities);

    if !changed {
        // Manifest unchanged since it was trusted → nothing to re-confirm.
        let state = if record.as_ref().is_some_and(|r| r.trusted) {
            format!("{} still trusted", "✓".green().bold())
        } else {
            format!("{} still untrusted — run `th ext trust {name}`", "⚠".yellow())
        };
        println!("  {state} (manifest unchanged).");
    } else {
        // The manifest changed (or was never trusted) — fail-safe requires a
        // fresh trust decision before it will load.
        let trusted = auto_trust || prompt_trust(name)?;
        store.set(name, &dir.to_string_lossy(), &hash, trusted);
        store.save()?;
        if trusted {
            println!("  {} Manifest changed — re-trusted.", "✓".green().bold());
        } else {
            println!("  {} Manifest changed — left untrusted; it will NOT load.", "⚠".yellow());
        }
    }
    println!(
        "  {} Takes effect on the next {} session (in-session live reload lands with the daemon relay).\n",
        "ℹ".cyan(),
        "th code".cyan()
    );
    Ok(())
}

fn remove(name: &str, project: bool) -> Result<()> {
    let dir = scope_dir(project)?.join(name);
    if !dir.exists() {
        bail!("extension `{name}` is not installed{}", if project { " (project)" } else { "" });
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    let mut store = TrustStore::load();
    if store.remove(name) {
        store.save()?;
    }
    println!("  {} Removed {}.", "✓".green().bold(), name.bold());
    Ok(())
}

// --- helpers ---

fn print_capabilities(caps: &Capabilities) {
    let mut on: Vec<&str> = Vec::new();
    for (flag, label) in [
        (caps.tools, "tools"),
        (caps.commands, "commands"),
        (caps.ui, "ui"),
        (caps.exec, "exec"),
        (caps.kv, "kv"),
        (caps.bus, "bus"),
        (caps.session, "session"),
    ] {
        if flag {
            on.push(label);
        }
    }
    let mut line = if on.is_empty() { "(none)".to_string() } else { on.join(", ") };
    if !caps.events.is_empty() {
        line.push_str(&format!("; events: {}", caps.events.join(", ")));
    }
    println!("  {} {}", "Capabilities:".dimmed(), line.yellow());
}

fn prompt_trust(name: &str) -> Result<bool> {
    // Non-interactive (piped/CI) → don't trust silently; require `th ext trust`.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(false);
    }
    let ok = dialoguer::Confirm::new()
        .with_prompt(format!("Trust `{name}` and its declared capabilities?"))
        .default(false)
        .interact()
        .unwrap_or(false);
    Ok(ok)
}

/// Every subdirectory of `dir` that carries an `extension.toml`.
fn installed_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let root = entry.path();
        if root.join("extension.toml").is_file() {
            if let Some(name) = root.file_name().and_then(|n| n.to_str()) {
                out.push((name.to_string(), root));
            }
        }
    }
    out
}

/// Recursively copy `src` into `dest` (std has no recursive copy).
fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_dir_recurses() {
        let tmp = std::env::temp_dir().join(format!("th-ext-test-{}", std::process::id()));
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("extension.toml"), "name=\"x\"").unwrap();
        std::fs::write(src.join("sub/a.txt"), "a").unwrap();
        let dest = tmp.join("dest");
        copy_dir(&src, &dest).unwrap();
        assert!(dest.join("extension.toml").is_file());
        assert!(dest.join("sub/a.txt").is_file());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn reload_retrusts_only_when_the_manifest_changes() {
        // Isolate the global store to a temp SMOOTH_HOME (scope_dir + TrustStore
        // both resolve through it). Single test → no env race with siblings.
        let home = std::env::temp_dir().join(format!("th-ext-reload-{}", std::process::id()));
        let ext_dir = home.join("extensions").join("demo");
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("extension.toml"),
            "name = \"demo\"\nversion = \"0.1.0\"\n[run]\ncommand = \"node\"\n",
        )
        .unwrap();
        std::env::set_var("SMOOTH_HOME", &home);

        // Trust the current content, then reload with no edit → stays trusted,
        // and reload must NOT flip it (the hash still matches).
        let h0 = hash_extension(&ext_dir).unwrap();
        let mut store = TrustStore::load();
        store.set("demo", &ext_dir.to_string_lossy(), &h0, true);
        store.save().unwrap();
        reload("demo", false, false).unwrap();
        assert!(TrustStore::load().is_trusted("demo", &h0), "unchanged reload keeps trust");

        // Edit the manifest → the old hash no longer matches, so it is untrusted
        // until re-confirmed; `--trust` re-confirms against the NEW hash.
        std::fs::write(
            ext_dir.join("extension.toml"),
            "name = \"demo\"\nversion = \"0.2.0\"\n[run]\ncommand = \"node\"\n",
        )
        .unwrap();
        let h1 = hash_extension(&ext_dir).unwrap();
        assert_ne!(h0, h1);
        assert!(!TrustStore::load().is_trusted("demo", &h1), "an edited manifest is untrusted until reloaded");
        reload("demo", false, true).unwrap();
        assert!(TrustStore::load().is_trusted("demo", &h1), "reload --trust re-trusts the new hash");

        std::env::remove_var("SMOOTH_HOME");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn installed_in_finds_manifested_dirs() {
        let tmp = std::env::temp_dir().join(format!("th-ext-list-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("good")).unwrap();
        std::fs::write(tmp.join("good/extension.toml"), "name=\"good\"").unwrap();
        std::fs::create_dir_all(tmp.join("notext")).unwrap();
        let found = installed_in(&tmp);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "good");
        std::fs::remove_dir_all(&tmp).ok();
    }
}

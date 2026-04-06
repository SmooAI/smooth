//! Git hook management for Smooth.
//!
//! Installs `.githooks/` scripts that run cargo quality gates and pearl
//! lifecycle operations. `th hooks install` writes the scripts and sets
//! `core.hooksPath`. `th hooks run <hook>` is called *from* those scripts
//! to execute pearl-specific logic.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use owo_colors::OwoColorize;

/// Directory name (relative to repo root) where hooks live.
const HOOKS_DIR: &str = ".githooks";

// ── Hook templates ────────────────────────────────────────────────

const PRE_COMMIT: &str = r#"#!/usr/bin/env sh
set -e

# ── Cargo format check ──────────────────────────────────────────────
echo "pre-commit: cargo fmt --check"
cargo fmt -- --check || {
    echo >&2 "pre-commit: formatting issues found. Run 'cargo fmt' and re-stage."
    exit 1
}

# ── Cargo clippy ─────────────────────────────────────────────────────
echo "pre-commit: cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings || {
    echo >&2 "pre-commit: clippy warnings found. Fix them before committing."
    exit 1
}

# ── Pearl hooks ──────────────────────────────────────────────────────
if command -v th >/dev/null 2>&1; then
    th hooks run pre-commit "$@" || true
fi
"#;

const PRE_PUSH: &str = r#"#!/usr/bin/env sh
set -e

# ── Cargo tests ──────────────────────────────────────────────────────
echo "pre-push: cargo test"
cargo test --workspace || {
    echo >&2 "pre-push: tests failed. Fix them before pushing."
    exit 1
}

# ── Pearl hooks ──────────────────────────────────────────────────────
if command -v th >/dev/null 2>&1; then
    th hooks run pre-push "$@" || true
fi
"#;

const PREPARE_COMMIT_MSG: &str = r#"#!/usr/bin/env sh
# ── Pearl hooks ──────────────────────────────────────────────────────
if command -v th >/dev/null 2>&1; then
    th hooks run prepare-commit-msg "$@"
fi
"#;

const POST_CHECKOUT: &str = r#"#!/usr/bin/env sh
# ── Pearl hooks ──────────────────────────────────────────────────────
if command -v th >/dev/null 2>&1; then
    th hooks run post-checkout "$@" || true
fi
"#;

const POST_MERGE: &str = r#"#!/usr/bin/env sh
# ── Pearl hooks ──────────────────────────────────────────────────────
if command -v th >/dev/null 2>&1; then
    th hooks run post-merge "$@" || true
fi
"#;

/// All hook templates we install.
const HOOK_TEMPLATES: &[(&str, &str)] = &[
    ("pre-commit", PRE_COMMIT),
    ("pre-push", PRE_PUSH),
    ("prepare-commit-msg", PREPARE_COMMIT_MSG),
    ("post-checkout", POST_CHECKOUT),
    ("post-merge", POST_MERGE),
];

// ── Install ───────────────────────────────────────────────────────

/// Find the git repo root from `start_dir` by walking up.
fn find_git_root(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Install `.githooks/` scripts and set `core.hooksPath`.
/// Returns the hooks directory path on success.
pub fn install(repo_root: Option<&Path>) -> Result<PathBuf> {
    let root = match repo_root {
        Some(r) => r.to_path_buf(),
        None => {
            let cwd = std::env::current_dir()?;
            find_git_root(&cwd).context("not in a git repository")?
        }
    };

    let hooks_dir = root.join(HOOKS_DIR);
    fs::create_dir_all(&hooks_dir).context("create .githooks/ directory")?;

    for (name, content) in HOOK_TEMPLATES {
        let path = hooks_dir.join(name);
        fs::write(&path, content).with_context(|| format!("write hook {name}"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).with_context(|| format!("chmod hook {name}"))?;
    }

    // Set core.hooksPath
    let status = Command::new("git")
        .args(["config", "core.hooksPath", HOOKS_DIR])
        .current_dir(&root)
        .status()
        .context("run git config")?;
    if !status.success() {
        anyhow::bail!("git config core.hooksPath failed");
    }

    Ok(hooks_dir)
}

/// Check whether hooks are properly installed.
pub fn check(repo_root: Option<&Path>) -> HooksStatus {
    let root = match repo_root {
        Some(r) => r.to_path_buf(),
        None => match std::env::current_dir().ok().and_then(|c| find_git_root(&c)) {
            Some(r) => r,
            None => return HooksStatus::NotGitRepo,
        },
    };

    let hooks_dir = root.join(HOOKS_DIR);
    if !hooks_dir.is_dir() {
        return HooksStatus::Missing;
    }

    // Check all expected hooks exist
    let mut missing_hooks = Vec::new();
    for (name, _) in HOOK_TEMPLATES {
        let path = hooks_dir.join(name);
        if !path.exists() {
            missing_hooks.push((*name).to_string());
        }
    }
    if !missing_hooks.is_empty() {
        return HooksStatus::Incomplete(missing_hooks);
    }

    // Check core.hooksPath
    let output = Command::new("git").args(["config", "core.hooksPath"]).current_dir(&root).output().ok();
    let hooks_path = output
        .as_ref()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    match hooks_path {
        Some(p) if p == HOOKS_DIR => HooksStatus::Ok,
        Some(p) => HooksStatus::WrongPath(p),
        None => HooksStatus::PathNotSet,
    }
}

/// Result of hooks health check.
#[derive(Debug)]
pub enum HooksStatus {
    Ok,
    NotGitRepo,
    Missing,
    Incomplete(Vec<String>),
    WrongPath(String),
    PathNotSet,
}

impl HooksStatus {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

// ── Run (pearl-specific hook logic) ───────────────────────────────

/// Execute pearl-specific logic for the given git hook.
pub fn run_hook(hook_name: &str, args: &[String]) -> Result<()> {
    match hook_name {
        "pre-commit" => run_pre_commit(),
        "pre-push" => run_pre_push(),
        "prepare-commit-msg" => run_prepare_commit_msg(args),
        "post-checkout" => run_post_checkout(),
        "post-merge" => run_post_merge(),
        _ => {
            eprintln!("smooth: unknown hook {hook_name:?}, skipping");
            Ok(())
        }
    }
}

/// pre-commit: auto-commit any pending pearl Dolt changes so they're
/// included in the git commit's tree.
fn run_pre_commit() -> Result<()> {
    let Some(dolt_dir) = find_dolt_dir_quiet() else {
        return Ok(());
    };
    let dolt = smooth_pearls::SmoothDolt::new(&dolt_dir)?;

    // Check if there are uncommitted Dolt changes
    let status = dolt.status()?;
    if !status.trim().is_empty() {
        dolt.commit("auto-commit pearl changes")?;
    }

    Ok(())
}

/// pre-push: push pearl data to Dolt remote (best-effort).
fn run_pre_push() -> Result<()> {
    let Some(dolt_dir) = find_dolt_dir_quiet() else {
        return Ok(());
    };
    let dolt = smooth_pearls::SmoothDolt::new(&dolt_dir)?;

    // Only push if a remote is configured
    let remotes = dolt.remote_list().unwrap_or_default();
    if !remotes.trim().is_empty() {
        match dolt.push() {
            Ok(output) => {
                if !output.trim().is_empty() {
                    eprintln!("smooth: pearl push: {output}");
                }
            }
            Err(e) => eprintln!("smooth: pearl push failed (non-fatal): {e}"),
        }
    }

    Ok(())
}

/// prepare-commit-msg: if the branch name starts with a pearl ID (th-XXXXXX),
/// prepend it to the commit message.
fn run_prepare_commit_msg(args: &[String]) -> Result<()> {
    // args[0] = commit message file, args[1] = source (message, merge, etc.)
    let msg_file = match args.first() {
        Some(f) => PathBuf::from(f),
        None => return Ok(()),
    };

    // Don't touch merge/squash/amend messages
    let source = args.get(1).map(String::as_str).unwrap_or("");
    if matches!(source, "merge" | "squash" | "commit") {
        return Ok(());
    }

    let branch = current_branch().unwrap_or_default();
    let pearl_id = extract_pearl_id(&branch);
    if pearl_id.is_empty() {
        return Ok(());
    }

    let existing = fs::read_to_string(&msg_file).unwrap_or_default();
    // Don't prepend if already present
    if existing.contains(pearl_id) {
        return Ok(());
    }

    let new_msg = format!("{pearl_id}: {existing}");
    fs::write(&msg_file, new_msg)?;

    Ok(())
}

/// post-checkout: no-op for now (placeholder for future pearl state refresh).
fn run_post_checkout() -> Result<()> {
    Ok(())
}

/// post-merge: auto-commit any Dolt changes that came from the merge.
fn run_post_merge() -> Result<()> {
    // Same logic as pre-commit: capture any pearl state changes
    run_pre_commit()
}

// ── Helpers ───────────────────────────────────────────────────────

/// Try to find `.smooth/dolt/` without printing errors.
fn find_dolt_dir_quiet() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    smooth_pearls::dolt::find_repo_dolt_dir(&cwd)
}

/// Get the current git branch name.
fn current_branch() -> Option<String> {
    let output = Command::new("git").args(["symbolic-ref", "--short", "HEAD"]).output().ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Extract a pearl ID (th-XXXXXX) from a branch name.
/// Handles patterns like `th-33b2a2-some-description` or `th-33b2a2`.
fn extract_pearl_id(branch: &str) -> &str {
    // Pearl IDs are `th-` followed by 6 hex chars
    if branch.len() >= 9 && branch.starts_with("th-") && branch[3..9].chars().all(|c| c.is_ascii_hexdigit()) {
        &branch[..9]
    } else {
        ""
    }
}

// ── CLI display helpers ───────────────────────────────────────────

/// Print install result (used by `th hooks install` and `th pearls init`).
pub fn print_install_result(hooks_dir: &Path) {
    println!("{} Git hooks installed at {}", "✓".green().bold(), hooks_dir.display());
    println!("  {} cargo fmt --check + clippy", "pre-commit:".bold());
    println!("  {} cargo test", "pre-push:".bold());
    println!("  {} pearl ID from branch name", "prepare-commit-msg:".bold());
    println!("  {} pearl Dolt auto-commit/push", "pearl lifecycle:".bold());
}

/// Print doctor-style status for hooks.
pub fn print_doctor_status(status: &HooksStatus) -> bool {
    match status {
        HooksStatus::Ok => {
            println!("  {} Hooks: {}", "✓".green().bold(), "installed (.githooks/)".green());
            true
        }
        HooksStatus::NotGitRepo => {
            println!("  {} Hooks: {}", "○".dimmed(), "not in a git repo".dimmed());
            true // not an issue per se
        }
        HooksStatus::Missing => {
            println!("  {} Hooks: {}", "✗".red().bold(), "not installed (run: th hooks install)".red());
            false
        }
        HooksStatus::Incomplete(missing) => {
            println!(
                "  {} Hooks: {}",
                "✗".red().bold(),
                format!("incomplete — missing: {} (run: th hooks install)", missing.join(", ")).red()
            );
            false
        }
        HooksStatus::WrongPath(path) => {
            println!(
                "  {} Hooks: {}",
                "✗".red().bold(),
                format!("core.hooksPath is \"{path}\" instead of \".githooks\" (run: th hooks install)").red()
            );
            false
        }
        HooksStatus::PathNotSet => {
            println!("  {} Hooks: {}", "✗".red().bold(), "core.hooksPath not set (run: th hooks install)".red());
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_pearl_id_from_branch() {
        assert_eq!(extract_pearl_id("th-33b2a2-some-desc"), "th-33b2a2");
        assert_eq!(extract_pearl_id("th-abcdef"), "th-abcdef");
        assert_eq!(extract_pearl_id("th-ABCDEF-upper"), "th-ABCDEF");
        assert_eq!(extract_pearl_id("main"), "");
        assert_eq!(extract_pearl_id("feature/something"), "");
        assert_eq!(extract_pearl_id("th-zz"), ""); // too short
        assert_eq!(extract_pearl_id("th-gggggg"), ""); // not hex
        assert_eq!(extract_pearl_id(""), "");
    }

    #[test]
    fn hook_templates_are_valid_shell() {
        for (name, content) in HOOK_TEMPLATES {
            assert!(content.starts_with("#!/usr/bin/env sh"), "hook {name} missing shebang");
            assert!(!content.is_empty(), "hook {name} is empty");
        }
    }

    #[test]
    fn hook_templates_have_pearl_integration() {
        for (name, content) in HOOK_TEMPLATES {
            assert!(content.contains("th hooks run"), "hook {name} missing `th hooks run` call");
        }
    }

    #[test]
    fn check_returns_not_git_repo_outside_repo() {
        // Use tempfile to get a dir that's guaranteed not inside a git repo
        let tmp = tempfile::tempdir().expect("create temp dir");
        // tempfile dirs aren't git repos, but find_git_root walks up.
        // To guarantee no .git ancestor, we check the returned status is
        // either NotGitRepo or Missing (if the temp dir happens to be
        // under a git repo).
        let status = check(Some(tmp.path()));
        assert!(
            matches!(status, HooksStatus::NotGitRepo | HooksStatus::Missing | HooksStatus::PathNotSet),
            "expected NotGitRepo/Missing/PathNotSet, got {status:?}"
        );
    }

    #[test]
    fn install_and_check_in_temp_repo() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let root = tmp.path();

        // git init
        Command::new("git").args(["init"]).current_dir(root).output().expect("git init");

        // Before install: missing
        assert!(matches!(check(Some(root)), HooksStatus::Missing | HooksStatus::PathNotSet));

        // Install
        let hooks_dir = install(Some(root)).expect("install hooks");
        assert!(hooks_dir.join("pre-commit").exists());
        assert!(hooks_dir.join("pre-push").exists());
        assert!(hooks_dir.join("prepare-commit-msg").exists());
        assert!(hooks_dir.join("post-checkout").exists());
        assert!(hooks_dir.join("post-merge").exists());

        // After install: ok
        assert!(check(Some(root)).is_ok());

        // Verify executable permissions
        for (name, _) in HOOK_TEMPLATES {
            let path = hooks_dir.join(name);
            let meta = std::fs::metadata(&path).expect("stat hook");
            assert!(meta.permissions().mode() & 0o111 != 0, "hook {name} not executable");
        }
    }
}

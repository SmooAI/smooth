//! Beads CLI wrapper — typed interface to the `bd` command.
//!
//! Beads is the durable system of record for all work items. This module
//! wraps the `bd` CLI to provide a Rust-native API.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Beads directory: `~/.smooth/.beads/`
fn beads_dir() -> PathBuf {
    dirs_next::home_dir().unwrap_or_default().join(".smooth").join(".beads")
}

/// Ensure the beads directory exists.
pub fn ensure_beads_dir() -> Result<PathBuf> {
    let dir = beads_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Get the beads directory path.
#[must_use]
pub fn get_beads_dir() -> PathBuf {
    beads_dir()
}

/// A bead (work item) from `bd list --json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bead {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: u8,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(rename = "type", default)]
    pub bead_type: String,
}

/// Run a `bd` command and capture output.
fn bd(args: &[&str]) -> Result<String> {
    let output = Command::new("bd")
        .args(args)
        .output()
        .context("Failed to run bd command. Is beads installed?")?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("bd command failed: {stderr}");
    }
}

/// Run `bd` with `--json` and parse output.
fn bd_json<T: for<'de> Deserialize<'de>>(args: &[&str]) -> Result<T> {
    let mut full_args = args.to_vec();
    full_args.push("--json");
    let output = bd(&full_args)?;
    serde_json::from_str(&output).context("Failed to parse bd JSON output")
}

/// List beads with optional status filter.
pub fn list_beads(status: Option<&str>) -> Result<Vec<Bead>> {
    let mut args = vec!["list"];
    let status_flag;
    if let Some(s) = status {
        status_flag = format!("--status={s}");
        args.push(&status_flag);
    }
    bd_json(&args)
}

/// Get ready beads (open, no blockers).
pub fn get_ready() -> Result<Vec<Bead>> {
    bd_json(&["ready"])
}

/// Get a specific bead by ID.
pub fn get_bead(id: &str) -> Result<serde_json::Value> {
    bd_json(&["show", id])
}

/// Create a new bead.
pub fn create_bead(title: &str, description: &str, bead_type: &str, priority: u8) -> Result<String> {
    let priority_str = priority.to_string();
    let output = bd(&[
        "create",
        "--title",
        title,
        "--description",
        description,
        "--type",
        bead_type,
        "--priority",
        &priority_str,
    ])?;
    // Extract bead ID from output (format: "✓ Created issue: smooth-abc")
    let id = output.split_whitespace().last().unwrap_or("unknown").to_string();
    Ok(id)
}

/// Update a bead's status.
pub fn update_bead_status(id: &str, status: &str) -> Result<()> {
    let flag = format!("--status={status}");
    bd(&["update", id, &flag])?;
    Ok(())
}

/// Close beads.
pub fn close_beads(ids: &[&str]) -> Result<()> {
    let mut args = vec!["close"];
    args.extend_from_slice(ids);
    bd(&args)?;
    Ok(())
}

/// Add a comment to a bead.
pub fn add_comment(id: &str, content: &str) -> Result<()> {
    bd(&["comment", id, content])?;
    Ok(())
}

/// Get comments for a bead.
pub fn get_comments(id: &str) -> Result<Vec<serde_json::Value>> {
    bd_json(&["show", id, "--comments"])
}

/// Check if bd is available.
pub fn is_available() -> bool {
    Command::new("bd").arg("--version").output().map_or(false, |o| o.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_beads_dir() {
        let dir = beads_dir();
        assert!(dir.to_string_lossy().contains(".smooth"));
    }

    #[test]
    fn test_is_available() {
        // This test just verifies the function doesn't panic
        let _ = is_available();
    }
}

//! smooth-dolt subprocess wrapper.
//!
//! Provides a clean Rust interface to the `smooth-dolt` Go binary for
//! all Dolt operations (init, SQL, commit, push, pull, log, remote, gc).
//! The binary is located once at startup and reused for all calls.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::Value;

/// Handle to the smooth-dolt binary. All Dolt operations go through this.
#[derive(Debug, Clone)]
pub struct SmoothDolt {
    /// Path to the smooth-dolt binary.
    bin: PathBuf,
    /// Path to the Dolt data directory (e.g., `.smooth/dolt/`).
    data_dir: PathBuf,
}

impl SmoothDolt {
    /// Create a new handle pointing at the given data directory.
    /// Locates the `smooth-dolt` binary automatically.
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let bin = find_smooth_dolt_binary().context("smooth-dolt binary not found. Run: scripts/build-smooth-dolt.sh")?;
        Ok(Self {
            bin,
            data_dir: data_dir.into(),
        })
    }

    /// Create a handle with an explicit binary path (for testing).
    #[must_use]
    pub fn with_bin(bin: PathBuf, data_dir: PathBuf) -> Self {
        Self { bin, data_dir }
    }

    /// Initialize a new Dolt database at the data directory.
    pub fn init(&self) -> Result<String> {
        self.run(&["init", &self.data_dir_str()])
    }

    /// Execute a SQL query and return parsed JSON results.
    pub fn sql(&self, query: &str) -> Result<Vec<Value>> {
        let output = self.run(&["sql", &self.data_dir_str(), "-q", query])?;
        if output.is_empty() || output == "null" {
            return Ok(Vec::new());
        }
        let parsed: Vec<Value> = serde_json::from_str(&output).with_context(|| format!("parse smooth-dolt sql output: {output}"))?;
        Ok(parsed)
    }

    /// Execute a SQL statement (INSERT/UPDATE/DELETE/CREATE). Returns raw output.
    pub fn exec(&self, statement: &str) -> Result<String> {
        self.run(&["sql", &self.data_dir_str(), "-q", statement])
    }

    /// Stage all changes and commit with a message.
    pub fn commit(&self, message: &str) -> Result<String> {
        self.run(&["commit", &self.data_dir_str(), "-m", message])
    }

    /// Query the Dolt commit log. Returns vec of (hash, author, date, message).
    pub fn log(&self, limit: usize) -> Result<Vec<(String, String, String, String)>> {
        let output = self.run(&["log", &self.data_dir_str(), "-n", &limit.to_string()])?;
        let mut entries = Vec::new();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Format: "hash message (author) date"
            // Just pass through as a single string for now.
            entries.push((line.to_string(), String::new(), String::new(), String::new()));
        }
        Ok(entries)
    }

    /// Push to the configured Dolt remote (refs/dolt/data on git origin).
    pub fn push(&self) -> Result<String> {
        self.run(&["push", &self.data_dir_str()])
    }

    /// Pull from the configured Dolt remote.
    pub fn pull(&self) -> Result<String> {
        self.run(&["pull", &self.data_dir_str()])
    }

    /// Add a Dolt remote.
    pub fn remote_add(&self, name: &str, url: &str) -> Result<String> {
        self.run(&["remote", &self.data_dir_str(), "add", name, url])
    }

    /// List configured Dolt remotes.
    pub fn remote_list(&self) -> Result<String> {
        self.run(&["remote", &self.data_dir_str(), "list"])
    }

    /// Garbage collect — compact the database to minimize storage.
    pub fn gc(&self) -> Result<String> {
        self.run(&["gc", &self.data_dir_str()])
    }

    /// Check the Dolt status (working set changes).
    pub fn status(&self) -> Result<String> {
        self.run(&["status", &self.data_dir_str()])
    }

    /// Get the version of the smooth-dolt binary.
    pub fn version(&self) -> Result<String> {
        let output = Command::new(&self.bin)
            .arg("version")
            .output()
            .with_context(|| format!("exec smooth-dolt version: {}", self.bin.display()))?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// The data directory as a string.
    fn data_dir_str(&self) -> String {
        self.data_dir.to_string_lossy().to_string()
    }

    /// Run a smooth-dolt command and return stdout.
    fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(&self.bin)
            .args(args)
            .output()
            .with_context(|| format!("exec smooth-dolt {}: {}", args.join(" "), self.bin.display()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("smooth-dolt {} failed: {}", args.first().unwrap_or(&""), stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// Locate the smooth-dolt binary.
///
/// Resolution order:
///  1. `SMOOTH_DOLT` env var (absolute path)
///  2. `target/release/smooth-dolt` relative to CARGO_MANIFEST_DIR (dev builds)
///  3. Same directory as the current executable (installed alongside `th`)
///  4. `PATH` lookup
fn find_smooth_dolt_binary() -> Option<PathBuf> {
    // 1. Explicit env var.
    if let Ok(p) = std::env::var("SMOOTH_DOLT") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }

    // 2. Workspace target/ directory (dev).
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let mut dir = PathBuf::from(manifest);
        for _ in 0..5 {
            let candidate = dir.join("target").join("release").join("smooth-dolt");
            if candidate.is_file() {
                return Some(candidate);
            }
            if !dir.pop() {
                break;
            }
        }
    }

    // 3. Next to the current executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("smooth-dolt");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // 4. PATH lookup.
    which_smooth_dolt()
}

fn which_smooth_dolt() -> Option<PathBuf> {
    let output = Command::new("which").arg("smooth-dolt").output().ok()?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Check if a `.smooth/dolt/` directory exists in any parent of `start_dir`.
pub fn find_repo_dolt_dir(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(".smooth").join("dolt");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_binary_resolution_order() {
        // Just verify the function doesn't panic. The binary may or may
        // not exist depending on the dev environment.
        let _ = find_smooth_dolt_binary();
    }

    #[test]
    fn find_repo_dolt_dir_returns_none_for_tmp() {
        let tmp = std::env::temp_dir();
        assert!(find_repo_dolt_dir(&tmp).is_none());
    }
}

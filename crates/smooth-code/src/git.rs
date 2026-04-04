//! Git integration — typed interface to `git` commands via `std::process::Command`.

use std::fmt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Status of a file in the git working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Staged,
}

impl fmt::Display for FileStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Modified => write!(f, "modified"),
            Self::Added => write!(f, "added"),
            Self::Deleted => write!(f, "deleted"),
            Self::Renamed => write!(f, "renamed"),
            Self::Untracked => write!(f, "untracked"),
            Self::Staged => write!(f, "staged"),
        }
    }
}

/// A single file's git status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitFileStatus {
    pub path: String,
    pub status: FileStatus,
}

/// Snapshot of the current git repository state.
#[derive(Debug, Clone)]
pub struct GitState {
    pub branch: String,
    pub is_repo: bool,
    pub files: Vec<GitFileStatus>,
}

impl GitState {
    /// Refresh git state by parsing `git status --porcelain -b`.
    ///
    /// # Errors
    ///
    /// Returns an error if the `git` command cannot be executed.
    pub fn refresh(working_dir: &Path) -> Result<Self> {
        let output = Command::new("git")
            .args(["status", "--porcelain", "-b"])
            .current_dir(working_dir)
            .output()
            .context("Failed to run git status")?;

        if !output.status.success() {
            // Not a git repo or git not available
            return Ok(Self {
                branch: String::new(),
                is_repo: false,
                files: Vec::new(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Self::parse_porcelain(&stdout)
    }

    /// Parse porcelain output into a `GitState`.
    ///
    /// # Errors
    ///
    /// This is infallible but returns `Result` for API consistency.
    pub fn parse_porcelain(output: &str) -> Result<Self> {
        let mut branch = String::new();
        let mut files = Vec::new();

        for line in output.lines() {
            if let Some(header) = line.strip_prefix("## ") {
                // Branch line: "## main...origin/main" or "## HEAD (no branch)" or "## main"
                branch = header.split("...").next().unwrap_or(header).to_string();
                continue;
            }

            if line.len() < 4 {
                continue;
            }

            let index = line.as_bytes()[0];
            let worktree = line.as_bytes()[1];
            let path = line[3..].to_string();

            // Staged changes (index column has a letter, worktree is space or has secondary change)
            if index != b' ' && index != b'?' {
                files.push(GitFileStatus {
                    path: path.clone(),
                    status: FileStatus::Staged,
                });
            }

            // Worktree changes
            match worktree {
                b'M' => files.push(GitFileStatus {
                    path,
                    status: FileStatus::Modified,
                }),
                b'D' => files.push(GitFileStatus {
                    path,
                    status: FileStatus::Deleted,
                }),
                b'?' => files.push(GitFileStatus {
                    path,
                    status: FileStatus::Untracked,
                }),
                _ => {
                    // For index-only changes already captured above, or renames
                    if index == b'R' {
                        // Replace the Staged entry with Renamed
                        if let Some(last) = files.last_mut() {
                            if last.path == path {
                                last.status = FileStatus::Renamed;
                            }
                        }
                    } else if index == b'A' && worktree == b' ' {
                        // Replace Staged with Added for newly added files
                        if let Some(last) = files.last_mut() {
                            if last.path == path {
                                last.status = FileStatus::Added;
                            }
                        }
                    }
                }
            }
        }

        Ok(Self { branch, is_repo: true, files })
    }

    /// Get the diff for a specific file.
    ///
    /// # Errors
    ///
    /// Returns an error if the `git diff` command cannot be executed.
    pub fn diff(working_dir: &Path, file: &str) -> Result<String> {
        let output = Command::new("git")
            .args(["diff", file])
            .current_dir(working_dir)
            .output()
            .context("Failed to run git diff")?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Stage a file with `git add`.
    ///
    /// # Errors
    ///
    /// Returns an error if `git add` fails (e.g., file does not exist).
    pub fn stage(working_dir: &Path, file: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["add", file])
            .current_dir(working_dir)
            .output()
            .context("Failed to run git add")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git add failed: {stderr}");
        }
        Ok(())
    }

    /// Unstage a file with `git restore --staged`.
    ///
    /// # Errors
    ///
    /// Returns an error if `git restore --staged` fails.
    pub fn unstage(working_dir: &Path, file: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["restore", "--staged", file])
            .current_dir(working_dir)
            .output()
            .context("Failed to run git restore --staged")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git restore --staged failed: {stderr}");
        }
        Ok(())
    }

    /// Commit staged changes.
    ///
    /// # Errors
    ///
    /// Returns an error if `git commit` fails (e.g., nothing staged).
    pub fn commit(working_dir: &Path, message: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(working_dir)
            .output()
            .context("Failed to run git commit")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git commit failed: {stderr}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_porcelain_output() {
        let output = "## main...origin/main\n M src/lib.rs\n?? newfile.txt\nA  added.rs\nD  deleted.rs\n";
        let state = GitState::parse_porcelain(output).unwrap();

        assert!(state.is_repo);
        assert_eq!(state.branch, "main");
        assert!(!state.files.is_empty());

        // Check that we found the modified file
        let modified: Vec<_> = state.files.iter().filter(|f| f.status == FileStatus::Modified).collect();
        assert!(!modified.is_empty());
        assert_eq!(modified[0].path, "src/lib.rs");

        // Check untracked
        let untracked: Vec<_> = state.files.iter().filter(|f| f.status == FileStatus::Untracked).collect();
        assert!(!untracked.is_empty());
        assert_eq!(untracked[0].path, "newfile.txt");
    }

    #[test]
    fn test_file_status_display() {
        assert_eq!(format!("{}", FileStatus::Modified), "modified");
        assert_eq!(format!("{}", FileStatus::Added), "added");
        assert_eq!(format!("{}", FileStatus::Deleted), "deleted");
        assert_eq!(format!("{}", FileStatus::Renamed), "renamed");
        assert_eq!(format!("{}", FileStatus::Untracked), "untracked");
        assert_eq!(format!("{}", FileStatus::Staged), "staged");
    }

    #[test]
    fn test_git_state_no_repo() {
        // Use a temp dir that is definitely not a git repo
        let tmp = tempfile::tempdir().unwrap();
        let state = GitState::refresh(tmp.path()).unwrap();
        assert!(!state.is_repo);
        assert!(state.branch.is_empty());
        assert!(state.files.is_empty());
    }

    #[test]
    fn test_git_diff_returns_string() {
        // In a non-repo dir, diff should still return without panicking
        let tmp = tempfile::tempdir().unwrap();
        // git diff on a non-repo will fail but we return empty-ish output
        let result = GitState::diff(tmp.path(), "nonexistent.rs");
        // Should not panic — either Ok with empty string or still Ok
        assert!(result.is_ok());
    }

    #[test]
    fn test_branch_parsing_from_status_header() {
        // Simple branch name
        let output = "## feature-x\n";
        let state = GitState::parse_porcelain(output).unwrap();
        assert_eq!(state.branch, "feature-x");

        // Branch with remote tracking
        let output = "## develop...origin/develop [ahead 2]\n";
        let state = GitState::parse_porcelain(output).unwrap();
        assert_eq!(state.branch, "develop");

        // Detached HEAD
        let output = "## HEAD (no branch)\n";
        let state = GitState::parse_porcelain(output).unwrap();
        assert_eq!(state.branch, "HEAD (no branch)");
    }

    #[test]
    fn test_git_file_status_serialization() {
        let status = GitFileStatus {
            path: "src/main.rs".to_string(),
            status: FileStatus::Modified,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("src/main.rs"));
        assert!(json.contains("Modified"));

        // Round-trip
        let deserialized: GitFileStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.path, "src/main.rs");
        assert_eq!(deserialized.status, FileStatus::Modified);
    }

    #[test]
    fn test_git_status_command_registered() {
        use crate::commands::CommandRegistry;

        let reg = CommandRegistry::new();
        let cmds = reg.list_commands();
        let names: Vec<_> = cmds.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"git"), "Expected 'git' command to be registered, found: {names:?}");
    }
}

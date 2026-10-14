//! Bookmark history as a git repo: every snapshot is a commit of `bookmarks.json` +
//! `bookmarks.html`, so any point can be diffed, restored, or pushed to a remote.
//! Each machine pushes to its own branch (its hostname), so several can share one remote.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::tree::{to_netscape_html, Node};

pub struct History {
    dir: PathBuf,
}

impl History {
    /// Open (creating and `git init`-ing if needed) the history repo at `dir`.
    ///
    /// # Errors
    /// Filesystem failure or git missing.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let h = Self { dir: dir.to_path_buf() };
        if !dir.join(".git").exists() {
            h.git(&["init", "-q", "-b", "main"])?;
            if h.git(&["config", "user.email"]).unwrap_or_default().is_empty() {
                h.git(&["config", "user.name", "Smooth"])?;
                h.git(&["config", "user.email", "smooth@localhost"])?;
            }
        }
        Ok(h)
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("git").arg("-C").arg(&self.dir).args(args).output().context("run git")?;
        if !out.status.success() {
            bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Commit the tree. `Ok(None)` when nothing changed since the last snapshot.
    ///
    /// # Errors
    /// Write or git failure. A failed auto-push is reported in the returned note, not as an error.
    pub fn snapshot(&self, tree: &Node, message: &str) -> Result<Option<String>> {
        std::fs::write(self.dir.join("bookmarks.json"), serde_json::to_string_pretty(tree)? + "\n")?;
        std::fs::write(self.dir.join("bookmarks.html"), to_netscape_html(tree))?;
        self.git(&["add", "-A"])?;
        if Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(["diff", "--cached", "--quiet"])
            .status()?
            .success()
        {
            return Ok(None);
        }
        self.git(&["commit", "-q", "-m", message])?;
        let sha = self.git(&["rev-parse", "--short", "HEAD"])?;
        if self.auto_push() {
            if let Err(e) = self.push() {
                return Ok(Some(format!("{sha} (auto-push failed: {e})")));
            }
        }
        Ok(Some(sha))
    }

    /// `git log` one line per snapshot, newest first.
    ///
    /// # Errors
    /// git failure.
    pub fn log(&self, limit: usize) -> Result<String> {
        if self.git(&["rev-parse", "--verify", "-q", "HEAD"]).is_err() {
            return Ok(String::new());
        }
        self.git(&["log", &format!("-{limit}"), "--date=format:%Y-%m-%d %H:%M", "--format=%h  %ad  %s"])
    }

    /// The tree as of `rev` (a sha, `HEAD~3`, `origin/<host>`, …).
    ///
    /// # Errors
    /// Unknown revision or unreadable snapshot.
    pub fn at(&self, rev: &str) -> Result<Node> {
        let text = self.git(&["show", &format!("{rev}:bookmarks.json")])?;
        serde_json::from_str(&text).context("snapshot isn't a bookmark tree")
    }

    /// Point `origin` at `url`; with `auto_push`, every snapshot is pushed.
    ///
    /// # Errors
    /// git failure.
    pub fn set_remote(&self, url: &str, auto_push: bool) -> Result<()> {
        if self.git(&["remote", "get-url", "origin"]).is_ok() {
            self.git(&["remote", "set-url", "origin", url])?;
        } else {
            self.git(&["remote", "add", "origin", url])?;
        }
        self.git(&["config", "smooth.autopush", if auto_push { "true" } else { "false" }]).map(drop)
    }

    fn auto_push(&self) -> bool {
        self.git(&["config", "--bool", "smooth.autopush"]).is_ok_and(|v| v == "true")
    }

    /// The branch this machine pushes to.
    pub fn branch() -> String {
        let host = sysinfo::System::host_name().unwrap_or_else(|| "machine".into());
        let host: String = host
            .split('.')
            .next()
            .unwrap_or("machine")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
            .collect();
        if host.is_empty() {
            "machine".into()
        } else {
            host.to_lowercase()
        }
    }

    /// Push history to `origin/<hostname>`.
    ///
    /// # Errors
    /// No remote, or the push failed.
    pub fn push(&self) -> Result<String> {
        self.git(&["remote", "get-url", "origin"])
            .context("no remote: run `th chrome bookmarks history remote <url>` first")?;
        let branch = Self::branch();
        self.git(&["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")])?;
        Ok(branch)
    }

    /// Fetch every machine's history from `origin`.
    ///
    /// # Errors
    /// No remote, or the fetch failed.
    pub fn fetch(&self) -> Result<String> {
        self.git(&["fetch", "-q", "origin"])?;
        self.git(&["branch", "-r", "--format=%(refname:short)"])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;
    use crate::chrome::tree::tests::sample;
    use crate::chrome::tree::{upsert, Api};

    #[test]
    fn snapshots_commit_only_changes_and_restore_by_rev() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(&dir.path().join("history")).unwrap();
        assert!(h.log(5).unwrap().is_empty());

        let mut t = sample();
        let first = h.snapshot(&t.root, "first").unwrap().expect("first snapshot commits");
        assert!(h.snapshot(&t.root, "same").unwrap().is_none(), "unchanged tree makes no commit");
        upsert(&mut t, "New", "n", "https://n.example", false).unwrap();
        h.snapshot(&t.tree().unwrap(), "added n").unwrap().unwrap();

        let log = h.log(10).unwrap();
        assert!(log.lines().next().unwrap().ends_with("added n"));
        assert_eq!(log.lines().count(), 2);
        assert_eq!(h.at(&first).unwrap(), sample().root);
        assert!(h.at("nope").is_err());
        assert!(std::fs::read_to_string(dir.path().join("history/bookmarks.html"))
            .unwrap()
            .contains("https://n.example"));
    }

    #[test]
    fn pushes_to_a_per_machine_branch_and_fetches() {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        assert!(Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap().success());
        let h = History::open(&dir.path().join("a")).unwrap();
        assert!(h.push().unwrap_err().to_string().contains("no remote"));
        h.set_remote(remote.to_str().unwrap(), true).unwrap();
        let sha = h.snapshot(&sample().root, "first").unwrap().unwrap();
        assert!(!sha.contains("failed"), "{sha}");

        let other = History::open(&dir.path().join("b")).unwrap();
        other.set_remote(remote.to_str().unwrap(), false).unwrap();
        let branches = other.fetch().unwrap();
        let branch = format!("origin/{}", History::branch());
        assert!(branches.lines().any(|b| b == branch), "{branches}");
        assert_eq!(other.at(&branch).unwrap(), sample().root);
    }

    #[test]
    fn branch_name_is_git_safe() {
        let b = History::branch();
        assert!(!b.is_empty());
        assert!(b.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "{b}");
    }
}

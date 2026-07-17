//! Session-scoped current working directory, confined under a fixed root.
//!
//! Big Smooth boots with a broad workspace root (`SMOOTH_WORKSPACE`, e.g.
//! `~/dev`). This lets a *conversation* scope itself to a subdirectory at
//! runtime — a `/cd` from the web UI (via the daemon's `/api/session/cwd`
//! route) or the agent's own `cd` tool — so the file tools operate under that
//! narrower directory for the rest of the conversation.
//!
//! The store is keyed by the operator's per-turn `conversation_id` (threaded
//! through `ToolProviderContext`), so two conversations get independent cwds
//! and a conversation's cwd survives across turns. Unset ⇒ the root.
//!
//! **Confinement is load-bearing.** A cwd can only ever be an *existing
//! directory under the root* — `set` canonicalizes the target and the root and
//! rejects anything that isn't lexically AND canonically inside the root, so
//! `..` traversal and symlink escapes both fail. `/cd /` or `/cd ~someone-else`
//! can never point Big Smooth outside its sandbox.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::path::lexical_normalize;

/// Per-conversation current working directory, confined under `root`. Cheap to
/// clone (the map is behind an `Arc`) so the ToolProvider, the `cd` tool, and
/// the daemon's HTTP route all share one store.
#[derive(Clone)]
pub struct SessionCwd {
    root: PathBuf,
    map: Arc<Mutex<HashMap<String, PathBuf>>>,
}

impl SessionCwd {
    /// A store rooted at `root`. The root is canonicalized (falling back to a
    /// lexical normalize when it doesn't exist yet) so confinement checks and
    /// the symlink-escape guard compare like-for-like.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        // Canonicalize the root so confinement checks + the symlink guard
        // compare like-for-like; fall back to the given path when it doesn't
        // exist yet (canonicalize borrows first, so the `Err` arm can move it).
        let root = root.canonicalize().unwrap_or(root);
        Self {
            root,
            map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The workspace root — the cwd every session falls back to.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The session's cwd, or the root when unset.
    #[must_use]
    pub fn get(&self, session: &str) -> PathBuf {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .cloned()
            .unwrap_or_else(|| self.root.clone())
    }

    /// Reset the session back to the root.
    pub fn reset(&self, session: &str) {
        self.map.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session);
    }

    /// Resolve `path` and, if it is an existing directory confined under the
    /// root, set it as the session's cwd. Relative paths resolve against the
    /// session's current cwd; absolute paths are taken as-is. An empty path or
    /// `~` resets to the root. Returns the resolved (canonical) cwd.
    ///
    /// # Errors
    /// The path escapes the root, doesn't exist, or isn't a directory.
    pub fn set(&self, session: &str, path: &str) -> anyhow::Result<PathBuf> {
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "~" {
            self.reset(session);
            return Ok(self.root.clone());
        }

        // 1. Lexical gate: resolve against the current cwd, collapse `.`/`..`,
        //    and require the result to be under the root before we touch disk.
        let current = self.get(session);
        let requested = Path::new(trimmed);
        let joined = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            current.join(requested)
        };
        let normalized = lexical_normalize(&joined);
        if !normalized.starts_with(&self.root) {
            anyhow::bail!("path `{trimmed}` is outside the workspace root {}", self.root.display());
        }

        // 2. Canonical gate (load-bearing): the target must exist, be a
        //    directory, and — after following symlinks — STILL be under the
        //    root. This is what defeats a symlink pointing out of the sandbox.
        let canonical = normalized
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("directory does not exist: {}", normalized.display()))?;
        if !canonical.is_dir() {
            anyhow::bail!("not a directory: {}", canonical.display());
        }
        if !canonical.starts_with(&self.root) {
            anyhow::bail!("path `{trimmed}` resolves outside the workspace root {}", self.root.display());
        }

        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.to_string(), canonical.clone());
        Ok(canonical)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    /// A root with `a/b` and a plain file `f.txt`, plus a symlink `esc → /` for
    /// the escape test. Returns (tempdir, canonical root).
    fn fixture() -> (tempfile::TempDir, SessionCwd) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a/b")).unwrap();
        std::fs::write(tmp.path().join("f.txt"), "x").unwrap();
        let cwd = SessionCwd::new(tmp.path().to_path_buf());
        (tmp, cwd)
    }

    #[test]
    fn unset_session_returns_root() {
        let (_tmp, cwd) = fixture();
        assert_eq!(cwd.get("s1"), cwd.root());
    }

    #[test]
    fn valid_subdir_sets_cwd() {
        let (_tmp, cwd) = fixture();
        let set = cwd.set("s1", "a/b").unwrap();
        assert_eq!(set, cwd.root().join("a/b").canonicalize().unwrap());
        assert_eq!(cwd.get("s1"), set, "cwd persists across get calls");
    }

    #[test]
    fn relative_path_resolves_against_current_cwd() {
        let (_tmp, cwd) = fixture();
        cwd.set("s1", "a").unwrap();
        // `b` is relative to the session's current cwd (`a`), not the root.
        let set = cwd.set("s1", "b").unwrap();
        assert_eq!(set, cwd.root().join("a/b").canonicalize().unwrap());
    }

    #[test]
    fn absolute_path_inside_root_ok() {
        let (_tmp, cwd) = fixture();
        let abs = cwd.root().join("a").to_string_lossy().into_owned();
        assert!(cwd.set("s1", &abs).is_ok());
    }

    #[test]
    fn nonexistent_path_rejected() {
        let (_tmp, cwd) = fixture();
        assert!(cwd.set("s1", "nope").is_err());
        assert_eq!(cwd.get("s1"), cwd.root(), "a rejected set leaves the cwd unchanged");
    }

    #[test]
    fn file_not_dir_rejected() {
        let (_tmp, cwd) = fixture();
        let err = cwd.set("s1", "f.txt").unwrap_err().to_string();
        assert!(err.contains("not a directory"), "{err}");
    }

    #[test]
    fn dotdot_escape_rejected() {
        let (_tmp, cwd) = fixture();
        for esc in ["..", "../..", "a/../../elsewhere", "/etc"] {
            assert!(cwd.set("s1", esc).is_err(), "{esc} should be rejected");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_rejected() {
        let (tmp, cwd) = fixture();
        // A symlink INSIDE the root that points OUT of it must not be a valid cwd.
        std::os::unix::fs::symlink("/tmp", tmp.path().join("esc")).unwrap();
        let err = cwd.set("s1", "esc").unwrap_err().to_string();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn empty_or_tilde_resets_to_root() {
        let (_tmp, cwd) = fixture();
        cwd.set("s1", "a/b").unwrap();
        assert_eq!(cwd.set("s1", "").unwrap(), cwd.root());
        cwd.set("s1", "a/b").unwrap();
        assert_eq!(cwd.set("s1", "~").unwrap(), cwd.root());
        assert_eq!(cwd.get("s1"), cwd.root());
    }

    #[test]
    fn sessions_are_independent() {
        let (_tmp, cwd) = fixture();
        cwd.set("s1", "a/b").unwrap();
        cwd.set("s2", "a").unwrap();
        assert_eq!(cwd.get("s1"), cwd.root().join("a/b").canonicalize().unwrap());
        assert_eq!(cwd.get("s2"), cwd.root().join("a").canonicalize().unwrap());
        assert_eq!(cwd.get("s3"), cwd.root(), "an untouched session is still the root");
    }
}

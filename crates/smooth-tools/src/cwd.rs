//! Session-scoped current working directory, confined under explicit roots.
//!
//! Big Smooth boots with one workspace root (`SMOOTH_WORKSPACE`) and may be
//! given additional, explicit roots via `SMOOTH_WORKSPACES`. A conversation
//! defaults to the primary root and can `/cd` to an allowed directory using
//! the web UI route or the agent's `cd` tool.
//!
//! The store is keyed by the operator's per-turn `conversation_id` (threaded
//! through `ToolProviderContext`), so two conversations get independent cwds
//! and a conversation's cwd survives across turns. Unset ⇒ the root.
//!
//! **Confinement is load-bearing.** A cwd can only ever be an *existing
//! directory under an allowed root* — `set` canonicalizes the target and
//! rejects anything that isn't lexically and canonically inside an allowed root, so
//! `..` traversal and symlink escapes both fail. `/cd /` or `/cd ~someone-else`
//! can never point Big Smooth outside its configured workspace roots.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::path::lexical_normalize;

/// Per-conversation current working directory, confined under `root`. Cheap to
/// clone (the map is behind an `Arc`) so the ToolProvider, the `cd` tool, and
/// the daemon's HTTP route all share one store.
#[derive(Clone)]
pub struct SessionCwd {
    root: PathBuf,
    /// Operator-configured roots shared by sessions (primary + explicit env config).
    allowed_roots: Arc<Mutex<Vec<PathBuf>>>,
    /// Roots explicitly opened from a coding client, scoped to one conversation.
    session_roots: Arc<Mutex<HashMap<String, Vec<PathBuf>>>>,
    session_paths: Arc<Mutex<HashMap<String, OsString>>>,
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
            allowed_roots: Arc::new(Mutex::new(vec![root.clone()])),
            session_roots: Arc::new(Mutex::new(HashMap::new())),
            session_paths: Arc::new(Mutex::new(HashMap::new())),
            root,
            map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Explicitly allow another workspace for sessions that need to inspect
    /// sibling repositories. The directory must exist when it is configured.
    pub fn add_root(&mut self, root: &Path) -> anyhow::Result<()> {
        let canonical = canonical_directory(root)?;
        let mut roots = self.allowed_roots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !roots.contains(&canonical) {
            roots.push(canonical);
        }
        Ok(())
    }

    /// Add a directory to one conversation after the user explicitly opens it.
    pub fn add_session_root(&self, session: &str, root: &Path) -> anyhow::Result<PathBuf> {
        let canonical = canonical_directory(root)?;
        {
            let mut session_roots = self.session_roots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let roots = session_roots.entry(session.to_string()).or_default();
            if !roots.contains(&canonical) {
                roots.push(canonical.clone());
            }
        }
        // On the first turn (or after a daemon restart), initialize the cwd to
        // the repository the user launched th code from. Preserve an explicit
        // `/cd` selection on later turns.
        let mut cwd = self.map.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cwd.entry(session.to_string()).or_insert_with(|| canonical.clone());
        Ok(canonical)
    }

    fn roots_for(&self, session: &str) -> Vec<PathBuf> {
        let mut roots = self.allowed_roots.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(extra) = self.session_roots.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session) {
            roots.extend(extra.iter().cloned());
        }
        roots
    }

    /// Record the attached user's PATH for shell tools in this conversation.
    pub fn set_user_path(&self, session: &str, path: &str) {
        self.session_paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.to_string(), OsString::from(path));
    }

    /// The attached user's PATH, when this conversation came from th code.
    #[must_use]
    pub fn user_path(&self, session: &str) -> Option<OsString> {
        self.session_paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .cloned()
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

    /// Resolve `path` and set it as the session's cwd if it belongs to a root.
    ///
    /// # Errors
    /// The path escapes all configured roots, doesn't exist, or isn't a directory.
    pub fn set(&self, session: &str, path: &str) -> anyhow::Result<PathBuf> {
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "~" {
            self.reset(session);
            return Ok(self.root.clone());
        }

        let current = self.get(session);
        let requested = Path::new(trimmed);
        let joined = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            current.join(requested)
        };
        let normalized = lexical_normalize(&joined);
        let roots = self.roots_for(session);
        let canonical = normalized
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("directory does not exist: {}", normalized.display()))?;
        if !roots.iter().any(|root| canonical.starts_with(root)) {
            anyhow::bail!("path `{trimmed}` resolves outside this session's opened workspaces; use /workspace add <path> first");
        }
        if !canonical.is_dir() {
            anyhow::bail!("not a directory: {}", canonical.display());
        }

        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.to_string(), canonical.clone());
        Ok(canonical)
    }
}

fn canonical_directory(root: &Path) -> anyhow::Result<PathBuf> {
    let canonical = root
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot add workspace root `{}`: {e}", root.display()))?;
    if !canonical.is_dir() {
        anyhow::bail!("workspace root is not a directory: {}", canonical.display());
    }
    Ok(canonical)
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
    fn can_cd_to_an_explicitly_allowed_external_workspace() {
        let (_primary, cwd) = fixture();
        let sibling = tempfile::tempdir().unwrap();
        let mut cwd = cwd;
        cwd.add_root(sibling.path()).unwrap();
        let resolved = cwd.set("s1", sibling.path().to_str().unwrap()).unwrap();
        assert_eq!(resolved, sibling.path().canonicalize().unwrap());
    }

    #[test]
    fn explicitly_opened_root_is_scoped_to_its_conversation() {
        let (_primary, cwd) = fixture();
        let sibling = tempfile::tempdir().unwrap();
        cwd.add_session_root("s1", sibling.path()).unwrap();
        assert_eq!(cwd.get("s1"), sibling.path().canonicalize().unwrap(), "opening a workspace sets the first cwd");
        assert!(cwd.set("s1", sibling.path().to_str().unwrap()).is_ok());
        assert!(cwd.set("s2", sibling.path().to_str().unwrap()).is_err());
        assert_eq!(cwd.get("s2"), cwd.root());
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

//! Workspace path confinement for filesystem tools.
//!
//! Every tool that touches the filesystem MUST route user-supplied paths
//! through [`resolve_workspace_path`]. Lexical traversal is rejected, and
//! existing path components are canonicalized to prevent symlinks from
//! escaping the selected workspace. Nonexistent write targets are checked
//! against their nearest existing ancestor. This is distinct from the `bash`
//! subprocess sandbox; that has platform-specific protections and does not
//! replace this per-tool root check.

use std::path::{Component, Path, PathBuf};

/// Resolve `rel` against the workspace `base`, confining the result to `base`.
///
/// Accepts a relative path (joined onto `base`) OR an absolute path that
/// resolves inside `base`. Rejects empty paths and any path — relative or
/// absolute — that escapes `base` after collapsing `.` / `..` or following
/// existing symlinks.
///
/// Absolute-within-workspace is allowed because the agent naturally emits
/// absolute paths when the user names one (e.g. `~/dev/smooai/x` →
/// `/Users/you/dev/smooai/x`); rejecting them outright made tool-using turns
/// flail and give up (th-c89c2a). Confinement is unchanged: an absolute path
/// outside `base` still fails the containment check, exactly as a relative
/// `../escape` does. Symlinks are resolved only for containment validation;
/// the returned path remains the lexical path for user-facing tool output.
///
/// # Errors
/// Returns an error if `rel` is empty or escapes the workspace.
pub fn resolve_workspace_path(base: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    if rel.is_empty() {
        anyhow::bail!("empty path");
    }
    let base_norm = lexical_normalize(base);
    let requested = Path::new(rel);
    let normalized = if requested.is_absolute() {
        lexical_normalize(requested)
    } else {
        lexical_normalize(&base_norm.join(requested))
    };

    if !normalized.starts_with(&base_norm) {
        anyhow::bail!(
            "path `{rel}` is outside the configured workspace root `{}`; filesystem tools cannot cross that boundary. To work across repositories, use `/workspace add <directory>` in th code",
            base_norm.display()
        );
    }

    ensure_path_stays_within(&base_norm, &normalized)
}

/// Validate existing path components without requiring the final target to
/// exist. Canonicalizing the nearest existing ancestor catches both symlink
/// reads and writes through a symlink to an external directory.
fn ensure_path_stays_within(base: &Path, path: &Path) -> anyhow::Result<PathBuf> {
    let canonical_base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let mut ancestor = path.to_path_buf();
    let mut missing = Vec::new();

    loop {
        if let Ok(canonical) = ancestor.canonicalize() {
            let resolved = missing.iter().rev().fold(canonical, |resolved, part| resolved.join(part));
            if resolved.starts_with(&canonical_base) {
                return Ok(resolved);
            }
            anyhow::bail!("path resolves outside the configured workspace root `{}`", canonical_base.display());
        }

        if let Ok(metadata) = std::fs::symlink_metadata(&ancestor) {
            if metadata.file_type().is_symlink() {
                anyhow::bail!("path traverses a symlink that cannot be resolved inside the workspace");
            }
        }

        let Some(name) = ancestor.file_name() else {
            anyhow::bail!("cannot resolve path ancestry inside the workspace");
        };
        missing.push(name.to_os_string());
        if !ancestor.pop() {
            anyhow::bail!("cannot resolve path ancestry inside the workspace");
        }
    }
}

/// Collapse `.` and `..` components lexically. Does NOT follow symlinks or
/// require the path to exist. A leading `..` that can't be popped is kept so
/// the prefix check in [`resolve_workspace_path`] catches the escape.
pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component);
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn base() -> PathBuf {
        PathBuf::from("/work/space")
    }

    #[test]
    fn resolves_a_simple_relative_path() {
        let p = resolve_workspace_path(&base(), "src/main.rs").unwrap();
        assert_eq!(p, PathBuf::from("/work/space/src/main.rs"));
    }

    #[test]
    fn allows_interior_dotdot_that_stays_inside() {
        let p = resolve_workspace_path(&base(), "src/../README.md").unwrap();
        assert_eq!(p, PathBuf::from("/work/space/README.md"));
    }

    #[test]
    fn allows_leading_dot_slash() {
        let p = resolve_workspace_path(&base(), "./Cargo.toml").unwrap();
        assert_eq!(p, PathBuf::from("/work/space/Cargo.toml"));
    }

    #[test]
    fn rejects_empty() {
        assert!(resolve_workspace_path(&base(), "").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_for_existing_and_new_paths() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("link")).unwrap();

        for path in ["link/secret.txt", "link/new-file.txt"] {
            assert!(resolve_workspace_path(workspace.path(), path).is_err(), "{path}");
        }
    }

    #[test]
    fn allows_absolute_path_inside_workspace() {
        // The agent naturally emits absolute paths when the user names one.
        // An absolute path INSIDE the workspace is allowed and resolves to itself.
        let p = resolve_workspace_path(&base(), "/work/space/x").unwrap();
        assert_eq!(p, PathBuf::from("/work/space/x"));
        let p = resolve_workspace_path(&base(), "/work/space/src/main.rs").unwrap();
        assert_eq!(p, PathBuf::from("/work/space/src/main.rs"));
        // The base itself.
        let p = resolve_workspace_path(&base(), "/work/space").unwrap();
        assert_eq!(p, base());
    }

    #[test]
    fn rejects_absolute_paths_outside_workspace() {
        // Absolute paths OUTSIDE the workspace are still rejected — confinement
        // is preserved. `//x` normalizes to `/x`, also outside.
        for abs in ["/etc/passwd", "//x", "/work", "/work/spaceother"] {
            let err = resolve_workspace_path(&base(), abs).unwrap_err();
            assert!(err.to_string().contains("outside"), "{abs}: {err}");
            assert!(err.to_string().contains("/workspace add"), "{abs}: {err}");
        }
    }

    #[test]
    fn rejects_absolute_dotdot_escape_from_inside() {
        // An absolute path that starts inside but climbs out via `..` is rejected.
        for esc in ["/work/space/../../etc/passwd", "/work/space/../space-evil/x"] {
            let err = resolve_workspace_path(&base(), esc).unwrap_err();
            assert!(err.to_string().contains("outside"), "{esc}: {err}");
        }
    }

    #[test]
    fn rejects_escape_via_dotdot() {
        for esc in ["../secret", "../../etc/passwd", "a/../../b", "src/../../outside"] {
            let err = resolve_workspace_path(&base(), esc).unwrap_err();
            assert!(err.to_string().contains("outside"), "{esc}: {err}");
        }
    }

    #[test]
    fn rejects_sneaky_sibling_prefix() {
        // `/work/space-evil` shares a string prefix with `/work/space` but is a
        // different directory; the component-wise starts_with must reject it.
        let err = resolve_workspace_path(&base(), "../space-evil/x").unwrap_err();
        assert!(err.to_string().contains("outside"), "{err}");
    }

    #[test]
    fn dotdot_to_exactly_base_is_allowed() {
        // `src/..` normalizes back to base itself, which is inside base.
        let p = resolve_workspace_path(&base(), "src/..").unwrap();
        assert_eq!(p, base());
    }
}

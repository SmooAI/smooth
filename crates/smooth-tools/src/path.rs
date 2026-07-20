//! Workspace path confinement — the security floor for every filesystem tool.
//!
//! Every tool that touches the filesystem MUST route user-supplied paths
//! through [`resolve_workspace_path`] so a prompt-injected agent can't read or
//! write outside the workspace. The confinement is **lexical** (no
//! `canonicalize`): we never follow symlinks (an attacker could symlink
//! `workspace/link → /etc` and write through it) and we don't require the path
//! to exist (writes target new files).
//!
//! NOTE: this is a defense-in-depth layer, not the whole story. Per EPIC
//! th-c89c2a, the load-bearing boundary is the kernel OS-sandbox added in Phase
//! 3; this lexical check is the cheap first gate.

use std::path::{Component, Path, PathBuf};

/// Resolve `rel` against the workspace `base`, confining the result to `base`.
///
/// Accepts a relative path (joined onto `base`) OR an absolute path that
/// lexically resolves **inside** `base`. Rejects empty paths and any path —
/// relative or absolute — that escapes `base` after collapsing `.` / `..`.
///
/// Absolute-within-workspace is allowed because the agent naturally emits
/// absolute paths when the user names one (e.g. `~/dev/smooai/x` →
/// `/Users/you/dev/smooai/x`); rejecting them outright made tool-using turns
/// flail and give up (th-c89c2a). Confinement is unchanged: an absolute path
/// outside `base` still fails the `starts_with` check, exactly as a relative
/// `../escape` does. The lexical (no-`canonicalize`) rule and the kernel
/// OS-sandbox remain the load-bearing symlink boundary.
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
            "path `{rel}` is outside the workspace (resolved to {}, which is not under {})",
            normalized.display(),
            base_norm.display()
        );
    }

    Ok(normalized)
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

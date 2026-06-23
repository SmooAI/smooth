//! Workspace path confinement — the security floor for every filesystem tool.
//!
//! Replicated from `smooth-operative/src/tool_support.rs::resolve_workspace_path`.
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
/// Rejects empty paths, absolute paths, and any path that escapes `base` after
/// lexically collapsing `.` / `..`.
///
/// # Errors
/// Returns an error if `rel` is empty, absolute, or escapes the workspace.
pub fn resolve_workspace_path(base: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    if rel.is_empty() {
        anyhow::bail!("empty path");
    }
    let requested = Path::new(rel);
    if requested.is_absolute() {
        anyhow::bail!("absolute path `{rel}` is not allowed — all paths must be relative to the workspace");
    }

    let base_norm = lexical_normalize(base);
    let normalized = lexical_normalize(&base_norm.join(requested));

    if !normalized.starts_with(&base_norm) {
        anyhow::bail!(
            "path `{rel}` escapes the workspace (resolved to {}, outside {})",
            normalized.display(),
            base_norm.display()
        );
    }

    Ok(normalized)
}

/// Collapse `.` and `..` components lexically. Does NOT follow symlinks or
/// require the path to exist. A leading `..` that can't be popped is kept so
/// the prefix check in [`resolve_workspace_path`] catches the escape.
fn lexical_normalize(path: &Path) -> PathBuf {
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
    fn rejects_absolute_paths() {
        for abs in ["/etc/passwd", "/work/space/x", "//x"] {
            let err = resolve_workspace_path(&base(), abs).unwrap_err();
            assert!(err.to_string().contains("absolute"), "{abs}: {err}");
        }
    }

    #[test]
    fn rejects_escape_via_dotdot() {
        for esc in ["../secret", "../../etc/passwd", "a/../../b", "src/../../outside"] {
            let err = resolve_workspace_path(&base(), esc).unwrap_err();
            assert!(err.to_string().contains("escapes"), "{esc}: {err}");
        }
    }

    #[test]
    fn rejects_sneaky_sibling_prefix() {
        // `/work/space-evil` shares a string prefix with `/work/space` but is a
        // different directory; the component-wise starts_with must reject it.
        let err = resolve_workspace_path(&base(), "../space-evil/x").unwrap_err();
        assert!(err.to_string().contains("escapes"), "{err}");
    }

    #[test]
    fn dotdot_to_exactly_base_is_allowed() {
        // `src/..` normalizes back to base itself, which is inside base.
        let p = resolve_workspace_path(&base(), "src/..").unwrap();
        assert_eq!(p, base());
    }
}

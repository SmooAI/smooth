//! Shared filesystem walking — ripgrep's `ignore` walker, pruned of the heavy
//! directories that make a naive walk stall on a real project tree.
//!
//! `list_files`/`grep` are built on `ignore::WalkBuilder` (libripgrep's
//! gitignore-aware walker), so per-file they're already fast. The trap is
//! *traversal*: with `.hidden(false)` the walker descends into `.git/` object
//! stores (tens of thousands of loose objects), and `.gitignore` doesn't always
//! cover `node_modules`/`target`. On a 100k+-file tree that walk dominates. We
//! prune those dirs by name so the walk stays bounded while still surfacing
//! ordinary dotfiles (e.g. `.env`, `.envrc`).

use std::path::Path;

use ignore::{Walk, WalkBuilder};

/// Directory names always pruned: version-control internals and
/// build/dependency caches. They explode entry counts and never hold the
/// source a user is asking about. `.git` itself is never in `.gitignore`, and
/// `node_modules`/`target` only are when the repo bothered to list them.
const PRUNE_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".next",
    ".turbo",
    ".gradle",
];

/// Whether a walked entry is a directory whose name is in [`PRUNE_DIRS`].
fn is_pruned_dir(entry: &ignore::DirEntry) -> bool {
    entry.file_type().is_some_and(|t| t.is_dir()) && entry.file_name().to_str().is_some_and(|n| PRUNE_DIRS.contains(&n))
}

/// A `.gitignore`-respecting walker rooted at `root`, pruned of [`PRUNE_DIRS`].
/// Hidden files are listed (dotfiles are often what a user wants), but the heavy
/// hidden/build dirs are skipped so the traversal can't stall.
#[must_use]
pub fn pruned_walk(root: &Path) -> Walk {
    WalkBuilder::new(root).hidden(false).filter_entry(|e| !is_pruned_dir(e)).build()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pruned_walk_skips_git_and_node_modules_but_keeps_dotfiles() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::write(root.join(".git/objects/deadbeef"), "obj").unwrap();
        std::fs::create_dir_all(root.join("node_modules/left-pad")).unwrap();
        std::fs::write(root.join("node_modules/left-pad/index.js"), "x").unwrap();
        std::fs::write(root.join(".envrc"), "export X=1").unwrap();
        std::fs::write(root.join("main.rs"), "fn main(){}").unwrap();

        let names: Vec<String> = pruned_walk(root)
            .flatten()
            .filter_map(|e| e.path().strip_prefix(root).ok().map(|p| p.display().to_string()))
            .collect();

        assert!(names.iter().any(|n| n == "main.rs"), "source file listed: {names:?}");
        assert!(names.iter().any(|n| n == ".envrc"), "ordinary dotfile still listed: {names:?}");
        assert!(!names.iter().any(|n| n.contains(".git/")), "must not descend into .git: {names:?}");
        assert!(
            !names.iter().any(|n| n.contains("node_modules/")),
            "must not descend into node_modules: {names:?}"
        );
    }
}

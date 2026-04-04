//! File tree browser with .gitignore-aware walking.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Maximum directory depth to walk.
const MAX_DEPTH: usize = 4;

/// Maximum number of entries to collect.
const MAX_ENTRIES: usize = 500;

/// A single entry in the file tree.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Full path to the file or directory.
    pub path: PathBuf,
    /// Display name (file/directory name only).
    pub name: String,
    /// Nesting depth relative to the root (0 = direct child).
    pub depth: usize,
    /// Whether this entry is a directory.
    pub is_dir: bool,
}

/// A navigable file tree rooted at a directory.
#[derive(Debug)]
pub struct FileTree {
    /// Root directory of the tree.
    pub root: PathBuf,
    /// Flat list of entries sorted dirs-first, then alphabetically.
    pub entries: Vec<FileEntry>,
    /// Index of the currently selected entry.
    pub selected: usize,
    /// Scroll offset for windowed display.
    pub scroll_offset: usize,
}

impl FileTree {
    /// Build a file tree from the given directory root.
    ///
    /// Uses the `ignore` crate's `WalkBuilder` to respect `.gitignore` rules.
    /// Directories are sorted before files at each level, both sorted alphabetically.
    /// Depth is limited to [`MAX_DEPTH`] and entries capped at [`MAX_ENTRIES`].
    ///
    /// # Errors
    /// Returns an error if the root path does not exist or is not a directory.
    pub fn from_dir(root: &Path) -> anyhow::Result<Self> {
        if !root.is_dir() {
            anyhow::bail!("not a directory: {}", root.display());
        }

        let walker = WalkBuilder::new(root)
            .max_depth(Some(MAX_DEPTH))
            .hidden(true) // respect hidden files setting (skip dotfiles)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .sort_by_file_path(|a, b| {
                let a_is_dir = a.is_dir();
                let b_is_dir = b.is_dir();
                match (a_is_dir, b_is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.file_name().cmp(&b.file_name()),
                }
            })
            .build();

        let mut entries = Vec::new();

        for result in walker {
            let Ok(entry) = result else { continue };

            // Skip the root directory itself.
            if entry.path() == root {
                continue;
            }

            let depth = entry.depth().saturating_sub(1);
            let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
            let name = entry.path().file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());

            if is_dir {
                // Append trailing slash for display.
                entries.push(FileEntry {
                    path: entry.path().to_path_buf(),
                    name: format!("{name}/"),
                    depth,
                    is_dir,
                });
            } else {
                entries.push(FileEntry {
                    path: entry.path().to_path_buf(),
                    name,
                    depth,
                    is_dir,
                });
            }

            if entries.len() >= MAX_ENTRIES {
                break;
            }
        }

        Ok(Self {
            root: root.to_path_buf(),
            entries,
            selected: 0,
            scroll_offset: 0,
        })
    }

    /// Return the path of the currently selected entry, if any.
    pub fn selected_path(&self) -> Option<&Path> {
        self.entries.get(self.selected).map(|e| e.path.as_path())
    }

    /// Move the selection up by one entry.
    pub fn select_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            // Keep selected item visible.
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            }
        }
    }

    /// Move the selection down by one entry.
    pub fn select_down(&mut self) {
        if !self.entries.is_empty() && self.selected < self.entries.len() - 1 {
            self.selected += 1;
        }
    }

    /// Return a windowed slice of entries for the given viewport height.
    ///
    /// Adjusts `scroll_offset` so that the selected item is always visible.
    pub fn visible_entries(&mut self, height: usize) -> &[FileEntry] {
        if self.entries.is_empty() || height == 0 {
            return &[];
        }

        // Ensure selected is visible within the window.
        if self.selected >= self.scroll_offset + height {
            self.scroll_offset = self.selected + 1 - height;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }

        let end = (self.scroll_offset + height).min(self.entries.len());
        &self.entries[self.scroll_offset..end]
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    /// Helper: create a small directory tree for testing.
    fn make_test_tree() -> TempDir {
        let tmp = TempDir::new().expect("create tempdir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::write(root.join("src/main.rs"), "fn main() {}").expect("write main.rs");
        fs::write(root.join("src/lib.rs"), "").expect("write lib.rs");
        fs::create_dir_all(root.join("tests")).expect("mkdir tests");
        fs::write(root.join("tests/integration.rs"), "").expect("write integration.rs");
        fs::write(root.join("Cargo.toml"), "[package]").expect("write Cargo.toml");
        fs::write(root.join("README.md"), "# Hi").expect("write README.md");

        tmp
    }

    #[test]
    fn test_from_dir_lists_files_respects_gitignore() {
        let tmp = make_test_tree();
        let root = tmp.path();

        // The ignore crate needs a git repo to honour .gitignore.
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .expect("git init");

        // Create a .gitignore that ignores *.md files.
        fs::write(root.join(".gitignore"), "*.md\n").expect("write .gitignore");

        let tree = FileTree::from_dir(root).expect("from_dir");
        let names: Vec<&str> = tree.entries.iter().map(|e| e.name.as_str()).collect();

        // README.md should be excluded by .gitignore.
        assert!(!names.contains(&"README.md"), "README.md should be ignored, got: {names:?}");
        // Cargo.toml should still be present.
        assert!(names.contains(&"Cargo.toml"), "Cargo.toml should be present, got: {names:?}");
    }

    #[test]
    fn test_dirs_sorted_before_files() {
        let tmp = make_test_tree();
        let tree = FileTree::from_dir(tmp.path()).expect("from_dir");

        // At depth 0, directories should come before files.
        let top_level: Vec<&FileEntry> = tree.entries.iter().filter(|e| e.depth == 0).collect();
        let first_file_idx = top_level.iter().position(|e| !e.is_dir);
        let last_dir_idx = top_level.iter().rposition(|e| e.is_dir);

        if let (Some(first_file), Some(last_dir)) = (first_file_idx, last_dir_idx) {
            assert!(last_dir < first_file, "dirs should come before files at depth 0");
        }
    }

    #[test]
    fn test_select_up_down_navigation() {
        let tmp = make_test_tree();
        let mut tree = FileTree::from_dir(tmp.path()).expect("from_dir");
        assert!(!tree.entries.is_empty());

        assert_eq!(tree.selected, 0);
        tree.select_down();
        assert_eq!(tree.selected, 1);
        tree.select_up();
        assert_eq!(tree.selected, 0);

        // select_up at 0 stays at 0.
        tree.select_up();
        assert_eq!(tree.selected, 0);

        // select_down to the end stays at last.
        for _ in 0..tree.entries.len() + 5 {
            tree.select_down();
        }
        assert_eq!(tree.selected, tree.entries.len() - 1);
    }

    #[test]
    fn test_visible_entries_windows_correctly() {
        let tmp = make_test_tree();
        let mut tree = FileTree::from_dir(tmp.path()).expect("from_dir");
        let total = tree.entries.len();
        assert!(total >= 3, "need at least 3 entries for this test");

        // Window of 2 entries.
        let visible = tree.visible_entries(2);
        assert_eq!(visible.len(), 2);

        // Move selected to the end.
        tree.selected = total - 1;
        let last_path = tree.entries[total - 1].path.clone();
        let visible = tree.visible_entries(2);
        assert_eq!(visible.len(), 2);
        // The last entry should be in the visible window.
        assert_eq!(visible.last().expect("last").path, last_path);

        // Window larger than entries returns all.
        tree.selected = 0;
        tree.scroll_offset = 0;
        let visible = tree.visible_entries(total + 10);
        assert_eq!(visible.len(), total);
    }
}

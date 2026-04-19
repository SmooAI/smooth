//! Input-box autocomplete for `@file` references and `/slash` commands.
//!
//! Both surfaces share the same popup UI, state, and key handling; only the
//! trigger character and the source of candidates differ. The `kind` field
//! lets the event loop and renderer distinguish the two.

use std::path::{Path, PathBuf};

use crate::files::FileEntry;

/// Maximum number of autocomplete results to show.
const MAX_RESULTS: usize = 20;

/// A pearl exposed to the `@` picker — id + title, pre-fetched from
/// `PearlStore` so autocomplete doesn't hit Dolt on every keystroke.
#[derive(Debug, Clone)]
pub struct PearlSuggestion {
    pub id: String,
    pub title: String,
}

/// Which completion surface is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionKind {
    /// `@` triggered — complete against the workspace file tree.
    #[default]
    File,
    /// `/` at the start of the input triggered — complete against
    /// registered slash commands.
    Command,
}

/// A single autocomplete suggestion.
#[derive(Debug, Clone)]
pub struct AutocompleteResult {
    /// Display label shown in the popup (e.g. `main.rs` or `/help`).
    pub label: String,
    /// Secondary line (relative path, command description).
    pub detail: String,
    /// Text to insert when the result is selected (e.g.
    /// `@src/main.rs` or `/help`).
    pub insert_text: String,
}

/// State for the autocomplete popup.
#[derive(Debug, Default)]
pub struct AutocompleteState {
    /// Whether the autocomplete popup is active.
    pub active: bool,
    /// What kind of completion is active.
    pub kind: CompletionKind,
    /// The query string after the trigger char (`@` or `/`).
    pub query: String,
    /// Filtered results matching the query.
    pub results: Vec<AutocompleteResult>,
    /// Index of the currently selected result.
    pub selected: usize,
    /// Byte offset of the trigger char in the input buffer — used on
    /// selection to replace `@query` / `/query` with the full insert
    /// text.
    pub trigger_pos: usize,
}

impl AutocompleteState {
    /// Activate file completion at the given trigger position (byte
    /// offset of the `@` char).
    pub fn activate_files(&mut self, trigger_pos: usize) {
        self.active = true;
        self.kind = CompletionKind::File;
        self.query.clear();
        self.results.clear();
        self.selected = 0;
        self.trigger_pos = trigger_pos;
    }

    /// Activate command completion at the given trigger position
    /// (byte offset of the `/` char — always 0 for a leading slash).
    pub fn activate_commands(&mut self, trigger_pos: usize) {
        self.active = true;
        self.kind = CompletionKind::Command;
        self.query.clear();
        self.results.clear();
        self.selected = 0;
        self.trigger_pos = trigger_pos;
    }

    /// Legacy alias for existing test callers — defaults to file
    /// completion.
    #[cfg(test)]
    pub fn activate(&mut self, at_pos: usize) {
        self.activate_files(at_pos);
    }

    /// Deactivate autocomplete and clear all state.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.kind = CompletionKind::default();
        self.query.clear();
        self.results.clear();
        self.selected = 0;
        self.trigger_pos = 0;
    }

    /// Update the `@` query with full routing: path prefixes
    /// (`~/`, `./`, `../`, `/`) expand to filesystem directory
    /// listings; otherwise pearls matching the query come first
    /// (they're scarce and high-signal), then workspace file
    /// matches fill the remainder up to [`MAX_RESULTS`].
    ///
    /// `workspace_root` is the directory relative paths resolve
    /// against — typically the TUI's current working directory.
    pub fn update_at_query(&mut self, query: &str, files: &[FileEntry], pearls: &[PearlSuggestion], workspace_root: &Path) {
        self.query = query.to_string();
        self.selected = 0;

        // Glob queries (any `*`, `?`, or `{` outside a leading
        // path prefix) recursively walk the filesystem from the
        // deepest literal base directory and match by the glob.
        // Handled before the simple path-prefix path because
        // `../**/(dashboard)` starts with `../` but means "recurse
        // under parent", not "list parent directory".
        if has_glob_meta(query) {
            if let Some(results) = glob_completions(query, workspace_root) {
                self.results = results;
                return;
            }
        }

        // Path-prefix queries win unconditionally — the user is
        // clearly asking for a filesystem listing, not a fuzzy
        // search over workspace files.
        if let Some(results) = path_completions(query, workspace_root) {
            self.results = results;
            return;
        }

        let lower_query = query.to_lowercase();
        let mut out: Vec<AutocompleteResult> = Vec::new();

        // Pearls first — few of them, high signal, named items
        // trump fuzzy filename hits. Cap at 6 so they don't crowd
        // out files entirely when the user is actually looking for
        // a path.
        for p in pearls {
            if lower_query.is_empty() || p.id.to_lowercase().contains(&lower_query) || p.title.to_lowercase().contains(&lower_query) {
                out.push(AutocompleteResult {
                    label: p.id.clone(),
                    detail: p.title.clone(),
                    insert_text: format!("@{}", p.id),
                });
                if out.len() >= 6 {
                    break;
                }
            }
        }

        // Workspace files fill the remainder.
        for entry in files {
            if out.len() >= MAX_RESULTS {
                break;
            }
            if !lower_query.is_empty() && !entry.name.to_lowercase().contains(&lower_query) {
                continue;
            }
            out.push(AutocompleteResult {
                label: entry.name.clone(),
                detail: entry.path.to_string_lossy().into_owned(),
                insert_text: format!("@{}", entry.path.display()),
            });
        }

        self.results = out;
    }

    /// Update the query and re-filter results from the file list
    /// only. Thin backward-compat wrapper around [`update_at_query`]
    /// for callers that don't have pearls or a workspace root.
    ///
    /// Uses case-insensitive substring matching on file names.
    /// An empty query returns all files up to [`MAX_RESULTS`].
    pub fn update_query(&mut self, query: &str, files: &[FileEntry]) {
        self.update_at_query(query, files, &[], Path::new("."));
    }

    /// Update the query and re-filter results against a list of
    /// registered slash commands `(name, description)`. Matches by
    /// case-insensitive prefix on the command name (so typing
    /// `/he` narrows straight to `/help`).
    pub fn update_command_query(&mut self, query: &str, commands: &[(String, String)]) {
        self.query = query.to_string();
        self.selected = 0;

        let lower_query = query.to_lowercase();

        self.results = commands
            .iter()
            .filter(|(name, _)| lower_query.is_empty() || name.to_lowercase().starts_with(&lower_query))
            .take(MAX_RESULTS)
            .map(|(name, description)| AutocompleteResult {
                label: format!("/{name}"),
                detail: description.clone(),
                insert_text: format!("/{name}"),
            })
            .collect();
    }

    /// Return the currently selected result, if any.
    pub fn selected_result(&self) -> Option<&AutocompleteResult> {
        self.results.get(self.selected)
    }

    /// Move the selection up by one entry.
    pub fn select_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move the selection down by one entry.
    pub fn select_down(&mut self) {
        if !self.results.is_empty() && self.selected < self.results.len() - 1 {
            self.selected += 1;
        }
    }
}

/// Attempt to expand `query` as a filesystem path rooted at `~`
/// (home), `.` / `..` (relative to `workspace_root`), or `/`
/// (absolute), and return the matching directory entries.
///
/// Returns `Some(results)` when `query` is path-like — even if the
/// resulting directory has no matches — so the caller knows not to
/// fall through to file-tree fuzzy matching. Returns `None` when
/// the query doesn't start with a recognised path prefix.
fn path_completions(query: &str, workspace_root: &Path) -> Option<Vec<AutocompleteResult>> {
    let (base_dir, file_prefix, query_dir_part): (PathBuf, String, String) = if query == "~" {
        let base = dirs_next::home_dir()?;
        (base, String::new(), "~/".to_string())
    } else if let Some(rest) = query.strip_prefix("~/") {
        let base = dirs_next::home_dir()?;
        let (dir_in_query, fp) = split_dir_and_filename(rest);
        (base.join(dir_in_query), fp.to_string(), format!("~/{dir_in_query}"))
    } else if let Some(rest) = query.strip_prefix("./") {
        let (dir_in_query, fp) = split_dir_and_filename(rest);
        (workspace_root.join(dir_in_query), fp.to_string(), format!("./{dir_in_query}"))
    } else if query.starts_with("../") || query == ".." {
        // Walk up one parent per leading "../" segment, then treat
        // the remainder as a sub-path.
        let mut path = workspace_root.to_path_buf();
        let mut remainder = query;
        let mut leading = String::new();
        while remainder == ".." || remainder.starts_with("../") {
            path = path.parent()?.to_path_buf();
            leading.push_str("../");
            remainder = remainder.strip_prefix("../").unwrap_or("");
        }
        let (dir_in_query, fp) = split_dir_and_filename(remainder);
        (path.join(dir_in_query), fp.to_string(), format!("{leading}{dir_in_query}"))
    } else if query.starts_with('/') {
        let (dir_in_query, fp) = split_dir_and_filename(query);
        (PathBuf::from(dir_in_query), fp.to_string(), dir_in_query.to_string())
    } else {
        return None;
    };

    let lower_prefix = file_prefix.to_lowercase();
    let show_hidden = file_prefix.starts_with('.');
    let mut results: Vec<AutocompleteResult> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&base_dir) {
        for entry in entries.flatten() {
            let name_os = entry.file_name();
            let name = name_os.to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            if !lower_prefix.is_empty() && !name.to_lowercase().starts_with(&lower_prefix) {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let slash = if is_dir { "/" } else { "" };
            let insert = format!("@{query_dir_part}{name}{slash}");
            results.push(AutocompleteResult {
                label: format!("{name}{slash}"),
                detail: if is_dir { "directory".into() } else { "file".into() },
                insert_text: insert,
            });
        }
    }

    // Directories first (users are usually path-hunting), then
    // alphabetical.
    results.sort_by(|a, b| {
        let a_dir = a.detail == "directory";
        let b_dir = b.detail == "directory";
        b_dir.cmp(&a_dir).then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    results.truncate(MAX_RESULTS);
    Some(results)
}

/// Does the query contain glob metacharacters (`*`, `?`, `{`)
/// anywhere? Used to route `@foo/**/*.rs`-style queries to the
/// recursive walker instead of the simple directory listing path.
fn has_glob_meta(query: &str) -> bool {
    query.chars().any(|c| matches!(c, '*' | '?' | '{'))
}

/// Recursively walk from the deepest literal prefix of `query` and
/// return entries matching the remainder as a glob. Respects
/// `.gitignore` via `ignore::WalkBuilder`.
///
/// Supported query shapes:
///
/// * `**/*.rs` — recurse from the workspace root
/// * `src/**/mod.rs` — recurse from `workspace_root/src`
/// * `../**/(dashboard)` — climb to the parent, then recurse
/// * `~/dev/**/README.md` — expand home, then recurse
/// * `/etc/**/*.conf` — recurse from an absolute path
///
/// Returns `None` when the query isn't a valid glob (e.g. `globset`
/// rejects the pattern) so callers can fall through to simpler
/// matching modes. Returns `Some(vec![])` when the walker ran but
/// matched nothing — the picker treats an empty result set as "close
/// silently" so a stray `*` doesn't trap the user inside a popup.
fn glob_completions(query: &str, workspace_root: &Path) -> Option<Vec<AutocompleteResult>> {
    let (base_dir, base_display, pattern) = split_base_and_glob(query, workspace_root)?;
    if !base_dir.is_dir() {
        return Some(Vec::new());
    }

    let matcher = globset::Glob::new(&pattern).ok()?.compile_matcher();

    let mut results: Vec<AutocompleteResult> = Vec::new();
    let walker = ignore::WalkBuilder::new(&base_dir)
        .hidden(false) // show hidden entries — users globbing for `.env*` expect them
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(12)) // enough for real repos; bounded so a bad glob can't hang
        .build();

    for entry in walker.flatten() {
        let Ok(rel) = entry.path().strip_prefix(&base_dir) else { continue };
        if rel.as_os_str().is_empty() {
            continue; // skip the base dir itself
        }
        let rel_str = rel.to_string_lossy();
        if !matcher.is_match(&*rel_str) {
            continue;
        }

        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        let slash = if is_dir { "/" } else { "" };
        // Preserve the leading "~/" / "../" / absolute prefix so the
        // inserted @-reference round-trips back to the same path.
        let insert_prefix = if base_display.is_empty() {
            String::new()
        } else if base_display.ends_with('/') {
            base_display.clone()
        } else {
            format!("{base_display}/")
        };
        results.push(AutocompleteResult {
            label: format!("{rel_str}{slash}"),
            detail: if is_dir { "directory".into() } else { "file".into() },
            insert_text: format!("@{insert_prefix}{rel_str}{slash}"),
        });
        if results.len() >= MAX_RESULTS {
            break;
        }
    }

    // Directories before files, alphabetical within.
    results.sort_by(|a, b| {
        let a_dir = a.detail == "directory";
        let b_dir = b.detail == "directory";
        b_dir.cmp(&a_dir).then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    Some(results)
}

/// Split a glob query into `(base_dir, base_display, glob_pattern)`.
///
/// `base_dir` is an absolute path to start the walker from.
/// `base_display` is how the prefix should appear back in the inserted
/// `@`-reference (e.g. `~/dev`, `../src`, `/etc`, or ``).
/// `glob_pattern` is what remains after stripping the literal prefix,
/// to be compiled with `globset`.
fn split_base_and_glob(query: &str, workspace_root: &Path) -> Option<(PathBuf, String, String)> {
    // Resolve any leading path prefix first. The remainder may still
    // contain literal path components before the first glob metachar.
    let (mut base_dir, mut base_display, rest) = if query == "~" {
        (dirs_next::home_dir()?, "~".to_string(), String::new())
    } else if let Some(r) = query.strip_prefix("~/") {
        (dirs_next::home_dir()?, "~".to_string(), r.to_string())
    } else if let Some(r) = query.strip_prefix("./") {
        (workspace_root.to_path_buf(), ".".to_string(), r.to_string())
    } else if query.starts_with("../") || query == ".." {
        let mut path = workspace_root.to_path_buf();
        let mut remainder = query;
        let mut display = String::new();
        while remainder == ".." || remainder.starts_with("../") {
            path = path.parent()?.to_path_buf();
            if !display.is_empty() {
                display.push('/');
            }
            display.push_str("..");
            remainder = remainder.strip_prefix("../").unwrap_or("");
        }
        (path, display, remainder.to_string())
    } else if query.starts_with('/') {
        let r = query.strip_prefix('/').unwrap_or(query).to_string();
        (PathBuf::from("/"), String::new(), r)
    } else {
        (workspace_root.to_path_buf(), String::new(), query.to_string())
    };

    // Walk literal path components until we hit the first one with a
    // glob metachar. Everything before it goes into base_dir; the
    // rest stays as the glob pattern.
    let mut pattern_parts: Vec<&str> = Vec::new();
    let mut consumed = true;
    for part in rest.split('/') {
        if !consumed || has_glob_meta(part) {
            consumed = false;
            pattern_parts.push(part);
            continue;
        }
        if part.is_empty() {
            continue;
        }
        base_dir.push(part);
        if base_display.is_empty() {
            base_display = part.to_string();
        } else {
            base_display.push('/');
            base_display.push_str(part);
        }
    }

    let pattern = pattern_parts.join("/");
    if pattern.is_empty() {
        return None; // no glob — caller should route to path_completions
    }
    Some((base_dir, base_display, pattern))
}

/// Split `s` into `(directory_part_with_trailing_slash, filename_prefix)`.
///
/// ```text
/// "Documents/proj"  → ("Documents/", "proj")
/// "proj"            → ("",           "proj")
/// "Documents/"      → ("Documents/", "")
/// ""                → ("",           "")
/// ```
fn split_dir_and_filename(s: &str) -> (&str, &str) {
    match s.rfind('/') {
        Some(idx) => (&s[..=idx], &s[idx + 1..]),
        None => ("", s),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn sample_files() -> Vec<FileEntry> {
        vec![
            FileEntry {
                path: PathBuf::from("src/main.rs"),
                name: "main.rs".to_string(),
                depth: 1,
                is_dir: false,
            },
            FileEntry {
                path: PathBuf::from("src/lib.rs"),
                name: "lib.rs".to_string(),
                depth: 1,
                is_dir: false,
            },
            FileEntry {
                path: PathBuf::from("src/app.rs"),
                name: "app.rs".to_string(),
                depth: 1,
                is_dir: false,
            },
            FileEntry {
                path: PathBuf::from("Cargo.toml"),
                name: "Cargo.toml".to_string(),
                depth: 0,
                is_dir: false,
            },
            FileEntry {
                path: PathBuf::from("README.md"),
                name: "README.md".to_string(),
                depth: 0,
                is_dir: false,
            },
        ]
    }

    #[test]
    fn test_activate_deactivate() {
        let mut ac = AutocompleteState::default();
        assert!(!ac.active);

        ac.activate(5);
        assert!(ac.active);
        assert!(ac.query.is_empty());
        assert_eq!(ac.selected, 0);

        ac.deactivate();
        assert!(!ac.active);
        assert!(ac.query.is_empty());
        assert!(ac.results.is_empty());
    }

    #[test]
    fn test_update_query_filters_by_substring() {
        let mut ac = AutocompleteState::default();
        ac.activate(0);

        let files = sample_files();
        ac.update_query("main", &files);

        assert_eq!(ac.results.len(), 1);
        assert_eq!(ac.results[0].label, "main.rs");
        assert!(ac.results[0].insert_text.contains("main.rs"));
    }

    #[test]
    fn test_selected_result_returns_correct_item() {
        let mut ac = AutocompleteState::default();
        ac.activate(0);

        let files = sample_files();
        ac.update_query("rs", &files);

        // Should match main.rs, lib.rs, app.rs
        assert_eq!(ac.results.len(), 3);
        assert_eq!(ac.selected, 0);

        let first = ac.selected_result().expect("should have a result");
        assert_eq!(first.label, "main.rs");

        ac.select_down();
        let second = ac.selected_result().expect("should have a result");
        assert_eq!(second.label, "lib.rs");
    }

    #[test]
    fn test_empty_query_returns_all_files() {
        let mut ac = AutocompleteState::default();
        ac.activate(0);

        let files = sample_files();
        ac.update_query("", &files);

        assert_eq!(ac.results.len(), files.len());
    }

    #[test]
    fn test_select_up_down() {
        let mut ac = AutocompleteState::default();
        ac.activate(0);

        let files = sample_files();
        ac.update_query("", &files);

        assert_eq!(ac.selected, 0);
        ac.select_down();
        assert_eq!(ac.selected, 1);
        ac.select_down();
        assert_eq!(ac.selected, 2);
        ac.select_up();
        assert_eq!(ac.selected, 1);

        // select_up at 0 stays at 0.
        ac.selected = 0;
        ac.select_up();
        assert_eq!(ac.selected, 0);

        // select_down at end stays at end.
        ac.selected = ac.results.len() - 1;
        ac.select_down();
        assert_eq!(ac.selected, ac.results.len() - 1);
    }

    #[test]
    fn test_case_insensitive_matching() {
        let mut ac = AutocompleteState::default();
        ac.activate(0);

        let files = sample_files();
        ac.update_query("MAIN", &files);

        assert_eq!(ac.results.len(), 1);
        assert_eq!(ac.results[0].label, "main.rs");
    }

    #[test]
    fn test_path_completions_returns_none_for_non_path_query() {
        // Plain words don't trigger path listing.
        assert!(path_completions("main.rs", std::path::Path::new(".")).is_none());
        assert!(path_completions("foo", std::path::Path::new(".")).is_none());
    }

    #[test]
    fn test_path_completions_tilde_lists_home() {
        let Some(home) = dirs_next::home_dir() else { return };
        if !home.is_dir() {
            return;
        }
        let results = path_completions("~/", std::path::Path::new(".")).expect("path-like query returns Some");
        // Home dirs have at least one non-hidden entry on test machines.
        assert!(
            results.iter().all(|r| r.insert_text.starts_with("@~/")),
            "insert texts: {:?}",
            results.iter().map(|r| &r.insert_text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_path_completions_relative_resolves_against_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("alpha")).unwrap();
        std::fs::write(tmp.path().join("beta.txt"), b"hi").unwrap();

        let results = path_completions("./", tmp.path()).expect("path-like");
        let labels: Vec<String> = results.iter().map(|r| r.label.clone()).collect();
        assert!(labels.iter().any(|l| l.starts_with("alpha")));
        assert!(labels.iter().any(|l| l == "beta.txt"));
        // Directories first
        assert!(labels[0].ends_with('/'), "first label is dir: {}", labels[0]);
    }

    #[test]
    fn test_path_completions_parent_walks_up() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("deep");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(tmp.path().join("sibling.txt"), b"").unwrap();

        let results = path_completions("../", &nested).expect("path-like");
        let labels: Vec<String> = results.iter().map(|r| r.label.clone()).collect();
        assert!(labels.iter().any(|l| l == "sibling.txt"), "labels: {labels:?}");
        let inserts: Vec<String> = results.iter().map(|r| r.insert_text.clone()).collect();
        assert!(inserts.iter().any(|i| i == "@../sibling.txt"), "inserts: {inserts:?}");
    }

    #[test]
    fn test_update_at_query_mixes_pearls_and_files() {
        let mut ac = AutocompleteState::default();
        ac.activate_files(0);
        let files = sample_files();
        let pearls = vec![
            PearlSuggestion {
                id: "th-aaa111".into(),
                title: "Refactor Main File".into(),
            },
            PearlSuggestion {
                id: "th-bbb222".into(),
                title: "Unrelated".into(),
            },
        ];
        // Query "main" matches one pearl title AND main.rs — pearls
        // come first.
        ac.update_at_query("main", &files, &pearls, std::path::Path::new("."));
        assert!(!ac.results.is_empty());
        assert_eq!(ac.results[0].label, "th-aaa111");
        assert_eq!(ac.results[0].insert_text, "@th-aaa111");
        // main.rs should still show up after the pearl
        assert!(ac.results.iter().any(|r| r.label == "main.rs"));
    }

    #[test]
    fn test_update_at_query_path_prefix_uses_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), b"").unwrap();
        let mut ac = AutocompleteState::default();
        ac.activate_files(0);
        // Files list is unused because path prefix wins.
        let files: Vec<FileEntry> = vec![];
        ac.update_at_query("./", &files, &[], tmp.path());
        assert!(ac.results.iter().any(|r| r.label == "hello.txt"));
        assert!(ac.results.iter().any(|r| r.insert_text == "@./hello.txt"));
    }

    #[test]
    fn test_update_command_query_prefix_match() {
        let mut ac = AutocompleteState::default();
        ac.activate_commands(0);
        let commands = vec![
            ("help".to_string(), "List all available commands".to_string()),
            ("clear".to_string(), "Clear chat history".to_string()),
            ("compact".to_string(), "Trigger context compaction".to_string()),
            ("quit".to_string(), "Exit the TUI".to_string()),
        ];

        // No query → all commands.
        ac.update_command_query("", &commands);
        assert_eq!(ac.results.len(), 4);

        // "c" → clear + compact (case-insensitive prefix).
        ac.update_command_query("c", &commands);
        assert_eq!(ac.results.len(), 2);
        assert_eq!(ac.results[0].insert_text, "/clear");
        assert_eq!(ac.results[1].insert_text, "/compact");

        // "Cle" → just clear.
        ac.update_command_query("Cle", &commands);
        assert_eq!(ac.results.len(), 1);
        assert_eq!(ac.results[0].label, "/clear");

        // No match.
        ac.update_command_query("zzz", &commands);
        assert!(ac.results.is_empty());
    }

    #[test]
    fn test_activate_commands_sets_kind() {
        let mut ac = AutocompleteState::default();
        ac.activate_commands(0);
        assert!(ac.active);
        assert_eq!(ac.kind, CompletionKind::Command);
        ac.activate_files(5);
        assert_eq!(ac.kind, CompletionKind::File);
        assert_eq!(ac.trigger_pos, 5);
    }

    #[test]
    fn test_no_results_for_unmatched_query() {
        let mut ac = AutocompleteState::default();
        ac.activate(0);

        let files = sample_files();
        ac.update_query("zzzzz_nonexistent", &files);

        assert!(ac.results.is_empty());
        assert!(ac.selected_result().is_none());
    }
}

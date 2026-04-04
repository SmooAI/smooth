//! @ reference autocomplete for file paths in the input box.

use crate::files::FileEntry;

/// Maximum number of autocomplete results to show.
const MAX_RESULTS: usize = 20;

/// A single autocomplete suggestion.
#[derive(Debug, Clone)]
pub struct AutocompleteResult {
    /// Display label shown in the popup.
    pub label: String,
    /// Text to insert when the result is selected (e.g., `@src/main.rs`).
    pub insert_text: String,
}

/// State for the @ autocomplete popup.
#[derive(Debug, Default)]
pub struct AutocompleteState {
    /// Whether the autocomplete popup is active.
    pub active: bool,
    /// The query string after the `@` trigger.
    pub query: String,
    /// Filtered results matching the query.
    pub results: Vec<AutocompleteResult>,
    /// Index of the currently selected result.
    pub selected: usize,
}

impl AutocompleteState {
    /// Activate autocomplete at the given cursor position.
    ///
    /// The `_at_pos` parameter is reserved for future use (e.g., positioning the popup).
    pub fn activate(&mut self, _at_pos: usize) {
        self.active = true;
        self.query.clear();
        self.results.clear();
        self.selected = 0;
    }

    /// Deactivate autocomplete and clear all state.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.query.clear();
        self.results.clear();
        self.selected = 0;
    }

    /// Update the query and re-filter results from the file list.
    ///
    /// Uses case-insensitive substring matching on file names.
    /// An empty query returns all files up to [`MAX_RESULTS`].
    pub fn update_query(&mut self, query: &str, files: &[FileEntry]) {
        self.query = query.to_string();
        self.selected = 0;

        let lower_query = query.to_lowercase();

        self.results = files
            .iter()
            .filter(|entry| {
                if lower_query.is_empty() {
                    true
                } else {
                    entry.name.to_lowercase().contains(&lower_query)
                }
            })
            .take(MAX_RESULTS)
            .map(|entry| {
                let display_name = &entry.name;
                let rel_path = entry.path.to_string_lossy();
                AutocompleteResult {
                    label: display_name.clone(),
                    insert_text: format!("@{rel_path}"),
                }
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
    fn test_no_results_for_unmatched_query() {
        let mut ac = AutocompleteState::default();
        ac.activate(0);

        let files = sample_files();
        ac.update_query("zzzzz_nonexistent", &files);

        assert!(ac.results.is_empty());
        assert!(ac.selected_result().is_none());
    }
}

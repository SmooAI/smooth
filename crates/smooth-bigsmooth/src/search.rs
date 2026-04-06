//! Search — powers @ autocomplete (issues, files, paths).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use smooth_pearls::{PearlQuery, PearlStore};

/// A search result for @ autocomplete.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    #[serde(rename = "type")]
    pub result_type: String,
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
}

/// Search issues by title/ID using `PearlStore`.
pub fn search_pearls(query: &str, pearl_store: &PearlStore) -> Vec<SearchResult> {
    let issues = pearl_store.list(&PearlQuery::new()).unwrap_or_default();
    let q = query.to_lowercase();
    issues
        .into_iter()
        .filter(|i| i.id.to_lowercase().contains(&q) || i.title.to_lowercase().contains(&q))
        .take(10)
        .map(|i| SearchResult {
            result_type: "issue".into(),
            id: i.id.clone(),
            label: format!("{}: {}", i.id, i.title),
            detail: Some(format!("{} {}", i.status, i.priority)),
        })
        .collect()
}

/// Search files using globwalk.
pub fn search_files(query: &str, base_path: &Path) -> Vec<SearchResult> {
    let q = query
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '.' && c != '_' && c != '-', "");
    if q.is_empty() {
        return vec![];
    }

    let pattern = format!("**/*{q}*");
    let walker = globwalk::GlobWalkerBuilder::from_patterns(base_path, &[&pattern]).max_depth(4).build();

    let Ok(walker) = walker else {
        return vec![];
    };

    walker
        .filter_map(Result::ok)
        .filter(|e| {
            let path = e.path().to_string_lossy();
            !path.contains("node_modules") && !path.contains(".git/") && !path.contains("target/") && !path.contains(".next/") && !path.contains("dist/")
        })
        .take(15)
        .map(|e| {
            let rel = e.path().strip_prefix(base_path).unwrap_or(e.path());
            let is_dir = e.file_type().is_dir();
            SearchResult {
                result_type: "file".into(),
                id: rel.to_string_lossy().into(),
                label: rel.to_string_lossy().into(),
                detail: Some(if is_dir { "dir" } else { "file" }.into()),
            }
        })
        .collect()
}

/// Expand path (supports ~) and list entries.
pub fn search_paths(query: &str) -> Vec<SearchResult> {
    let expanded = query.strip_prefix('~').map_or_else(
        || PathBuf::from(query),
        |rest| {
            let rest = rest.trim_start_matches('/');
            if rest.is_empty() {
                dirs_next::home_dir().unwrap_or_default()
            } else {
                dirs_next::home_dir().unwrap_or_default().join(rest)
            }
        },
    );

    // If it's a directory, list contents
    if expanded.is_dir() {
        return fs::read_dir(&expanded)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .take(15)
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let is_dir = e.file_type().is_ok_and(|ft| ft.is_dir());
                let id = format!("{}/{name}", query.trim_end_matches('/'));
                SearchResult {
                    result_type: "path".into(),
                    id,
                    label: if is_dir { format!("{name}/") } else { name },
                    detail: Some(if is_dir { "dir" } else { "file" }.into()),
                }
            })
            .collect();
    }

    // Partial path — list parent filtered
    let parent = expanded.parent().unwrap_or(Path::new("/"));
    let partial = expanded.file_name().map_or(String::new(), |n| n.to_string_lossy().to_lowercase());

    if parent.is_dir() {
        return fs::read_dir(parent)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_lowercase();
                name.starts_with(&partial) && !name.starts_with('.')
            })
            .take(15)
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let is_dir = e.file_type().is_ok_and(|ft| ft.is_dir());
                let parent_query = query.rsplit_once('/').map_or("", |p| p.0);
                SearchResult {
                    result_type: "path".into(),
                    id: format!("{parent_query}/{name}"),
                    label: if is_dir { format!("{name}/") } else { name },
                    detail: Some(if is_dir { "dir" } else { "file" }.into()),
                }
            })
            .collect();
    }

    vec![]
}

/// Combined search: issues + files + paths.
pub fn search_all(query: &str, base_path: &Path, pearl_store: &PearlStore) -> Vec<SearchResult> {
    if query.starts_with('~') || query.starts_with('/') {
        return search_paths(query);
    }

    let mut results = search_pearls(query, pearl_store);
    results.extend(search_files(query, base_path));
    results.truncate(15);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use smooth_pearls::{NewPearl, PearlType as IType, Priority as Prio};

    fn test_store() -> Option<PearlStore> {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let dolt_dir = tmp.path().join("dolt");
        match PearlStore::init(&dolt_dir) {
            Ok(store) => {
                std::mem::forget(tmp);
                Some(store)
            }
            Err(_) => None,
        }
    }

    fn new_issue(title: &str) -> NewPearl {
        NewPearl {
            title: title.into(),
            description: String::new(),
            pearl_type: IType::Task,
            priority: Prio::Medium,
            assigned_to: None,
            parent_id: None,
            labels: vec![],
        }
    }

    #[test]
    fn test_search_pearls_by_title() {
        let Some(store) = test_store() else { return };
        let mut login_issue = new_issue("Fix login bug");
        login_issue.description = "Users cannot log in".into();
        store.create(&login_issue).unwrap();
        store.create(&new_issue("Add dashboard widget")).unwrap();

        let results = search_pearls("login", &store);
        assert_eq!(results.len(), 1);
        assert!(results[0].label.contains("Fix login bug"));
        assert_eq!(results[0].result_type, "issue");
    }

    #[test]
    fn test_search_pearls_by_id() {
        let Some(store) = test_store() else { return };
        let issue = store.create(&new_issue("Some task")).unwrap();

        let results = search_pearls(&issue.id, &store);
        assert_eq!(results.len(), 1);
        assert!(results[0].id == issue.id);
    }

    #[test]
    fn test_search_pearls_empty_query() {
        let Some(store) = test_store() else { return };
        store.create(&new_issue("Task one")).unwrap();

        // Empty query matches everything (contains "")
        let results = search_pearls("", &store);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_search_pearls_no_match() {
        let Some(store) = test_store() else { return };
        store.create(&new_issue("Fix login bug")).unwrap();

        let results = search_pearls("zzzznotfound", &store);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("hello.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("world.rs"), "fn test() {}").unwrap();
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/hello_sub.rs"), "").unwrap();

        let results = search_files("hello", dir.path());
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.label.contains("hello")));
    }

    #[test]
    fn test_search_paths_home() {
        let results = search_paths("~/");
        // Home directory should have entries
        assert!(!results.is_empty());
    }

    #[test]
    fn test_search_files_empty_query() {
        let dir = tempfile::tempdir().unwrap();
        let results = search_files("", dir.path());
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_all_issues_and_files() {
        let Some(store) = test_store() else { return };
        store.create(&new_issue("hello world task")).unwrap();

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("hello.rs"), "").unwrap();

        let results = search_all("hello", dir.path(), &store);
        // Should have both issue and file results
        assert!(results.iter().any(|r| r.result_type == "issue"));
        assert!(results.iter().any(|r| r.result_type == "file"));
    }

    #[test]
    fn test_search_all_path_mode() {
        let Some(store) = test_store() else { return };
        let results = search_all("~/", Path::new("/tmp"), &store);
        // Path queries bypass issue+file search
        assert!(results.iter().all(|r| r.result_type == "path"));
    }
}

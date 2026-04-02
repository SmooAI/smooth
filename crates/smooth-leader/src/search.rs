//! Search — powers @ autocomplete (beads, files, paths).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// A search result for @ autocomplete.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    #[serde(rename = "type")]
    pub result_type: String,
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
}

/// Search beads by title/ID.
pub fn search_beads(query: &str) -> Vec<SearchResult> {
    let beads = crate::beads::list_beads(None).unwrap_or_default();
    let q = query.to_lowercase();
    beads
        .into_iter()
        .filter(|b| b.id.to_lowercase().contains(&q) || b.title.to_lowercase().contains(&q))
        .take(10)
        .map(|b| SearchResult {
            result_type: "bead".into(),
            id: b.id.clone(),
            label: format!("{}: {}", b.id, b.title),
            detail: Some(format!("{} P{}", b.status, b.priority)),
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
    let expanded = if query.starts_with('~') {
        let rest = query[1..].trim_start_matches('/');
        if rest.is_empty() {
            dirs_next::home_dir().unwrap_or_default()
        } else {
            dirs_next::home_dir().unwrap_or_default().join(rest)
        }
    } else {
        PathBuf::from(query)
    };

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
                let is_dir = e.file_type().map_or(false, |ft| ft.is_dir());
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
                let is_dir = e.file_type().map_or(false, |ft| ft.is_dir());
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

/// Combined search: beads + files + paths.
pub fn search_all(query: &str, base_path: &Path) -> Vec<SearchResult> {
    if query.starts_with('~') || query.starts_with('/') {
        return search_paths(query);
    }

    let mut results = search_beads(query);
    results.extend(search_files(query, base_path));
    results.truncate(15);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
}

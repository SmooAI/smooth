//! `grep` — in-process regex search across the workspace (respects `.gitignore`).

use std::fmt::Write as _;
use std::path::PathBuf;

use async_trait::async_trait;
use globset::{Glob, GlobSetBuilder};
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::path::resolve_workspace_path;
use crate::util::req_str;

/// Max matches returned.
const MATCH_CAP: usize = 250;
/// Max characters per matched line before truncation.
const LINE_CAP: usize = 200;

/// `grep` tool — uses the `ripgrep` libraries (no shelling out).
pub struct GrepTool {
    /// Workspace root.
    pub workspace: PathBuf,
}

#[async_trait]
impl Tool for GrepTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "grep".into(),
            description: "Search file contents in the workspace with a regex (respects .gitignore). Returns file:line:match.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for" },
                    "path": { "type": "string", "description": "Relative dir or file to search in (default: entire workspace)" },
                    "include": { "type": "string", "description": "Glob to filter files, e.g. '*.rs', '*.{ts,tsx}'" }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let pattern = req_str(&arguments, "pattern")?;
        let rel = arguments.get("path").and_then(Value::as_str).unwrap_or(".").to_owned();
        let include = arguments.get("include").and_then(Value::as_str).map(str::to_owned);
        let root = resolve_workspace_path(&self.workspace, &rel)?;
        let base = self.workspace.clone();

        tokio::task::spawn_blocking(move || grep_blocking(&base, &root, &pattern, include.as_deref()))
            .await
            .map_err(|e| anyhow::anyhow!("grep task panicked: {e}"))?
    }
}

fn grep_blocking(base: &std::path::Path, root: &std::path::Path, pattern: &str, include: Option<&str>) -> anyhow::Result<String> {
    let matcher = RegexMatcher::new(pattern).map_err(|e| anyhow::anyhow!("invalid regex `{pattern}`: {e}"))?;

    let include_set = match include {
        Some(glob) => {
            let g = Glob::new(glob).map_err(|e| anyhow::anyhow!("invalid include glob `{glob}`: {e}"))?;
            let mut b = GlobSetBuilder::new();
            b.add(g);
            Some(b.build().map_err(|e| anyhow::anyhow!("invalid include glob set: {e}"))?)
        }
        None => None,
    };

    let mut searcher = Searcher::new();
    let mut results: Vec<String> = Vec::new();
    let mut capped = false;

    'walk: for entry in crate::walk::pruned_walk(root).flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap_or(path);
        if let Some(set) = &include_set {
            if !set.is_match(rel) {
                continue;
            }
        }
        let display = rel.display().to_string();

        let mut local: Vec<(u64, String)> = Vec::new();
        let _ = searcher.search_path(
            &matcher,
            path,
            UTF8(|lnum, line| {
                let trimmed = line.trim_end();
                let capped_line: String = if trimmed.chars().count() > LINE_CAP {
                    let mut s: String = trimmed.chars().take(LINE_CAP).collect();
                    s.push('…');
                    s
                } else {
                    trimmed.to_owned()
                };
                local.push((lnum, capped_line));
                Ok(true)
            }),
        );

        for (lnum, line) in local {
            results.push(format!("{display}:{lnum}:{line}"));
            if results.len() >= MATCH_CAP {
                capped = true;
                break 'walk;
            }
        }
    }

    if results.is_empty() {
        return Ok("no matches found".to_owned());
    }
    let mut out = results.join("\n");
    out.push('\n');
    if capped {
        let _ = writeln!(out, "... (showing first {MATCH_CAP} matches)");
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    async fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join("src")).await.unwrap();
        tokio::fs::write(dir.path().join("src/a.rs"), "fn alpha() {}\nlet needle = 1;\n").await.unwrap();
        tokio::fs::write(dir.path().join("src/b.txt"), "needle here too\n").await.unwrap();
        dir
    }

    #[tokio::test]
    async fn finds_matches_with_location() {
        let dir = workspace().await;
        let tool = GrepTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = tool.execute(json!({"pattern": "needle"})).await.unwrap();
        assert!(out.contains("src/a.rs:2:let needle = 1;"), "{out}");
        assert!(out.contains("src/b.txt:1:needle here too"), "{out}");
    }

    #[tokio::test]
    async fn include_glob_filters_files() {
        let dir = workspace().await;
        let tool = GrepTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = tool.execute(json!({"pattern": "needle", "include": "*.rs"})).await.unwrap();
        assert!(out.contains("src/a.rs"), "{out}");
        assert!(!out.contains("b.txt"), "include should exclude txt: {out}");
    }

    #[tokio::test]
    async fn no_matches_message() {
        let dir = workspace().await;
        let tool = GrepTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = tool.execute(json!({"pattern": "zzzznotfound"})).await.unwrap();
        assert_eq!(out, "no matches found");
    }

    #[tokio::test]
    async fn invalid_regex_errors() {
        let dir = workspace().await;
        let tool = GrepTool {
            workspace: dir.path().to_path_buf(),
        };
        let err = tool.execute(json!({"pattern": "("})).await.unwrap_err();
        assert!(err.to_string().contains("invalid regex"), "{err}");
    }
}

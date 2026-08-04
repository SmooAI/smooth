//! Read-only filesystem tools: `read_file`, `list_files`.

use std::fmt::Write as _;
use std::path::PathBuf;

use async_trait::async_trait;
use globset::{Glob, GlobSetBuilder};
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::path::resolve_workspace_path;
use crate::util::req_str;

/// Default max lines returned by `read_file`.
const READ_DEFAULT_LIMIT: usize = 2000;
/// Max entries returned by `list_files`.
const LIST_CAP: usize = 200;
/// Max filesystem entries `list_files` will *examine* before stopping. A
/// backstop so a glob over a real project tree stays bounded even after the
/// heavy dirs are pruned — the walk can't run away. (`.git`/`node_modules`/
/// `target` are already pruned by [`crate::walk::pruned_walk`].)
const LIST_WALK_BUDGET: usize = 50_000;

/// `read_file` — read a workspace file, optionally a line window, `cat -n` style.
pub struct ReadFileTool {
    /// Workspace root all paths are confined to.
    pub workspace: PathBuf,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".into(),
            description: "Read a UTF-8 text file within the workspace. Returns line-numbered content; supports an optional line window.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path within the workspace" },
                    "offset": { "type": "integer", "description": "1-based start line (default: 1)" },
                    "limit": { "type": "integer", "description": "Max lines to return (default: 2000)" }
                },
                "required": ["path"]
            }),
        }
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let rel = req_str(&arguments, "path")?;
        let path = resolve_workspace_path(&self.workspace, &rel)?;

        // Gate 1: an opt-in deny rule (e.g. `Read(**/.env)`) keeps secrets out of
        // an exfiltration-prone turn — blocked before the file is ever read.
        if crate::permission::read_denied(&self.workspace, &path) {
            return Ok(format!("BLOCKED: a permission policy (deny) rule refused reading {rel}"));
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("cannot read `{rel}`: {e}"))?;

        let offset = usize::try_from(arguments.get("offset").and_then(Value::as_u64).unwrap_or(1))
            .unwrap_or(1)
            .max(1);
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(READ_DEFAULT_LIMIT);

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start = offset - 1;
        if start >= total {
            return Ok(format!("(file has {total} lines; offset {offset} is past the end)"));
        }
        let end = (start + limit).min(total);

        let mut out = String::new();
        for (i, line) in lines[start..end].iter().enumerate() {
            let _ = writeln!(out, "{:>6}\t{}", start + i + 1, line);
        }
        if end < total {
            let _ = writeln!(out, "... ({} more lines, {total} total)", total - end);
        }
        Ok(out)
    }
}

/// `list_files` — glob the workspace (respecting `.gitignore`), newest first.
pub struct ListFilesTool {
    /// Workspace root.
    pub workspace: PathBuf,
}

#[async_trait]
impl Tool for ListFilesTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_files".into(),
            description: "List files in the workspace matching a glob (respects .gitignore), most-recently-modified first.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern to match (default: '**/*' — all files)" }
                },
                "required": []
            }),
        }
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let pattern = arguments.get("pattern").and_then(Value::as_str).unwrap_or("**/*").to_owned();
        let base = self.workspace.clone();

        tokio::task::spawn_blocking(move || list_files_blocking(&base, &pattern))
            .await
            .map_err(|e| anyhow::anyhow!("list_files task panicked: {e}"))?
    }
}

/// The longest leading run of glob components that contain no glob metachars —
/// the concrete directory a pattern is rooted at. `dev/smooai/x/**/*.rs` →
/// `dev/smooai/x`. Lets the walk start deep instead of at the workspace root,
/// so a glob into one repo doesn't walk (and blow the budget on) a huge home.
fn glob_literal_prefix(glob: &str) -> String {
    let mut parts = Vec::new();
    for comp in glob.split('/') {
        if comp.is_empty() || comp.contains(['*', '?', '[', ']', '{', '}']) {
            break;
        }
        parts.push(comp);
    }
    parts.join("/")
}

fn list_files_blocking(base: &std::path::Path, pattern: &str) -> anyhow::Result<String> {
    // Accept an absolute-within-workspace pattern by making it workspace-relative
    // (globs match against paths relative to `base`). Absolute-but-outside ⇒
    // nothing to list. The agent naturally emits absolute paths (th-c89c2a).
    // Detect absoluteness with `Path`, not `starts_with('/')`: a Windows
    // absolute path is `D:\ws\src/*.rs`, so the string check never fired there
    // and an absolute pattern was treated as relative, matching nothing
    // (pearl th-a165b4). Globs are built with `/` — globset normalizes
    // separators when matching.
    // `has_root()` as well as `is_absolute()`: on Windows a drive-less rooted
    // path like `/etc/*` is NOT absolute, so `is_absolute()` alone would let it
    // fall through to the relative branch and quietly match nothing instead of
    // being refused as outside the workspace. On Unix the two are equivalent.
    let pattern_path = std::path::Path::new(pattern);
    let mut rel_pattern: String = if pattern_path.is_absolute() || pattern_path.has_root() {
        match pattern_path.strip_prefix(base) {
            Ok(stripped) => crate::util::to_slash(stripped),
            Err(_) => return Ok(format!("no files match `{pattern}` (path is outside the workspace)")),
        }
    } else {
        pattern.to_string()
    };
    if rel_pattern.is_empty() {
        rel_pattern = "**/*".to_string();
    } else if glob_literal_prefix(&rel_pattern) == rel_pattern {
        // A bare directory path (no glob chars) ⇒ list everything under it.
        rel_pattern = format!("{}/**/*", rel_pattern.trim_end_matches('/'));
    }

    let glob = Glob::new(&rel_pattern).map_err(|e| anyhow::anyhow!("invalid glob `{rel_pattern}`: {e}"))?;
    let mut gsb = GlobSetBuilder::new();
    gsb.add(glob);
    let set = gsb.build().map_err(|e| anyhow::anyhow!("invalid glob set: {e}"))?;

    // Root the walk at the pattern's literal prefix — a glob deep in a large
    // workspace then walks only that subtree instead of the whole home dir.
    let literal = glob_literal_prefix(&rel_pattern);
    let walk_root = if literal.is_empty() { base.to_path_buf() } else { base.join(&literal) };

    let mut matches: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    let mut examined = 0usize;
    let mut budget_hit = false;
    for entry in crate::walk::pruned_walk(&walk_root).flatten() {
        examined += 1;
        if examined > LIST_WALK_BUDGET {
            budget_hit = true;
            break;
        }
        let Some(ft) = entry.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(base) else { continue };
        if !set.is_match(rel) {
            continue;
        }
        let mtime = entry.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
        matches.push((rel.to_path_buf(), mtime));
    }

    matches.sort_by_key(|m| std::cmp::Reverse(m.1));
    let total = matches.len();
    if total == 0 {
        let hint = if budget_hit {
            " (walk budget reached — narrow the pattern or pick a subdirectory)"
        } else {
            ""
        };
        return Ok(format!("no files match `{pattern}`{hint}"));
    }
    let mut out = String::new();
    for (p, _) in matches.iter().take(LIST_CAP) {
        out.push_str(&p.display().to_string());
        out.push('\n');
    }
    if budget_hit {
        let _ = writeln!(
            out,
            "... (walk budget reached; results may be incomplete — narrow the pattern or pick a subdirectory)"
        );
    } else if total > LIST_CAP {
        let _ = writeln!(out, "... ({total} total, showing {LIST_CAP})");
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    async fn workspace_with_files() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join("src")).await.unwrap();
        tokio::fs::write(dir.path().join("src/main.rs"), "fn main() {}\nprintln!\n").await.unwrap();
        tokio::fs::write(dir.path().join("README.md"), "hello\n").await.unwrap();
        dir
    }

    /// Listings carry native path separators, so Windows reports
    /// `src\main.rs` where Unix reports `src/main.rs`. Normalize to `/` so the
    /// assertions read the same on both platforms (pearl th-a165b4) — the
    /// separator is not what these tests are about.
    fn norm(s: &str) -> String {
        s.replace('\\', "/")
    }

    #[tokio::test]
    async fn read_file_numbers_lines() {
        let dir = workspace_with_files().await;
        let tool = ReadFileTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = tool.execute(json!({"path": "src/main.rs"})).await.unwrap();
        assert!(out.contains("     1\tfn main() {}"), "{out}");
        assert!(out.contains("     2\tprintln!"), "{out}");
    }

    #[tokio::test]
    async fn read_file_honors_offset_and_limit() {
        let dir = workspace_with_files().await;
        let tool = ReadFileTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = tool.execute(json!({"path": "src/main.rs", "offset": 2, "limit": 1})).await.unwrap();
        assert!(out.contains("     2\tprintln!"), "{out}");
        assert!(!out.contains("fn main"), "offset should skip line 1: {out}");
    }

    #[tokio::test]
    async fn read_file_rejects_escape() {
        let dir = workspace_with_files().await;
        let tool = ReadFileTool {
            workspace: dir.path().to_path_buf(),
        };
        let err = tool.execute(json!({"path": "../../../etc/passwd"})).await.unwrap_err();
        assert!(err.to_string().contains("outside"), "{err}");
    }

    #[tokio::test]
    async fn list_files_matches_glob_newest_first() {
        let dir = workspace_with_files().await;
        let tool = ListFilesTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = norm(&tool.execute(json!({"pattern": "**/*.rs"})).await.unwrap());
        assert!(out.contains("src/main.rs"), "{out}");
        assert!(!out.contains("README.md"), "glob should exclude non-rs: {out}");
    }

    #[tokio::test]
    async fn list_files_default_lists_all() {
        let dir = workspace_with_files().await;
        let tool = ListFilesTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = norm(&tool.execute(json!({})).await.unwrap());
        assert!(out.contains("README.md") && out.contains("src/main.rs"), "{out}");
    }

    #[test]
    fn glob_literal_prefix_extracts_root() {
        assert_eq!(glob_literal_prefix("dev/smooai/x/**/*.rs"), "dev/smooai/x");
        assert_eq!(glob_literal_prefix("**/*.rs"), "");
        assert_eq!(glob_literal_prefix("src/*.rs"), "src");
        assert_eq!(glob_literal_prefix("dev/smoo-hub"), "dev/smoo-hub");
    }

    #[tokio::test]
    async fn list_files_accepts_absolute_path_inside_workspace() {
        // The agent emits an absolute path (the user named one) — it must resolve.
        let dir = workspace_with_files().await;
        let tool = ListFilesTool {
            workspace: dir.path().to_path_buf(),
        };
        let abs = format!("{}/src/*.rs", dir.path().display());
        let out = norm(&tool.execute(json!({ "pattern": abs })).await.unwrap());
        assert!(out.contains("src/main.rs"), "abs-in-workspace should match: {out}");
    }

    #[tokio::test]
    async fn list_files_bare_directory_lists_its_contents() {
        // A bare directory path (no glob chars) lists everything under it.
        let dir = workspace_with_files().await;
        let tool = ListFilesTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = norm(&tool.execute(json!({ "pattern": "src" })).await.unwrap());
        assert!(out.contains("src/main.rs"), "bare dir should list contents: {out}");
    }

    #[tokio::test]
    async fn list_files_absolute_outside_workspace_no_match() {
        let dir = workspace_with_files().await;
        let tool = ListFilesTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = tool.execute(json!({ "pattern": "/etc/*" })).await.unwrap();
        assert!(out.contains("outside the workspace"), "abs-outside must be refused: {out}");
    }
}

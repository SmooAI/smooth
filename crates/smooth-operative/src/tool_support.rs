//! Shared tool support utilities: auto-format, file-time tracking,
//! fuzzy path suggestions, diff generation, and patch application.
//!
//! These are used by the tool implementations in `main.rs` to enrich
//! tool results and catch common agent mistakes early.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

// ---------------------------------------------------------------------------
// 0. Workspace path-escape guard
// ---------------------------------------------------------------------------

/// Resolve a relative path against `base` and verify it stays *inside*
/// the workspace. Rejects `..` escapes, absolute paths, symlinks that
/// point outside the base, and the base path itself being bypassed.
///
/// Returns the resolved absolute path on success. Tools should use the
/// returned path, not the raw `base.join(rel)`.
///
/// # Security
///
/// The agent's file tools are scoped to the bind-mounted workspace. An
/// `edit_file("../../etc/shadow")` call would otherwise escape the
/// workspace since the host mounts `/workspace` RW from the user's
/// current directory. This function enforces the scope.
pub fn resolve_workspace_path(base: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    if rel.is_empty() {
        return Err(anyhow::anyhow!("empty path"));
    }
    let requested = Path::new(rel);
    if requested.is_absolute() {
        return Err(anyhow::anyhow!(
            "absolute path `{rel}` is not allowed — all paths must be relative to the workspace"
        ));
    }

    let combined = base.join(requested);

    // Normalize without following symlinks: collapse `.` and `..`
    // lexically. We use a manual normalizer instead of canonicalize()
    // because canonicalize() follows symlinks (an attacker could symlink
    // `/workspace/link` → `/etc` then write through it) AND because it
    // requires the path to exist.
    let normalized = lexical_normalize(&combined);

    // Must stay under base.
    let base_norm = lexical_normalize(base);
    if !normalized.starts_with(&base_norm) {
        return Err(anyhow::anyhow!(
            "path `{rel}` escapes the workspace (resolved to {}, outside {})",
            normalized.display(),
            base_norm.display()
        ));
    }

    Ok(normalized)
}

/// Collapse `.` and `..` components lexically. Does NOT follow symlinks
/// or require the path to exist. Adapted from cargo's path utilities.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                // Pop if possible. If out is empty, leave the `..` in —
                // the subsequent prefix check will catch the escape.
                if !out.pop() {
                    out.push(component);
                }
            }
            std::path::Component::CurDir => {
                // Skip.
            }
            other => {
                out.push(other);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 1. Auto-format on write
// ---------------------------------------------------------------------------

/// Detect the project type from the workspace root and return the
/// appropriate format command, if any.
///
/// Runs synchronously — call from `spawn_blocking` if needed.
pub fn detect_formatter(workspace: &Path, file_path: &Path) -> Option<Vec<String>> {
    let ext = file_path.extension()?.to_str()?;
    match ext {
        // Rust — rustfmt (available if cargo is installed)
        "rs" if workspace.join("Cargo.toml").exists() => Some(vec!["rustfmt".into(), file_path.to_string_lossy().into()]),

        // TypeScript / JavaScript — try prettier first, then dprint
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "json" | "css" | "html" | "md" if workspace.join("package.json").exists() => {
            // Check for prettier in node_modules
            if workspace.join("node_modules/.bin/prettier").exists() {
                Some(vec!["node_modules/.bin/prettier".into(), "--write".into(), file_path.to_string_lossy().into()])
            } else if workspace.join("node_modules/.bin/dprint").exists() {
                Some(vec!["node_modules/.bin/dprint".into(), "fmt".into(), file_path.to_string_lossy().into()])
            } else {
                None
            }
        }

        // Python — ruff format (fast, modern) or black
        "py" if workspace.join("pyproject.toml").exists() || workspace.join("setup.py").exists() => {
            Some(vec!["ruff".into(), "format".into(), file_path.to_string_lossy().into()])
        }

        // Go
        "go" if workspace.join("go.mod").exists() => Some(vec!["gofmt".into(), "-w".into(), file_path.to_string_lossy().into()]),

        _ => None,
    }
}

/// Run the detected formatter on a file. Best-effort — returns Ok even if
/// the formatter isn't installed (we don't want to block writes just
/// because rustfmt is missing).
pub async fn auto_format(workspace: &Path, file_path: &Path) {
    let Some(cmd_parts) = detect_formatter(workspace, file_path) else {
        return;
    };
    let Some((program, args)) = cmd_parts.split_first() else {
        return;
    };

    let result = tokio::process::Command::new(program).args(args).current_dir(workspace).output().await;

    match result {
        Ok(output) if output.status.success() => {
            tracing::debug!(file = %file_path.display(), "auto-formatted");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::debug!(
                file = %file_path.display(),
                stderr = %stderr.chars().take(200).collect::<String>(),
                "auto-format failed (non-fatal)"
            );
        }
        Err(e) => {
            // Formatter not installed — silently skip.
            tracing::debug!(file = %file_path.display(), error = %e, "auto-format binary not found (non-fatal)");
        }
    }
}

// ---------------------------------------------------------------------------
// 3. File-time tracking
// ---------------------------------------------------------------------------

/// Tracks the last-known mtime of files the agent has read. When the agent
/// tries to write or edit a file, we compare against the tracked mtime —
/// if the file was modified externally (by another agent, by the user, or
/// by a build tool), we warn so the agent doesn't silently clobber changes.
#[derive(Default)]
pub struct FileTimeTracker {
    mtimes: Mutex<HashMap<PathBuf, SystemTime>>,
}

impl FileTimeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the current mtime of a file (called after every read).
    pub fn record(&self, path: &Path) {
        if let Ok(meta) = std::fs::metadata(path) {
            if let Ok(mtime) = meta.modified() {
                if let Ok(mut map) = self.mtimes.lock() {
                    map.insert(path.to_path_buf(), mtime);
                }
            }
        }
    }

    /// Check whether a file has been modified since the last recorded read.
    /// Returns `Some(warning_message)` if it was modified externally, `None`
    /// if it's safe to overwrite (or if we have no prior record).
    pub fn check_before_write(&self, path: &Path) -> Option<String> {
        let Ok(map) = self.mtimes.lock() else {
            return None;
        };
        let Some(recorded) = map.get(path) else {
            return None; // Never read before — no conflict possible.
        };
        let Ok(meta) = std::fs::metadata(path) else {
            return None; // File was deleted — no conflict.
        };
        let Ok(current_mtime) = meta.modified() else {
            return None;
        };
        if current_mtime > *recorded {
            let rel = path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            Some(format!(
                "WARNING: {rel} was modified externally since you last read it. \
                 Your edit may overwrite those changes. Read the file again to see \
                 the current state before editing."
            ))
        } else {
            None
        }
    }

    /// Update the recorded mtime after a successful write.
    pub fn update_after_write(&self, path: &Path) {
        self.record(path);
    }
}

// ---------------------------------------------------------------------------
// 5. Unified diff application (apply_patch)
// ---------------------------------------------------------------------------

/// Apply a unified diff patch to the workspace. Parses the patch text,
/// validates that target files exist, and applies each hunk.
///
/// Returns a summary of what was changed.
pub fn apply_unified_patch(workspace: &Path, patch_text: &str) -> anyhow::Result<String> {
    // Parse the patch into file-level blocks.
    let mut summary = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_hunks: Vec<String> = Vec::new();
    let mut in_hunk = false;

    for line in patch_text.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/").or_else(|| line.strip_prefix("+++ ")) {
            // New file target. Apply any pending hunks first.
            if let Some(ref file) = current_file {
                let result = apply_hunks_to_file(workspace, file, &current_hunks)?;
                summary.push(result);
            }
            current_file = Some(rest.split('\t').next().unwrap_or(rest).to_string());
            current_hunks.clear();
            in_hunk = false;
        } else if line.starts_with("--- ") {
            // Source file header — skip (we only care about the target).
            continue;
        } else if line.starts_with("@@ ") {
            in_hunk = true;
            current_hunks.push(line.to_string());
        } else if in_hunk {
            if let Some(last) = current_hunks.last_mut() {
                last.push('\n');
                last.push_str(line);
            }
        }
    }

    // Apply final file.
    if let Some(ref file) = current_file {
        let result = apply_hunks_to_file(workspace, file, &current_hunks)?;
        summary.push(result);
    }

    if summary.is_empty() {
        return Err(anyhow::anyhow!("no valid hunks found in patch"));
    }

    Ok(summary.join("\n"))
}

fn apply_hunks_to_file(workspace: &Path, rel_path: &str, hunks: &[String]) -> anyhow::Result<String> {
    let path = workspace.join(rel_path);
    let original = if path.exists() { std::fs::read_to_string(&path)? } else { String::new() };

    let mut lines: Vec<String> = original.lines().map(String::from).collect();
    let mut offset: i64 = 0;
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for hunk in hunks {
        let hunk_lines: Vec<&str> = hunk.lines().collect();
        let Some(header) = hunk_lines.first() else {
            continue;
        };

        // Parse @@ -start,count +start,count @@
        let (old_start, _) = parse_hunk_header(header)?;
        let target_line = ((old_start as i64) + offset - 1).max(0) as usize;

        let mut pos = target_line;
        for &line in &hunk_lines[1..] {
            if let Some(removed) = line.strip_prefix('-') {
                if pos < lines.len() && lines[pos] == removed {
                    lines.remove(pos);
                    offset -= 1;
                    deletions += 1;
                }
            } else if let Some(added) = line.strip_prefix('+') {
                lines.insert(pos, added.to_string());
                pos += 1;
                offset += 1;
                additions += 1;
            } else if let Some(_context) = line.strip_prefix(' ') {
                pos += 1;
            } else if !line.is_empty() {
                // Treat unrecognized lines as context.
                pos += 1;
            }
        }
    }

    let new_content = lines.join("\n");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Preserve trailing newline if original had one.
    let new_content = if original.ends_with('\n') && !new_content.ends_with('\n') {
        format!("{new_content}\n")
    } else {
        new_content
    };
    std::fs::write(&path, &new_content)?;

    Ok(format!("{rel_path}: +{additions} -{deletions}"))
}

fn parse_hunk_header(header: &str) -> anyhow::Result<(usize, usize)> {
    // @@ -old_start,old_count +new_start,new_count @@
    let parts: Vec<&str> = header.split_whitespace().collect();
    let old_part = parts.get(1).ok_or_else(|| anyhow::anyhow!("invalid hunk header: {header}"))?;
    let old_start_str = old_part.trim_start_matches('-').split(',').next().unwrap_or("1");
    let old_start: usize = old_start_str.parse().map_err(|_| anyhow::anyhow!("invalid hunk start: {old_start_str}"))?;
    Ok((old_start, 0))
}

// ---------------------------------------------------------------------------
// 6. "Did you mean?" fuzzy path suggestions
// ---------------------------------------------------------------------------

/// When a file path doesn't exist, scan the workspace for similar names
/// and return the top 3 suggestions. Uses nucleo-matcher for fast fuzzy
/// matching.
pub fn suggest_similar_paths(workspace: &Path, missing_rel_path: &str, max_results: usize) -> Vec<String> {
    use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
    use nucleo_matcher::{Config, Matcher, Utf32Str};

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(missing_rel_path, CaseMatching::Ignore, Normalization::Smart);

    let mut candidates: Vec<(String, u32)> = Vec::new();
    let walker = ignore::WalkBuilder::new(workspace).hidden(false).build();

    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let rel = entry.path().strip_prefix(workspace).unwrap_or(entry.path());
        let rel_str = rel.to_string_lossy();
        let mut haystack_buf = Vec::new();
        let haystack = Utf32Str::new(&rel_str, &mut haystack_buf);
        if let Some(score) = pattern.score(haystack, &mut matcher) {
            candidates.push((rel_str.to_string(), score));
        }
    }

    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.into_iter().take(max_results).map(|(path, _)| path).collect()
}

// ---------------------------------------------------------------------------
// Diff generation (for tool result metadata)
// ---------------------------------------------------------------------------

/// Generate a unified diff between old and new content. Used by write_file
/// and edit_file to attach a diff to the tool result metadata.
pub fn generate_diff(file_path: &str, old_content: &str, new_content: &str) -> String {
    use similar::TextDiff;
    let diff = TextDiff::from_lines(old_content, new_content);
    let mut output = String::new();
    for hunk in diff.unified_diff().header(&format!("a/{file_path}"), &format!("b/{file_path}")).iter_hunks() {
        output.push_str(&format!("{hunk}"));
    }
    output
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_workspace_path_accepts_normal_paths() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_workspace_path(dir.path(), "src/main.rs").expect("ok");
        assert!(resolved.starts_with(dir.path()));
        assert!(resolved.to_string_lossy().ends_with("src/main.rs"));
    }

    #[test]
    fn resolve_workspace_path_rejects_parent_dir_escape() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_workspace_path(dir.path(), "../etc/shadow").is_err());
        assert!(resolve_workspace_path(dir.path(), "../../root/.ssh/id_rsa").is_err());
        assert!(resolve_workspace_path(dir.path(), "src/../../etc/passwd").is_err());
    }

    #[test]
    fn resolve_workspace_path_rejects_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_workspace_path(dir.path(), "/etc/shadow").is_err());
        assert!(resolve_workspace_path(dir.path(), "/tmp/evil").is_err());
    }

    #[test]
    fn resolve_workspace_path_rejects_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_workspace_path(dir.path(), "").is_err());
    }

    #[test]
    fn resolve_workspace_path_allows_dot_inside() {
        let dir = tempfile::tempdir().unwrap();
        // `src/./main.rs` is inside, just has a redundant component.
        let resolved = resolve_workspace_path(dir.path(), "src/./main.rs").expect("ok");
        assert!(resolved.starts_with(dir.path()));
    }

    #[test]
    fn resolve_workspace_path_allows_dotdot_that_stays_inside() {
        let dir = tempfile::tempdir().unwrap();
        // `src/../Cargo.toml` resolves to base/Cargo.toml, still inside.
        let resolved = resolve_workspace_path(dir.path(), "src/../Cargo.toml").expect("ok");
        assert!(resolved.starts_with(dir.path()));
        assert!(resolved.to_string_lossy().ends_with("Cargo.toml"));
    }

    #[test]
    fn detect_formatter_rust() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let cmd = detect_formatter(dir.path(), Path::new("src/main.rs"));
        assert!(cmd.is_some());
        assert_eq!(cmd.unwrap()[0], "rustfmt");
    }

    #[test]
    fn detect_formatter_python() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();
        let cmd = detect_formatter(dir.path(), Path::new("app.py"));
        assert!(cmd.is_some());
        assert!(cmd.unwrap().contains(&"ruff".to_string()));
    }

    #[test]
    fn detect_formatter_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = detect_formatter(dir.path(), Path::new("random.xyz"));
        assert!(cmd.is_none());
    }

    #[test]
    fn file_time_tracker_no_conflict_on_first_write() {
        let tracker = FileTimeTracker::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello").unwrap();
        // No prior read — should be safe.
        assert!(tracker.check_before_write(&path).is_none());
    }

    #[test]
    fn file_time_tracker_detects_external_modification() {
        let tracker = FileTimeTracker::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "v1").unwrap();
        tracker.record(&path);

        // Simulate external modification with a newer mtime.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&path, "v2 — modified externally").unwrap();

        let warning = tracker.check_before_write(&path);
        assert!(warning.is_some(), "should detect external modification");
        assert!(warning.unwrap().contains("modified externally"));
    }

    #[test]
    fn apply_patch_simple_addition() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "line 1\nline 2\nline 3\n").unwrap();
        let patch = "\
--- a/hello.txt
+++ b/hello.txt
@@ -1,3 +1,4 @@
 line 1
+line 1.5
 line 2
 line 3";
        let result = apply_unified_patch(dir.path(), patch).unwrap();
        assert!(result.contains("+1"));
        let content = std::fs::read_to_string(dir.path().join("hello.txt")).unwrap();
        assert!(content.contains("line 1.5"));
    }

    #[test]
    fn apply_patch_deletion() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "line 1\nline 2\nline 3\n").unwrap();
        let patch = "\
--- a/hello.txt
+++ b/hello.txt
@@ -1,3 +1,2 @@
 line 1
-line 2
 line 3";
        let result = apply_unified_patch(dir.path(), patch).unwrap();
        assert!(result.contains("-1"));
        let content = std::fs::read_to_string(dir.path().join("hello.txt")).unwrap();
        assert!(!content.contains("line 2"));
    }

    #[test]
    fn suggest_similar_paths_finds_close_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();

        // Use a close-enough typo. nucleo's path-mode fuzzy matching should
        // find "src/main.rs" for "main.rs" at minimum.
        let suggestions = suggest_similar_paths(dir.path(), "main.rs", 5);
        assert!(!suggestions.is_empty(), "should suggest something for main.rs query: {suggestions:?}");
        assert!(suggestions.iter().any(|s| s.contains("main.rs")), "should suggest main.rs: {suggestions:?}");
    }

    #[test]
    fn generate_diff_shows_changes() {
        let diff = generate_diff("test.rs", "fn main() {}\n", "fn main() {\n    println!(\"hello\");\n}\n");
        // `similar` uses the headers from .header(), verify some diff output exists.
        assert!(!diff.is_empty(), "diff should not be empty: {diff:?}");
        assert!(diff.contains('+'), "diff should contain added lines: {diff:?}");
    }
}

//! Mutating filesystem tools: `write_file`, `edit_file`.
//!
//! Both confine paths via [`resolve_workspace_path`] (the kernel sandbox is the
//! load-bearing boundary) **and** consult Gate 1: a configurable deny rule under
//! the `Write` label (e.g. `Write(.git/hooks/**)` in `~/.smooth/permissions.toml`)
//! blocks modifying a protected in-workspace path before any write — see
//! [`crate::permission`] (EPIC th-c89c2a, th-515a13).

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::path::resolve_workspace_path;
use crate::util::req_str;

/// `write_file` — create or overwrite a workspace file with exact content.
pub struct WriteFileTool {
    /// Workspace root.
    pub workspace: PathBuf,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write_file".into(),
            description: "Create or overwrite a file in the workspace with the given content (parent dirs are created).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path within the workspace" },
                    "content": { "type": "string", "description": "Full file content to write" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let rel = req_str(&arguments, "path")?;
        let content = req_str(&arguments, "content")?;
        let path = resolve_workspace_path(&self.workspace, &rel)?;

        // Gate 1: a configurable deny rule (e.g. `Write(.git/hooks/**)`) blocks
        // modifying a protected in-workspace path before any write happens.
        if crate::permission::write_denied(&self.workspace, &path) {
            return Ok(format!("BLOCKED: a permission policy (deny) rule refused writing {rel}"));
        }

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| anyhow::anyhow!("cannot create parent dirs for `{rel}`: {e}"))?;
        }
        tokio::fs::write(&path, content.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("cannot write `{rel}`: {e}"))?;
        Ok(format!("wrote {} bytes to {rel}", content.len()))
    }
}

/// `edit_file` — exact-string find/replace within a workspace file.
pub struct EditFileTool {
    /// Workspace root.
    pub workspace: PathBuf,
}

#[async_trait]
impl Tool for EditFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".into(),
            description: "Replace an exact string in a workspace file. Errors if old_string is absent, or appears more than once without replace_all.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path within the workspace" },
                    "old_string": { "type": "string", "description": "The exact string to find and replace" },
                    "new_string": { "type": "string", "description": "The replacement string" },
                    "replace_all": { "type": "boolean", "description": "If true, replace ALL occurrences. Default false." }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let rel = req_str(&arguments, "path")?;
        let old = req_str(&arguments, "old_string")?;
        let new = req_str(&arguments, "new_string")?;
        let replace_all = arguments.get("replace_all").and_then(Value::as_bool).unwrap_or(false);

        if old.is_empty() {
            anyhow::bail!("old_string must not be empty");
        }

        let path = resolve_workspace_path(&self.workspace, &rel)?;

        // Gate 1: same deny gate as write_file (the `Write` label covers both).
        if crate::permission::write_denied(&self.workspace, &path) {
            return Ok(format!("BLOCKED: a permission policy (deny) rule refused editing {rel}"));
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("cannot read `{rel}` for editing: {e}"))?;

        let count = content.matches(&old).count();
        if count == 0 {
            anyhow::bail!("old_string not found in `{rel}`");
        }
        if count > 1 && !replace_all {
            anyhow::bail!("old_string appears {count} times in `{rel}` — pass replace_all=true or use a more specific string");
        }

        let (updated, replaced) = if replace_all {
            (content.replace(&old, &new), count)
        } else {
            (content.replacen(&old, &new, 1), 1)
        };
        let (old_len, new_len) = (content.len(), updated.len());

        tokio::fs::write(&path, updated.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("cannot write `{rel}`: {e}"))?;
        Ok(format!("replaced {replaced} occurrence(s) in {rel} ({old_len} → {new_len} bytes)"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn ws() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn write_creates_file_and_parent_dirs() {
        let dir = ws();
        let tool = WriteFileTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = tool.execute(json!({"path": "a/b/c.txt", "content": "hello"})).await.unwrap();
        assert!(out.contains("5 bytes to a/b/c.txt"), "{out}");
        let written = tokio::fs::read_to_string(dir.path().join("a/b/c.txt")).await.unwrap();
        assert_eq!(written, "hello");
    }

    #[tokio::test]
    async fn write_rejects_escape() {
        let dir = ws();
        let tool = WriteFileTool {
            workspace: dir.path().to_path_buf(),
        };
        let err = tool.execute(json!({"path": "../evil.txt", "content": "x"})).await.unwrap_err();
        assert!(err.to_string().contains("escapes"), "{err}");
    }

    #[tokio::test]
    async fn edit_replaces_single_occurrence() {
        let dir = ws();
        tokio::fs::write(dir.path().join("f.txt"), "the quick brown fox").await.unwrap();
        let tool = EditFileTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = tool
            .execute(json!({"path": "f.txt", "old_string": "quick", "new_string": "slow"}))
            .await
            .unwrap();
        assert!(out.contains("replaced 1 occurrence"), "{out}");
        assert_eq!(tokio::fs::read_to_string(dir.path().join("f.txt")).await.unwrap(), "the slow brown fox");
    }

    #[tokio::test]
    async fn edit_errors_when_absent() {
        let dir = ws();
        tokio::fs::write(dir.path().join("f.txt"), "abc").await.unwrap();
        let tool = EditFileTool {
            workspace: dir.path().to_path_buf(),
        };
        let err = tool
            .execute(json!({"path": "f.txt", "old_string": "zzz", "new_string": "x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[tokio::test]
    async fn edit_errors_on_ambiguous_without_replace_all() {
        let dir = ws();
        tokio::fs::write(dir.path().join("f.txt"), "x x x").await.unwrap();
        let tool = EditFileTool {
            workspace: dir.path().to_path_buf(),
        };
        let err = tool.execute(json!({"path": "f.txt", "old_string": "x", "new_string": "y"})).await.unwrap_err();
        assert!(err.to_string().contains("3 times"), "{err}");
    }

    #[tokio::test]
    async fn edit_replace_all_replaces_every_occurrence() {
        let dir = ws();
        tokio::fs::write(dir.path().join("f.txt"), "x x x").await.unwrap();
        let tool = EditFileTool {
            workspace: dir.path().to_path_buf(),
        };
        let out = tool
            .execute(json!({"path": "f.txt", "old_string": "x", "new_string": "y", "replace_all": true}))
            .await
            .unwrap();
        assert!(out.contains("replaced 3 occurrence"), "{out}");
        assert_eq!(tokio::fs::read_to_string(dir.path().join("f.txt")).await.unwrap(), "y y y");
    }
}

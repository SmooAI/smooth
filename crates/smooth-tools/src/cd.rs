//! The `cd` tool — the agent scopes its own working directory.
//!
//! When the user says "work on the smoo-hub repo", the model can call `cd` to
//! point the file tools (read/list/grep/write/bash) at that subdirectory for
//! the rest of the conversation. Confinement is enforced by [`SessionCwd::set`]
//! — a `cd` can only ever land on an existing directory *under the root*.
//!
//! The tool is constructed per-turn by the daemon's ToolProvider, which injects
//! the shared [`SessionCwd`] store + the turn's `conversation_id` + the current
//! cwd. The current cwd is baked into the schema description so the model is
//! reminded where it is on every turn (no per-turn persona seam needed).

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::cwd::SessionCwd;

/// `cd` — change the conversation's working directory to a subdirectory of the
/// workspace root.
pub struct CdTool {
    cwd: SessionCwd,
    /// The conversation this turn belongs to — the key into the cwd store.
    session: String,
    /// The cwd at build time, surfaced in the schema so the model knows where
    /// it currently is.
    current: PathBuf,
}

impl CdTool {
    #[must_use]
    pub fn new(cwd: SessionCwd, session: String, current: PathBuf) -> Self {
        Self { cwd, session, current }
    }
}

#[async_trait]
impl Tool for CdTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "cd".into(),
            description: format!(
                "Change the working directory that the file tools (read_file, list_files, grep, write_file, edit_file, bash) \
                 operate under, for the rest of this conversation. Current working directory: {}. The path must be an existing \
                 directory inside the workspace root {} — you cannot escape above the root. Pass an empty path or \"~\" to reset \
                 back to the root.",
                self.current.display(),
                self.cwd.root().display()
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to switch to — absolute (inside the root) or relative to the current directory. Empty or \"~\" resets to the root."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let path = arguments.get("path").and_then(Value::as_str).unwrap_or_default();
        let resolved = self.cwd.set(&self.session, path)?;
        Ok(format!("Working directory is now {}", resolved.display()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, SessionCwd) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo")).unwrap();
        let cwd = SessionCwd::new(tmp.path().to_path_buf());
        (tmp, cwd)
    }

    #[tokio::test]
    async fn cd_sets_the_session_cwd() {
        let (_tmp, cwd) = fixture();
        let tool = CdTool::new(cwd.clone(), "conv-1".into(), cwd.root().to_path_buf());
        let out = tool.execute(json!({ "path": "repo" })).await.unwrap();
        assert!(out.contains("repo"), "{out}");
        assert_eq!(cwd.get("conv-1"), cwd.root().join("repo").canonicalize().unwrap());
    }

    #[tokio::test]
    async fn cd_outside_root_errors() {
        let (_tmp, cwd) = fixture();
        let tool = CdTool::new(cwd.clone(), "conv-1".into(), cwd.root().to_path_buf());
        assert!(tool.execute(json!({ "path": "../escape" })).await.is_err());
    }

    #[tokio::test]
    async fn cd_empty_resets_to_root() {
        let (_tmp, cwd) = fixture();
        cwd.set("conv-1", "repo").unwrap();
        let tool = CdTool::new(cwd.clone(), "conv-1".into(), cwd.get("conv-1"));
        tool.execute(json!({ "path": "" })).await.unwrap();
        assert_eq!(cwd.get("conv-1"), cwd.root());
    }

    #[test]
    fn schema_surfaces_current_dir() {
        let (_tmp, cwd) = fixture();
        let tool = CdTool::new(cwd.clone(), "conv-1".into(), cwd.root().join("repo"));
        let schema = tool.schema();
        assert_eq!(schema.name, "cd");
        assert!(schema.description.contains("repo"), "current dir is in the description");
    }
}

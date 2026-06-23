//! `bash` — run a shell command in the workspace.
//!
//! ⚠️ **Pre-sandbox.** This runs `sh -c <command>` with the workspace as its
//! working directory, but a shell can `cd` and touch anything the daemon user
//! can — workspace confinement does NOT apply to bash. The kernel OS-sandbox
//! that actually bounds shell execution is Phase 3 of EPIC th-c89c2a. Until
//! then this is acceptable only because the daemon is single-trusted-user on
//! loopback. When the sandbox lands, this spawn must go through the
//! non-bypassable `SandboxedCommand` path (P0 hardening).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::util::req_str;

/// Max bytes returned per stream before truncation.
const OUTPUT_CAP: usize = 50_000;

/// `bash` tool — shell execution rooted at the workspace.
pub struct BashTool {
    /// Working directory the command starts in.
    pub workspace: PathBuf,
}

#[async_trait]
impl Tool for BashTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash".into(),
            description: "Run a shell command (sh -c) with the workspace as the working directory. Returns exit code, stdout, stderr.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to run" },
                    "timeout": { "type": "integer", "description": "Optional: max seconds before the command is killed" }
                },
                "required": ["command"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        false
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let command = req_str(&arguments, "command")?;
        let timeout_secs = arguments.get("timeout").and_then(Value::as_u64);

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&command)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true); // so a timeout actually kills the child

        let child = cmd.spawn().map_err(|e| anyhow::anyhow!("failed to spawn shell: {e}"))?;

        let output = match timeout_secs {
            Some(secs) => match tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output()).await {
                Ok(result) => result.map_err(|e| anyhow::anyhow!("shell error: {e}"))?,
                Err(_) => return Ok(format!("command timed out after {secs}s and was killed")),
            },
            None => child.wait_with_output().await.map_err(|e| anyhow::anyhow!("shell error: {e}"))?,
        };

        let code = output.status.code().map_or_else(|| "killed by signal".to_owned(), |c| c.to_string());
        let stdout = truncate(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate(&String::from_utf8_lossy(&output.stderr));
        Ok(format!("exit code: {code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"))
    }
}

fn truncate(s: &str) -> String {
    if s.len() <= OUTPUT_CAP {
        return s.to_owned();
    }
    // Cut on a char boundary at or below the cap.
    let mut end = OUTPUT_CAP;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... (truncated, {} bytes total)", &s[..end], s.len())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn tool() -> (tempfile::TempDir, BashTool) {
        let dir = tempfile::tempdir().unwrap();
        let tool = BashTool {
            workspace: dir.path().to_path_buf(),
        };
        (dir, tool)
    }

    #[tokio::test]
    async fn runs_and_captures_stdout() {
        let (_dir, tool) = tool();
        let out = tool.execute(json!({"command": "echo hello"})).await.unwrap();
        assert!(out.contains("exit code: 0"), "{out}");
        assert!(out.contains("hello"), "{out}");
    }

    #[tokio::test]
    async fn nonzero_exit_is_reported() {
        let (_dir, tool) = tool();
        let out = tool.execute(json!({"command": "exit 7"})).await.unwrap();
        assert!(out.contains("exit code: 7"), "{out}");
    }

    #[tokio::test]
    async fn runs_in_the_workspace_dir() {
        let (dir, tool) = tool();
        // Writing via the shell lands in the workspace.
        let out = tool.execute(json!({"command": "echo data > made.txt"})).await.unwrap();
        assert!(out.contains("exit code: 0"), "{out}");
        assert!(dir.path().join("made.txt").exists(), "file should be created in workspace");
    }

    #[tokio::test]
    async fn timeout_kills_long_command() {
        let (_dir, tool) = tool();
        let out = tool.execute(json!({"command": "sleep 5", "timeout": 1})).await.unwrap();
        assert!(out.contains("timed out"), "{out}");
    }
}

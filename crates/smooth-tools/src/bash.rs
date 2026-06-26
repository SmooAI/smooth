//! `bash` — run a shell command, kernel-sandboxed.
//!
//! The subprocess is built **only** through [`SandboxedCommand`] (P0: there is
//! no unsandboxed spawn path). On macOS the command runs inside a Seatbelt
//! profile that confines filesystem writes to the workspace + temp and denies
//! reads of `~/.ssh` / `~/.aws` / etc. On Linux the kernel sandbox is not yet
//! implemented (bubblewrap+Landlock is TODO) and the command falls back to an
//! unsandboxed shell with a loud warning — acceptable only for the
//! single-trusted-user loopback daemon. See [`crate::sandbox`].

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
    /// When set (`host:port`), the shell's egress is forced through this
    /// loopback proxy and direct off-box network is kernel-denied (see
    /// [`crate::sandbox::SandboxPolicy::with_proxy`]). `None` = unrestricted.
    pub proxy: Option<String>,
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

        // Hard-deny circuit-breakers (rm -rf /, fork bombs, curl|sh, …) before we
        // ever spawn. The kernel sandbox is still the load-bearing boundary; this
        // is cheap defense-in-depth, and the only deny gate on the operator
        // local-flavor path (which doesn't install the bespoke permission engine).
        if crate::guard::is_circuit_breaker(&command) {
            return Ok(format!(
                "BLOCKED: refused to run a circuit-breaker command (catastrophic — e.g. `rm -rf /`, fork bomb, `curl … | sh`): {command}"
            ));
        }

        // The ONLY shell-spawn path: through the kernel sandbox (P0).
        let mut policy = crate::sandbox::SandboxPolicy::for_workspace(self.workspace.clone());
        if let Some(addr) = &self.proxy {
            policy = policy.with_proxy(addr.clone());
        }
        let mut cmd = crate::sandbox::SandboxedCommand::shell(&policy, &command).into_command();
        cmd.current_dir(&self.workspace)
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
            proxy: None,
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

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn proxy_bash_tool_routes_egress_through_the_proxy() {
        // With a proxy configured, the tool's shell sees HTTP_PROXY pointing at
        // it (the macos_profile also denies direct egress — see sandbox tests).
        let dir = tempfile::tempdir().unwrap();
        let tool = BashTool {
            workspace: dir.path().to_path_buf(),
            proxy: Some("127.0.0.1:3128".into()),
        };
        let out = tool.execute(json!({"command": "echo PROXY=$HTTP_PROXY"})).await.unwrap();
        assert!(out.contains("exit code: 0"), "{out}");
        assert!(out.contains("PROXY=http://127.0.0.1:3128"), "egress proxy env reaches the shell: {out}");
    }
}

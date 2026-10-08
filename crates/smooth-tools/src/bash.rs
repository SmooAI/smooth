//! `bash` — run a shell command as the user (kernel sandbox opt-in).
//!
//! The subprocess is built **only** through [`SandboxedCommand`] — the single
//! spawn point where the sandbox mode and the env scrub are applied. By default
//! (pearl th-efbab1) that is a plain user subprocess: Big Smooth is a personal
//! agent acting as its user, and the sandbox broke `ssh` / `git fetch`. With
//! `SMOOTH_SANDBOX=1` the command runs inside a Seatbelt profile on macOS that
//! denies reads/writes of `~/.ssh` / `~/.aws` / etc. and git-hook re-entry (no
//! kernel sandbox exists on Linux/Windows yet — th-08e05a). See
//! [`crate::sandbox`].

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
    /// When set (`host:port`), the shell's `HTTP(S)_PROXY` point at this
    /// loopback proxy; with the opt-in sandbox on macOS, direct off-box network
    /// is also kernel-denied (see [`crate::sandbox::SandboxPolicy::with_proxy`]).
    /// `None` = unrestricted.
    pub proxy: Option<String>,
    /// PATH from the attached coding client, when available.
    pub user_path: Option<std::ffi::OsString>,
}

#[async_trait]
impl Tool for BashTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash".into(),
            // Name the interpreter accurately per platform. Told "sh -c" on a
            // Windows host, the model writes POSIX syntax that `cmd` rejects,
            // then reads the resulting error as its own mistake rather than a
            // shell mismatch and retries the same thing.
            description: if cfg!(target_os = "windows") {
                "Run a shell command via `cmd /C` — the Windows command shell, NOT bash. Use Windows syntax (`dir`, `type`, `%VAR%`, `&&`); \
                 POSIX-isms like pipes into `grep`, `$VAR`, or `&&` chains with Unix tools will fail. The workspace is the working \
                 directory. Returns exit code, stdout, stderr."
            } else {
                "Run a shell command (sh -c) with the workspace as the working directory. Returns exit code, stdout, stderr."
            }
            .into(),
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

    fn timeout(&self) -> Option<Duration> {
        // The engine bounds a tool call at 120s by default (core 1.14.2). A shell
        // command legitimately runs longer (builds, test suites), and the call's
        // own `timeout` argument is the deadline that kills the child; the
        // engine's would only drop the future. So defer to it entirely.
        Some(smooth_operator::tool::NO_TOOL_TIMEOUT)
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let command = req_str(&arguments, "command")?;
        let timeout_secs = arguments.get("timeout").and_then(Value::as_u64);

        // Hard-deny circuit-breakers (rm -rf /, fork bombs, curl|sh, …) before we
        // ever spawn. Cheap defense-in-depth that holds whether or not the
        // opt-in kernel sandbox is on.
        if crate::guard::is_circuit_breaker(&command) {
            return Ok(format!(
                "BLOCKED: refused to run a circuit-breaker command (catastrophic — e.g. `rm -rf /`, fork bomb, `curl … | sh`): {command}"
            ));
        }

        // Gate 1: a configurable deny rule (`~/.smooth/permissions.toml`) blocks
        // the command deterministically before spawn — every subcommand of a
        // compound command is judged, so `ls && rm -rf ~` is caught on the `rm`.
        if crate::permission::bash_denied(&command) {
            return Ok(format!("BLOCKED: a permission policy (deny) rule refused this command: {command}"));
        }

        // The ONLY shell-spawn path: `SandboxedCommand`, which applies the
        // sandbox mode (`SMOOTH_SANDBOX`, default off) and the env scrub.
        let mut policy = crate::sandbox::SandboxPolicy::for_workspace(self.workspace.clone());
        if let Some(addr) = &self.proxy {
            policy = policy.with_proxy(addr.clone());
        }
        let mut cmd = crate::sandbox::SandboxedCommand::shell(&policy, &command).into_command();
        // Daemons started by launchd often inherit a minimal PATH, while the
        // user's development tools are installed through mise/Homebrew. Keep
        // the daemon PATH and add only standard user tool directories so
        // `node`, `pnpm`, and similar commands resolve inside the shell too.
        let inherited_path = self.user_path.clone().or_else(|| std::env::var_os("PATH"));
        cmd.env("PATH", tool_path(inherited_path.as_deref(), dirs_next::home_dir().as_deref()))
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

/// The caller's PATH first, so its ordering (a mise-pinned `node` over
/// Homebrew's) wins, then common tool and system directories as fallbacks.
fn tool_path(current: Option<&std::ffi::OsStr>, home: Option<&std::path::Path>) -> std::ffi::OsString {
    let mut paths: Vec<std::path::PathBuf> = current.map(|c| std::env::split_paths(c).collect()).unwrap_or_default();
    let mut fallbacks = Vec::new();
    if let Some(home) = home {
        fallbacks.push(home.join(".local/share/mise/shims"));
        fallbacks.push(home.join(".local/bin"));
    }
    fallbacks.extend(["/opt/homebrew/bin", "/usr/local/bin"].into_iter().map(std::path::PathBuf::from));
    #[cfg(unix)]
    fallbacks.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].into_iter().map(std::path::PathBuf::from));
    for dir in fallbacks {
        if !paths.contains(&dir) {
            paths.push(dir);
        }
    }
    std::env::join_paths(paths).unwrap_or_else(|_| current.map_or_else(std::ffi::OsString::new, std::ffi::OsStr::to_os_string))
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
            user_path: None,
        };
        (dir, tool)
    }

    #[cfg(unix)]
    #[test]
    fn developer_tool_path_adds_mise_and_preserves_daemon_path() {
        let path = tool_path(Some(std::ffi::OsStr::new("/usr/bin:/bin")), Some(std::path::Path::new("/Users/test")));
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();
        assert!(parts.contains(&std::path::PathBuf::from("/Users/test/.local/share/mise/shims")));
        assert!(parts.contains(&std::path::PathBuf::from("/usr/bin")));
        assert!(parts.contains(&std::path::PathBuf::from("/bin")));
    }

    #[cfg(unix)]
    #[test]
    fn caller_path_order_wins_over_fallbacks() {
        let path = tool_path(Some(std::ffi::OsStr::new("/mise/node/bin:/usr/bin")), None);
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();
        assert_eq!(parts[0], std::path::PathBuf::from("/mise/node/bin"));
        assert_eq!(
            parts.iter().filter(|p| p.as_path() == std::path::Path::new("/usr/bin")).count(),
            1,
            "deduped: {parts:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_receives_path_from_the_attached_coding_session() {
        let (dir, mut tool) = tool();
        tool.user_path = Some(std::ffi::OsString::from("/user/toolchain/bin"));
        let out = tool.execute(json!({"command": "printf '%s' \"$PATH\""})).await.unwrap();
        assert!(out.contains("exit code: 0"), "{out}");
        assert!(out.contains("/user/toolchain/bin"), "the caller PATH reaches the shell: {out}");
        assert!(dir.path().is_dir());
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
        // it in either sandbox mode (enforced, the macos_profile also denies
        // direct egress — see sandbox tests).
        let dir = tempfile::tempdir().unwrap();
        let tool = BashTool {
            workspace: dir.path().to_path_buf(),
            proxy: Some("127.0.0.1:3128".into()),
            user_path: None,
        };
        let out = tool.execute(json!({"command": "echo PROXY=$HTTP_PROXY"})).await.unwrap();
        assert!(out.contains("exit code: 0"), "{out}");
        assert!(out.contains("PROXY=http://127.0.0.1:3128"), "egress proxy env reaches the shell: {out}");
    }

    #[test]
    fn engine_deadline_defers_to_the_calls_own_timeout() {
        // Core 1.14.2 bounds tool calls at 120s by default; a build or test run
        // outlasts that, and the `timeout` argument is what kills the child.
        let (_dir, tool) = tool();
        assert_eq!(tool.timeout(), Some(smooth_operator::tool::NO_TOOL_TIMEOUT));
    }
}

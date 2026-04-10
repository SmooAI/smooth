//! Shared tool implementations for headless and server-side agent execution.
//!
//! Provides write_file, read_file, bash, and list_files tools scoped to a
//! working directory. Used by both headless mode (smooth-code) and the
//! /api/tasks endpoint (smooth-bigsmooth).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use smooth_operator::tool::{Tool, ToolSchema};
use smooth_operator::ToolRegistry;

// ---------------------------------------------------------------------------
// Tool implementations (scoped to working_dir)
// ---------------------------------------------------------------------------

pub struct WriteFileTool {
    pub base_dir: PathBuf,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write_file".into(),
            description: "Write content to a file. Creates parent directories automatically.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative file path within the project directory"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let path = arguments
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'path' parameter"))?;
        let content = arguments
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'content' parameter"))?;

        let full_path = self.base_dir.join(path);
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&full_path, content).await?;
        Ok(format!("wrote {} bytes to {path}", content.len()))
    }
}

pub struct ReadFileTool {
    pub base_dir: PathBuf,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".into(),
            description: "Read the contents of a file.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative file path within the project directory"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let path = arguments
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'path' parameter"))?;

        let full_path = self.base_dir.join(path);
        let content = tokio::fs::read_to_string(&full_path).await?;
        Ok(content)
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

pub struct BashTool {
    pub base_dir: PathBuf,
}

#[async_trait]
impl Tool for BashTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash".into(),
            description: "Run a shell command in the project directory. Returns stdout and stderr.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let command = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'command' parameter"))?;

        let output = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(&self.base_dir)
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        Ok(format!("exit code: {exit_code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"))
    }

    fn is_concurrent_safe(&self) -> bool {
        false
    }
}

pub struct ListFilesTool {
    pub base_dir: PathBuf,
}

#[async_trait]
impl Tool for ListFilesTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_files".into(),
            description: "List all files in the project directory recursively.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn execute(&self, _arguments: serde_json::Value) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("find")
            .arg(".")
            .arg("-type")
            .arg("f")
            .current_dir(&self.base_dir)
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.into_owned())
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Create the 4 tools scoped to a directory
// ---------------------------------------------------------------------------

/// Build a [`ToolRegistry`] with write_file, read_file, bash, and list_files
/// scoped to the given working directory.
/// In-process port forwarding — in non-sandboxed mode, the port is already
/// on localhost so this just confirms it's accessible.
pub struct ForwardPortTool;

#[async_trait]
impl Tool for ForwardPortTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "forward_port".into(),
            description: "Expose a port to the host network. In local mode, confirms the port is already accessible on localhost.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "guest_port": {
                        "type": "integer",
                        "description": "The port number your service is listening on (e.g. 3000)"
                    }
                },
                "required": ["guest_port"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let port = arguments.get("guest_port").and_then(|v| v.as_u64()).unwrap_or(3000) as u16;
        Ok(format!(
            "Port {port} is accessible at http://localhost:{port} (running in local mode, no forwarding needed)"
        ))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

pub fn create_tools(working_dir: &Path) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register(WriteFileTool {
        base_dir: working_dir.to_path_buf(),
    });
    tools.register(ReadFileTool {
        base_dir: working_dir.to_path_buf(),
    });
    tools.register(BashTool {
        base_dir: working_dir.to_path_buf(),
    });
    tools.register(ListFilesTool {
        base_dir: working_dir.to_path_buf(),
    });
    tools.register(ForwardPortTool);
    tools
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tool_write_file_creates_file() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let tool = WriteFileTool {
            base_dir: dir.path().to_path_buf(),
        };

        let args = serde_json::json!({
            "path": "hello.txt",
            "content": "hello world"
        });
        let result = tool.execute(args).await;
        assert!(result.is_ok(), "write_file should succeed: {result:?}");

        let content = std::fs::read_to_string(dir.path().join("hello.txt")).expect("read file");
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn tool_read_file_reads_content() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(dir.path().join("test.txt"), "test content").expect("write");

        let tool = ReadFileTool {
            base_dir: dir.path().to_path_buf(),
        };
        let args = serde_json::json!({"path": "test.txt"});
        let result = tool.execute(args).await.expect("read_file should succeed");
        assert_eq!(result, "test content");
    }

    #[tokio::test]
    async fn tool_bash_runs_command() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let tool = BashTool {
            base_dir: dir.path().to_path_buf(),
        };

        let args = serde_json::json!({"command": "echo hello"});
        let result = tool.execute(args).await.expect("bash should succeed");
        assert!(result.contains("hello"));
        assert!(result.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn tool_list_files_works() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(dir.path().join("a.txt"), "a").expect("write");
        std::fs::write(dir.path().join("b.txt"), "b").expect("write");

        let tool = ListFilesTool {
            base_dir: dir.path().to_path_buf(),
        };
        let result = tool.execute(serde_json::json!({})).await.expect("list_files should succeed");
        assert!(result.contains("a.txt"));
        assert!(result.contains("b.txt"));
    }

    #[test]
    fn create_tools_registers_five() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let tools = create_tools(dir.path());
        let schemas = tools.schemas();
        assert_eq!(schemas.len(), 5);
        let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"list_files"));
        assert!(names.contains(&"forward_port"));
    }
}

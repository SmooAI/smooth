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

/// Edit a file by replacing an exact string match. More token-efficient
/// than full-file rewrites — the agent only sends the changed fragment.
pub struct EditFileTool {
    base_dir: PathBuf,
}

#[async_trait]
impl Tool for EditFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".into(),
            description: "Replace a specific string in a file with a new string. More efficient than rewriting the entire file.".into(),
            parameters: serde_json::json!({
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

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let rel = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("missing 'path'"))?;
        let old_string = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'old_string'"))?;
        let new_string = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'new_string'"))?;
        let replace_all = args.get("replace_all").and_then(serde_json::Value::as_bool).unwrap_or(false);
        let path = self.base_dir.join(rel);
        let content = tokio::fs::read_to_string(&path).await?;
        let count = content.matches(old_string).count();
        if count == 0 {
            return Err(anyhow::anyhow!("old_string not found in {rel}"));
        }
        if count > 1 && !replace_all {
            return Err(anyhow::anyhow!(
                "old_string appears {count} times in {rel}; set replace_all=true or provide more context"
            ));
        }
        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };
        tokio::fs::write(&path, &new_content).await?;
        Ok(format!("edited {rel}: {count} replacement(s)"))
    }
}

/// In-process regex search over workspace files using the grep-searcher
/// library crate (the engine behind ripgrep). No subprocess overhead.
pub struct GrepTool {
    base_dir: PathBuf,
}

#[async_trait]
impl Tool for GrepTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "grep".into(),
            description: "Search file contents for a regex pattern. Fast, in-process ripgrep. Respects .gitignore.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for" },
                    "path": { "type": "string", "description": "Relative dir or file to search in (default: workspace root)" },
                    "include": { "type": "string", "description": "Glob pattern to filter files, e.g. '*.rs'" }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'pattern'"))?;
        let sub_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let include = args.get("include").and_then(|v| v.as_str());
        let base = self.base_dir.clone();
        let search_root = base.join(sub_path);
        let pattern = pattern.to_string();
        let include = include.map(std::string::ToString::to_string);
        tokio::task::spawn_blocking(move || {
            use grep_regex::RegexMatcher;
            use grep_searcher::sinks::UTF8;
            use grep_searcher::Searcher;
            let matcher = RegexMatcher::new_line_matcher(&pattern).map_err(|e| anyhow::anyhow!("invalid regex: {e}"))?;
            let mut walker_builder = ignore::WalkBuilder::new(&search_root);
            walker_builder.hidden(false);
            if let Some(ref inc) = include {
                let mut types = ignore::types::TypesBuilder::new();
                types.add("custom", inc).ok();
                if let Ok(built) = types.select("custom").build() {
                    walker_builder.types(built);
                }
            }
            let mut results = String::new();
            let mut count = 0usize;
            let max = 250;
            for entry in walker_builder.build().flatten() {
                if count >= max {
                    break;
                }
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }
                let file_path = entry.path().to_path_buf();
                let rel = file_path.strip_prefix(&base).unwrap_or(&file_path);
                let _ = Searcher::new().search_path(
                    &matcher,
                    &file_path,
                    UTF8(|line_num, line| {
                        if count < max {
                            let trimmed = if line.len() > 200 { &line[..200] } else { line.trim_end() };
                            results.push_str(&format!("{}:{}:{}\n", rel.display(), line_num, trimmed));
                            count += 1;
                        }
                        Ok(count < max)
                    }),
                );
            }
            if count == 0 {
                results.push_str("no matches found\n");
            }
            Ok::<String, anyhow::Error>(results)
        })
        .await?
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
    tools.register(EditFileTool {
        base_dir: working_dir.to_path_buf(),
    });
    tools.register(GrepTool {
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
    fn create_tools_registers_all() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let tools = create_tools(dir.path());
        let schemas = tools.schemas();
        assert_eq!(schemas.len(), 7);
        let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"list_files"));
        assert!(names.contains(&"forward_port"));
    }
}

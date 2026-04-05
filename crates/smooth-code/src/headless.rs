//! Headless (non-interactive) mode for smooth-code.
//!
//! Runs the agent without any TUI — output streams to stdout,
//! tool call diagnostics go to stderr. Suitable for scripting and CI.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Serialize;
use smooth_operator::cost::CostBudget;
use smooth_operator::llm::LlmConfig;
use smooth_operator::providers::ProviderRegistry;
use smooth_operator::tool::{Tool, ToolSchema};
use smooth_operator::{Agent, AgentConfig, AgentEvent, ToolRegistry};

// ---------------------------------------------------------------------------
// Tool implementations (same as golden E2E test, scoped to working_dir)
// ---------------------------------------------------------------------------

struct WriteFileTool {
    base_dir: PathBuf,
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

struct ReadFileTool {
    base_dir: PathBuf,
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

struct BashTool {
    base_dir: PathBuf,
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

struct ListFilesTool {
    base_dir: PathBuf,
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
// JSON output types
// ---------------------------------------------------------------------------

/// Structured JSON output for headless mode.
#[derive(Serialize)]
pub struct HeadlessOutput {
    pub content: String,
    pub tool_calls: Vec<HeadlessToolCall>,
    pub cost: f64,
}

/// A tool call recorded during headless execution.
#[derive(Clone, Serialize)]
pub struct HeadlessToolCall {
    pub name: String,
    pub success: bool,
}

// ---------------------------------------------------------------------------
// Create the 4 tools scoped to a directory
// ---------------------------------------------------------------------------

/// Build a [`ToolRegistry`] with write_file, read_file, bash, and list_files
/// scoped to the given working directory.
pub fn create_headless_tools(working_dir: &Path) -> ToolRegistry {
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
    tools
}

// ---------------------------------------------------------------------------
// Headless entry point
// ---------------------------------------------------------------------------

/// Run smooth-code in headless (non-interactive) mode.
///
/// Output streams to stdout, tool call diagnostics go to stderr.
///
/// # Errors
/// Returns an error if no API key is found, the message is empty,
/// or the agent encounters an unrecoverable error.
pub async fn run_headless(working_dir: PathBuf, message: String, model: Option<String>, budget: Option<f64>, json_output: bool) -> anyhow::Result<()> {
    if message.trim().is_empty() {
        anyhow::bail!("message must not be empty");
    }

    // 1. Load LLM config from providers.json
    let providers_path = dirs_next::home_dir()
        .map(|h| h.join(".smooth/providers.json"))
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;

    let mut llm_config = if providers_path.exists() {
        let registry = ProviderRegistry::load_from_file(&providers_path).map_err(|e| anyhow::anyhow!("Failed to load providers.json: {e}"))?;
        registry
            .default_llm_config()
            .map_err(|e| anyhow::anyhow!("No default provider configured: {e}"))?
            .with_temperature(0.3)
    } else {
        anyhow::bail!("No LLM providers configured. Run: th auth login <provider>");
    };

    // 2. Override model if specified
    if let Some(ref m) = model {
        llm_config = llm_config.with_model(m);
    }

    // 3. Create AgentConfig
    let system_prompt = "You are Smooth Coding, an AI coding assistant running in headless mode. \
        Help the user with their coding task. Use the provided tools to read, write, and execute code. \
        Be concise and thorough.";

    let mut config = AgentConfig::new("smooth-coding-headless", system_prompt, llm_config).with_max_iterations(50);

    // 4. Set budget if specified
    if let Some(max_usd) = budget {
        config = config.with_budget(CostBudget {
            max_cost_usd: Some(max_usd),
            max_tokens: None,
        });
    }

    // 5. Create tools
    let tools = create_headless_tools(&working_dir);

    // 6. Track tool calls for JSON output
    let tool_calls: Arc<Mutex<Vec<HeadlessToolCall>>> = Arc::new(Mutex::new(Vec::new()));
    let content_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    let tool_calls_clone = Arc::clone(&tool_calls);
    let content_buf_clone = Arc::clone(&content_buf);
    let is_json = json_output;

    // 7. Run agent with event handler
    let agent = Agent::new(config, tools).with_event_handler(move |event| match &event {
        AgentEvent::TokenDelta { content } => {
            // Accumulate content
            if let Ok(mut buf) = content_buf_clone.lock() {
                buf.push_str(content);
            }
            // Stream to stdout unless JSON mode
            if !is_json {
                print!("{content}");
                let _ = std::io::stdout().flush();
            }
        }
        AgentEvent::ToolCallStart { tool_name, .. } => {
            eprintln!("[tool] {tool_name}(...)");
        }
        AgentEvent::ToolCallComplete { tool_name, is_error, .. } => {
            let status = if *is_error { "error" } else { "ok" };
            eprintln!("[tool] {tool_name} -> {status}");
            if let Ok(mut calls) = tool_calls_clone.lock() {
                calls.push(HeadlessToolCall {
                    name: tool_name.clone(),
                    success: !is_error,
                });
            }
        }
        AgentEvent::Error { message } => {
            eprintln!("[error] {message}");
        }
        AgentEvent::Completed { iterations, .. } => {
            eprintln!("[done] completed in {iterations} iterations");
        }
        AgentEvent::MaxIterationsReached { max, .. } => {
            eprintln!("[warn] hit max iterations ({max})");
        }
        AgentEvent::BudgetExceeded { spent_usd, limit_usd } => {
            eprintln!("[warn] budget exceeded: ${spent_usd:.4} / ${limit_usd:.4}");
        }
        _ => {}
    });

    let _conversation = agent.run(&message).await?;

    // Ensure trailing newline for plain text output
    if !json_output {
        println!();
    }

    // 8. JSON output mode
    if json_output {
        #[allow(clippy::expect_used)]
        let cost = agent.cost_tracker.lock().expect("lock cost_tracker").total_cost_usd;

        #[allow(clippy::expect_used)]
        let tool_calls_vec = {
            let guard = tool_calls.lock().expect("lock tool_calls");
            guard.clone()
        };

        #[allow(clippy::expect_used)]
        let content = {
            let guard = content_buf.lock().expect("lock content_buf");
            guard.clone()
        };

        let output = HeadlessOutput {
            content,
            tool_calls: tool_calls_vec,
            cost,
        };

        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn headless_empty_message_returns_error() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let result = run_headless(dir.path().to_path_buf(), String::new(), None, None, false).await;
        assert!(result.is_err());
        let err_msg = result.expect_err("should error").to_string();
        assert!(err_msg.contains("empty"), "error should mention empty message, got: {err_msg}");
    }

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
    async fn json_output_format_is_valid() {
        // Verify the JSON serialization of HeadlessOutput
        let output = HeadlessOutput {
            content: "Hello from the agent".into(),
            tool_calls: vec![
                HeadlessToolCall {
                    name: "write_file".into(),
                    success: true,
                },
                HeadlessToolCall {
                    name: "bash".into(),
                    success: false,
                },
            ],
            cost: 0.0042,
        };

        let json_str = serde_json::to_string(&output).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("parse");

        assert_eq!(parsed["content"].as_str().expect("content"), "Hello from the agent");
        assert_eq!(parsed["tool_calls"].as_array().expect("tool_calls").len(), 2);
        assert!(parsed["tool_calls"][0]["success"].as_bool().expect("success"));
        assert!(!parsed["tool_calls"][1]["success"].as_bool().expect("success"));
        assert!((parsed["cost"].as_f64().expect("cost") - 0.0042).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn budget_config_is_respected() {
        // Verify that CostBudget is correctly constructed from a dollar amount
        let budget_usd = 1.5;
        let budget = CostBudget {
            max_cost_usd: Some(budget_usd),
            max_tokens: None,
        };
        assert_eq!(budget.max_cost_usd, Some(1.5));
        assert!(budget.max_tokens.is_none());

        // Verify the config builder accepts it
        let llm_config = LlmConfig::openrouter("test-key");
        let config = AgentConfig::new("test", "prompt", llm_config).with_budget(budget);
        assert!(config.budget.is_some());
        assert_eq!(config.budget.as_ref().expect("budget").max_cost_usd, Some(1.5));
    }
}

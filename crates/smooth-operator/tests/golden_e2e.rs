//! Golden E2E test: Smooth builds a web server and security-reviews it.
//!
//! Run with: cargo test --test golden_e2e -- --ignored
//! Requires: SMOOTH_E2E_API_KEY env var

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use smooth_narc::{NarcHook, Severity};
use smooth_operator::cost::CostBudget;
use smooth_operator::llm::LlmConfig;
use smooth_operator::tool::{Tool, ToolCall, ToolHook, ToolResult, ToolSchema};
use smooth_operator::{Agent, AgentConfig, AgentEvent, ToolRegistry};

// ---------------------------------------------------------------------------
// Shared NarcHook wrapper (so we can inspect alerts after the agent run)
// ---------------------------------------------------------------------------

struct SharedNarcHook {
    inner: Arc<NarcHook>,
}

#[async_trait]
impl ToolHook for SharedNarcHook {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        self.inner.pre_call(call).await
    }
    async fn post_call(&self, call: &ToolCall, result: &ToolResult) -> anyhow::Result<()> {
        self.inner.post_call(call, result).await
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
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
// Golden E2E test
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires real API tokens — run with: cargo test --test golden_e2e -- --ignored"]
#[allow(clippy::too_many_lines)]
async fn golden_e2e_build_web_server() -> anyhow::Result<()> {
    // 1. Setup: load API key
    let Ok(api_key) = std::env::var("SMOOTH_E2E_API_KEY") else {
        println!("Skipping: no API key (set SMOOTH_E2E_API_KEY)");
        return Ok(());
    };

    let tmp_dir = tempfile::tempdir()?;
    let base_dir = tmp_dir.path().to_path_buf();
    println!("Working directory: {}", base_dir.display());

    // 2. Configure agent
    let llm_config = LlmConfig::openrouter(&api_key).with_model("kimi-k2.5");
    let budget = CostBudget {
        max_cost_usd: Some(2.0),
        max_tokens: None,
    };
    let system_prompt = "\
        You are a coding agent. Build what the user asks. \
        Write files using the write_file tool. \
        Run commands using the bash tool. \
        Read files using the read_file tool. \
        List files using the list_files tool. \
        Be thorough.";

    let config = AgentConfig::new("golden-e2e", system_prompt, llm_config)
        .with_max_iterations(20)
        .with_budget(budget);

    // 3. Register tools
    let mut tools = ToolRegistry::new();
    tools.register(WriteFileTool { base_dir: base_dir.clone() });
    tools.register(ReadFileTool { base_dir: base_dir.clone() });
    tools.register(BashTool { base_dir: base_dir.clone() });
    tools.register(ListFilesTool { base_dir: base_dir.clone() });

    // 4. Add NarcHook for secret detection (write guard disabled — we want writes)
    let narc_hook = Arc::new(NarcHook::new(false));
    tools.add_hook(SharedNarcHook { inner: Arc::clone(&narc_hook) });

    // 5. Create and run the agent
    let agent = Agent::new(config, tools).with_event_handler(|event| match &event {
        AgentEvent::LlmRequest { iteration, message_count } => {
            println!("[iter {iteration}] LLM request with {message_count} messages");
        }
        AgentEvent::ToolCallStart { iteration, tool_name } => {
            println!("[iter {iteration}] calling tool: {tool_name}");
        }
        AgentEvent::ToolCallComplete {
            iteration,
            tool_name,
            is_error,
        } => {
            let status = if *is_error { "ERROR" } else { "ok" };
            println!("[iter {iteration}] tool {tool_name} -> {status}");
        }
        AgentEvent::Completed { iterations, .. } => {
            println!("Agent completed in {iterations} iterations");
        }
        AgentEvent::MaxIterationsReached { max, .. } => {
            println!("Agent hit max iterations ({max})");
        }
        AgentEvent::BudgetExceeded { spent_usd, limit_usd } => {
            println!("Budget exceeded: ${spent_usd:.4} / ${limit_usd:.4}");
        }
        _ => {}
    });

    let task = "\
        Build a simple REST API web server in Rust using axum with:\n\
        - GET /health -> returns {\"status\": \"ok\"}\n\
        - POST /items -> accepts JSON {\"name\": \"string\"}, returns {\"id\": 1, \"name\": \"string\"}\n\
        - GET /items -> returns list of all items\n\
        - In-memory storage (Vec behind Arc<Mutex>)\n\
        - Proper error handling (400 for bad input, 404 for not found)\n\
        - Create a Cargo.toml with axum, serde, serde_json, tokio dependencies\n\
        - Create src/main.rs with the full implementation\n\
        - Create a test.sh script that uses curl to test all endpoints";

    let conversation = agent.run(task).await?;

    // 6. Verify outputs
    let cargo_toml_path = base_dir.join("Cargo.toml");
    let main_rs_path = base_dir.join("src/main.rs");

    assert!(cargo_toml_path.exists(), "Cargo.toml was not created");
    assert!(main_rs_path.exists(), "src/main.rs was not created");

    let cargo_toml_content = tokio::fs::read_to_string(&cargo_toml_path).await?;
    let main_rs_content = tokio::fs::read_to_string(&main_rs_path).await?;

    // Content assertions
    assert!(cargo_toml_content.contains("axum"), "Cargo.toml should reference axum");
    assert!(main_rs_content.contains("async fn"), "src/main.rs should contain async functions");

    // Count files created
    let find_output = tokio::process::Command::new("find")
        .arg(".")
        .arg("-type")
        .arg("f")
        .current_dir(&base_dir)
        .output()
        .await?;
    let file_list = String::from_utf8_lossy(&find_output.stdout);
    let file_count = file_list.lines().count();
    assert!(file_count >= 3, "Expected at least 3 files, found {file_count}");
    println!("Files created: {file_count}");

    // Verify the code compiles
    let cargo_check = tokio::process::Command::new("cargo").arg("check").current_dir(&base_dir).output().await?;
    let check_stderr = String::from_utf8_lossy(&cargo_check.stderr);
    println!("cargo check stderr:\n{check_stderr}");
    assert!(
        cargo_check.status.success(),
        "cargo check failed — generated code does not compile:\n{check_stderr}"
    );

    // 7. Check agent didn't hit max iterations
    let last_msg = conversation.last_assistant_content();
    assert!(last_msg.is_some(), "Agent should have produced at least one assistant message");

    // Check cost stayed under budget
    #[allow(clippy::expect_used)]
    let cost = agent.cost_tracker.lock().expect("lock cost_tracker");
    println!("Total cost: ${:.4} ({} calls)", cost.total_cost_usd, cost.calls);
    assert!(cost.total_cost_usd <= 2.0, "Cost ${:.4} exceeded $2.00 budget", cost.total_cost_usd);

    // Check NarcHook: no Block-level alerts (secrets leaked)
    let block_alerts = narc_hook.alerts_above(Severity::Block);
    assert!(
        block_alerts.is_empty(),
        "NarcHook detected {} Block-level alert(s): {:?}",
        block_alerts.len(),
        block_alerts.iter().map(|a| &a.category).collect::<Vec<_>>()
    );

    println!("Golden E2E test PASSED");
    Ok(())
}

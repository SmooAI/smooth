//! Smooth Operator runner — the binary that actually runs inside each
//! microVM sandbox.
//!
//! Big Smooth (the READ-ONLY orchestrator) spawns a microVM via the embedded
//! `microsandbox` crate, mounts this binary into the VM, and execs it with a
//! task + LLM config provided via environment variables. The runner:
//!
//!  1. Loads its configuration from env vars (single-pass, no file I/O).
//!  2. Registers the file and shell tools, scoped to `SMOOTH_WORKSPACE`
//!     (default `/workspace`) so the agent cannot write outside the mount.
//!  3. Installs a `NarcHook` as a tool hook — this runs INSIDE the sandbox
//!     and catches secret leaks, prompt injection attempts, and dangerous
//!     writes before the tool executes (or, for secret leaks, before the
//!     result is handed back to the LLM).
//!  4. Runs `Agent::run_with_channel`, re-emitting every `AgentEvent` on
//!     stdout as a single-line JSON object. Big Smooth on the host captures
//!     this stream, parses each line, and forwards the matching `ServerEvent`
//!     to WebSocket clients.
//!  5. Exits 0 on success, non-zero on failure. Any anyhow error is printed
//!     as a final `{"type":"Error","message":"…"}` line before exit.
//!
//! ## Contract with Big Smooth
//!
//! **Inputs (env vars):**
//! - `SMOOTH_TASK` (required) — the user task the agent should execute
//! - `SMOOTH_API_URL` (required) — OpenAI-compatible base URL, e.g. `https://opencode.ai/zen/v1`
//! - `SMOOTH_API_KEY` (required) — bearer token
//! - `SMOOTH_MODEL` (optional, default `gpt-5.4-mini`) — model id
//! - `SMOOTH_BUDGET_USD` (optional) — cost cap in USD
//! - `SMOOTH_MAX_ITERATIONS` (optional, default 50)
//! - `SMOOTH_WORKSPACE` (optional, default `/workspace`) — tool sandbox root
//! - `SMOOTH_OPERATOR_ID` (optional, default `operator`) — included in log lines
//! - `SMOOTH_NARC_WRITE_GUARD` (optional, default `1`) — set to `0` to disable
//!
//! **Outputs (stdout):** JSON-lines, one `AgentEvent` per line.
//!
//! Stderr is reserved for tracing logs that Big Smooth treats as operator
//! diagnostics (forwarded as TokenDelta with a `[stderr]` prefix).

#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use smooth_narc::NarcHook;
use smooth_operator::cost::CostBudget;
use smooth_operator::llm::LlmConfig;
use smooth_operator::tool::{Tool, ToolCall, ToolHook, ToolRegistry, ToolResult, ToolSchema};
use smooth_operator::{Agent, AgentConfig, AgentEvent};
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Tools — scoped to SMOOTH_WORKSPACE.
// ---------------------------------------------------------------------------

struct ReadFileTool {
    base: PathBuf,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".into(),
            description: "Read the contents of a file under the workspace.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path within the workspace" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let rel = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("missing 'path'"))?;
        let path = self.base.join(rel);
        let content = tokio::fs::read_to_string(&path).await?;
        Ok(content)
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

struct WriteFileTool {
    base: PathBuf,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write_file".into(),
            description: "Write content to a file under the workspace. Creates parent dirs.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path within the workspace" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let rel = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("missing 'path'"))?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'content'"))?;
        let path = self.base.join(rel);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, content).await?;
        Ok(format!("wrote {} bytes to {rel}", content.len()))
    }
}

struct ListFilesTool {
    base: PathBuf,
}

#[async_trait]
impl Tool for ListFilesTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_files".into(),
            description: "List files in the workspace recursively.".into(),
            parameters: serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
        }
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<String> {
        let out = tokio::process::Command::new("find")
            .arg(".")
            .arg("-type")
            .arg("f")
            .current_dir(&self.base)
            .output()
            .await?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

struct BashTool {
    base: PathBuf,
}

#[async_trait]
impl Tool for BashTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash".into(),
            description: "Run a shell command inside the workspace.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to run" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'command'"))?;
        let out = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.base)
            .output()
            .await?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let code = out.status.code().unwrap_or(-1);
        Ok(format!("exit code: {code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"))
    }

    fn is_concurrent_safe(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// A thin ToolHook wrapper around NarcHook so we can own the `Arc` and inspect
// alerts afterwards. Mirrors the pattern from the golden_e2e test.
// ---------------------------------------------------------------------------

struct SharedNarc {
    inner: Arc<NarcHook>,
}

#[async_trait]
impl ToolHook for SharedNarc {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        self.inner.pre_call(call).await
    }

    async fn post_call(&self, call: &ToolCall, result: &ToolResult) -> anyhow::Result<()> {
        self.inner.post_call(call, result).await
    }
}

// ---------------------------------------------------------------------------
// Env config
// ---------------------------------------------------------------------------

struct RunnerConfig {
    task: String,
    api_url: String,
    api_key: String,
    model: String,
    budget_usd: Option<f64>,
    max_iterations: u32,
    workspace: PathBuf,
    operator_id: String,
    narc_write_guard: bool,
}

impl RunnerConfig {
    fn from_env() -> anyhow::Result<Self> {
        let require = |k: &str| -> anyhow::Result<String> { std::env::var(k).map_err(|_| anyhow::anyhow!("required env var {k} is not set")) };

        Ok(Self {
            task: require("SMOOTH_TASK")?,
            api_url: require("SMOOTH_API_URL")?,
            api_key: require("SMOOTH_API_KEY")?,
            model: std::env::var("SMOOTH_MODEL").unwrap_or_else(|_| "gpt-5.4-mini".into()),
            budget_usd: std::env::var("SMOOTH_BUDGET_USD").ok().and_then(|v| v.parse().ok()),
            max_iterations: std::env::var("SMOOTH_MAX_ITERATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(50),
            workspace: std::env::var("SMOOTH_WORKSPACE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/workspace")),
            operator_id: std::env::var("SMOOTH_OPERATOR_ID").unwrap_or_else(|_| "operator".into()),
            narc_write_guard: std::env::var("SMOOTH_NARC_WRITE_GUARD")
                .map(|v| v != "0" && v.to_ascii_lowercase() != "false")
                .unwrap_or(true),
        })
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

/// Emit a single JSON-line `AgentEvent` on stdout. Any serialization failure
/// is silently swallowed — stdout is how we communicate with Big Smooth, and
/// losing an event is preferable to crashing the runner.
fn emit_event(event: &AgentEvent) {
    if let Ok(line) = serde_json::to_string(event) {
        println!("{line}");
    }
}

#[tokio::main]
async fn main() {
    // tracing → stderr so stdout stays clean JSON-lines.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .try_init();

    let config = match RunnerConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            emit_event(&AgentEvent::Error { message: e.to_string() });
            std::process::exit(2);
        }
    };

    tracing::info!(
        operator = %config.operator_id,
        model = %config.model,
        workspace = %config.workspace.display(),
        narc_write_guard = config.narc_write_guard,
        "smooth-operator-runner starting"
    );

    // Make sure the workspace exists inside the VM.
    if let Err(e) = tokio::fs::create_dir_all(&config.workspace).await {
        emit_event(&AgentEvent::Error {
            message: format!("failed to create workspace {}: {e}", config.workspace.display()),
        });
        std::process::exit(2);
    }

    // Build the LLM config + agent config.
    let llm = LlmConfig {
        api_url: config.api_url.clone(),
        api_key: config.api_key.clone(),
        model: config.model.clone(),
        max_tokens: 8192,
        temperature: 0.3,
        retry_policy: smooth_operator::llm::RetryPolicy::default(),
        api_format: smooth_operator::llm::ApiFormat::OpenAiCompat,
    };

    let system_prompt = "You are Smooth Operator, an AI coding agent running inside a hardware-isolated microVM. \
        Use read_file, write_file, list_files, and bash tools to complete the user task. \
        All paths are relative to your workspace. Be concise and thorough.";

    let mut agent_config = AgentConfig::new(format!("op-{}", config.operator_id), system_prompt, llm).with_max_iterations(config.max_iterations);
    if let Some(cap) = config.budget_usd {
        agent_config = agent_config.with_budget(CostBudget {
            max_cost_usd: Some(cap),
            max_tokens: None,
        });
    }

    // Tools + NarcHook
    let mut tools = ToolRegistry::new();
    tools.register(ReadFileTool {
        base: config.workspace.clone(),
    });
    tools.register(WriteFileTool {
        base: config.workspace.clone(),
    });
    tools.register(ListFilesTool {
        base: config.workspace.clone(),
    });
    tools.register(BashTool {
        base: config.workspace.clone(),
    });

    let narc = Arc::new(NarcHook::new(config.narc_write_guard));
    tools.add_hook(SharedNarc { inner: Arc::clone(&narc) });

    // Run the agent on a channel and re-emit every AgentEvent as JSON-lines.
    let agent = Agent::new(agent_config, tools);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

    let emit_task = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            emit_event(&event);
        }
    });

    let result = agent.run_with_channel(&config.task, tx).await;

    // Drain the emitter before we exit.
    let _ = emit_task.await;

    // Report any NarcHook alerts that fired during the run as a final summary
    // event. This lets Big Smooth see the security verdict without having to
    // parse every tool result.
    let alerts = narc.alerts();
    if !alerts.is_empty() {
        if let Ok(json) = serde_json::to_string(&alerts) {
            eprintln!("[narc-alerts] {json}");
        }
    }

    match result {
        Ok(_conv) => {
            tracing::info!("smooth-operator-runner completed successfully");
            std::process::exit(0);
        }
        Err(e) => {
            emit_event(&AgentEvent::Error { message: e.to_string() });
            tracing::error!(error = %e, "smooth-operator-runner failed");
            std::process::exit(1);
        }
    }
}

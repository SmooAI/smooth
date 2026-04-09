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
//! - `SMOOTH_API_URL` (required) — OpenAI-compatible base URL, e.g. `https://openrouter.ai/api/v1`
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

mod delegate;
mod pearl_tools;
mod port_forward;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use smooth_goalie::{audit::AuditLogger, proxy::run_proxy, wonk::WonkClient};
use smooth_narc::NarcHook;
use smooth_operator::cost::CostBudget;
use smooth_operator::llm::LlmConfig;
use smooth_operator::tool::{Tool, ToolCall, ToolHook, ToolRegistry, ToolResult, ToolSchema};
use smooth_operator::{Agent, AgentConfig, AgentEvent};
use smooth_policy::Policy;
use smooth_scribe::hook::AuditHook as ScribeAuditHook;
use smooth_scribe::server::{build_router_with_state as scribe_router_with_state, AppState as ScribeAppState};
use smooth_scribe::store::{LogStore, MemoryLogStore, Query as LogQuery};
use smooth_wonk::hook::WonkHook;
use smooth_wonk::negotiate::Negotiator;
use smooth_wonk::policy::PolicyHolder;
use smooth_wonk::server::{build_router as wonk_router, AppState as WonkAppState};
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
    /// HTTP(S) proxy URL to forward into child processes via the standard
    /// env vars. When set, any `curl` / `wget` / etc. the agent invokes is
    /// routed through Goalie → Wonk for policy enforcement. The runner's
    /// own HTTP traffic (LLM provider, in-VM Wonk/Scribe) intentionally
    /// does NOT go through the proxy — setting HTTP_PROXY on the runner
    /// process would loop WonkHook's localhost check request back through
    /// Goalie and deadlock the policy check.
    proxy_url: Option<String>,
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
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command).current_dir(&self.base);
        if let Some(ref proxy) = self.proxy_url {
            cmd.env("HTTP_PROXY", proxy)
                .env("http_proxy", proxy)
                .env("HTTPS_PROXY", proxy)
                .env("https_proxy", proxy)
                // Local in-VM services (Wonk, Scribe, Goalie itself) must
                // not be proxied — they run on localhost.
                .env("NO_PROXY", "127.0.0.1,localhost")
                .env("no_proxy", "127.0.0.1,localhost");
        }
        let out = cmd.output().await?;
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
    /// Policy TOML to feed into Wonk. Resolution order:
    /// 1. `SMOOTH_POLICY_TOML` env var (the full TOML string inline)
    /// 2. `SMOOTH_POLICY_FILE` env var (path to a TOML file)
    /// 3. `/opt/smooth/policy.toml` if present
    /// 4. A permissive default ([`default_policy_toml`]).
    policy_toml: String,
}

impl RunnerConfig {
    fn from_env() -> anyhow::Result<Self> {
        let require = |k: &str| -> anyhow::Result<String> { std::env::var(k).map_err(|_| anyhow::anyhow!("required env var {k} is not set")) };

        let policy_toml = resolve_policy_toml();

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
            // WriteGuard default is OFF in the runner: the microVM's workspace
            // is a dedicated, throwaway bind mount and the agent is *expected*
            // to write files into it. NarcHook's other detectors (secrets,
            // injection) stay on regardless. Opt back in with
            // `SMOOTH_NARC_WRITE_GUARD=1` for phases where writes should be
            // audited or blocked.
            narc_write_guard: std::env::var("SMOOTH_NARC_WRITE_GUARD")
                .map(|v| v == "1" || v.to_ascii_lowercase() == "true")
                .unwrap_or(false),
            policy_toml,
        })
    }
}

/// Resolve a policy TOML string from env vars, a standard path, or the
/// permissive default. This runs before Wonk's `PolicyHolder` is built.
fn resolve_policy_toml() -> String {
    if let Ok(inline) = std::env::var("SMOOTH_POLICY_TOML") {
        if !inline.trim().is_empty() {
            return inline;
        }
    }
    if let Ok(file) = std::env::var("SMOOTH_POLICY_FILE") {
        if let Ok(contents) = std::fs::read_to_string(&file) {
            return contents;
        }
    }
    if let Ok(contents) = std::fs::read_to_string("/opt/smooth/policy.toml") {
        return contents;
    }
    default_policy_toml()
}

/// A permissive baseline policy. Wonk still checks every tool call and
/// network request against this, but everything the operator reasonably
/// needs is allowed. Big Smooth should generate a tighter policy per task
/// and pass it via `SMOOTH_POLICY_TOML`.
fn default_policy_toml() -> String {
    r#"
[metadata]
operator_id = "runner"
bead_id = ""
phase = "execute"

[auth]
token = "runner-default"

[network]
[[network.allow]]
domain = "openrouter.ai"
[[network.allow]]
domain = "api.openai.com"
[[network.allow]]
domain = "api.anthropic.com"
[[network.allow]]
domain = "127.0.0.1"
[[network.allow]]
domain = "localhost"

[filesystem]
writable = true
deny_patterns = ["*.env", "*.pem", ".ssh/*", "id_rsa*"]

[tools]
allow = ["read_file", "write_file", "list_files", "bash"]
deny = []

[beads]

[mcp]

[access_requests]
"#
    .to_string()
}

// ---------------------------------------------------------------------------
// In-VM cast: Wonk + Goalie + Scribe spawned on ephemeral localhost ports.
// ---------------------------------------------------------------------------

/// Handles to the in-VM cast members. Returned from [`spawn_cast`] so the
/// runner can (a) point the agent's hooks at them, and (b) inspect their
/// state (e.g. the Scribe log store) for the final stderr summary.
struct Cast {
    wonk_url: String,
    scribe_url: String,
    #[allow(dead_code)]
    goalie_url: String,
    scribe_store: Arc<MemoryLogStore>,
    /// Absolute path to Goalie's JSON-lines audit log inside the VM. The
    /// runner reads this back into the final cast summary so tests (and
    /// humans reading the [runner stderr] forward) can see every allowed
    /// and denied network request the sandbox actually attempted.
    goalie_audit_path: String,
}

/// Spawn Wonk, Scribe, and Goalie in-process on ephemeral localhost ports.
///
/// Wonk gets the runner's configured policy. Scribe is a fresh in-memory
/// store (we dump it to stderr at the end). Goalie is pointed at Wonk and
/// writes its JSON-lines audit log to `/tmp/goalie-<operator>.jsonl` inside
/// the VM (which is tmpfs — ephemeral, fine for this round).
///
/// All three bind to `127.0.0.1:0` and their URLs are returned in [`Cast`].
async fn spawn_cast(policy_toml: &str, operator_id: &str) -> anyhow::Result<Cast> {
    // --- Scribe ---
    // If SMOOTH_ARCHIVIST_URL is set, mirror every log entry to the
    // Boardroom's Archivist via a background forwarder. Otherwise run
    // standalone (legacy behavior, fine for host-mode sandboxed tests).
    let archivist_url = std::env::var("SMOOTH_ARCHIVIST_URL").ok().filter(|s| !s.trim().is_empty());
    // Diagnostic: write the archivist URL to the workspace for host-side
    // inspection. Uses SMOOTH_WORKSPACE since we don't have the config here.
    if let Ok(ws) = std::env::var("SMOOTH_WORKSPACE") {
        let diag = format!("SMOOTH_ARCHIVIST_URL={}", archivist_url.as_deref().unwrap_or("<NOT SET>"));
        let _ = std::fs::write(format!("{ws}/.archivist-diag.txt"), &diag);
    }
    let scribe_state = if let Some(url) = archivist_url {
        tracing::info!(archivist = %url, operator = operator_id, "spawning scribe with archivist forwarder");
        let forwarder = smooth_scribe::spawn_forwarder(url, operator_id.to_string());
        ScribeAppState::with_forwarder(forwarder)
    } else {
        tracing::warn!(
            operator = operator_id,
            "SMOOTH_ARCHIVIST_URL not set — scribe will store logs locally only (no cross-VM forwarding)"
        );
        ScribeAppState::local_only()
    };
    let scribe_store = Arc::clone(&scribe_state.store);
    let scribe_router = scribe_router_with_state(scribe_state);
    let scribe_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let scribe_addr = scribe_listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(scribe_listener, scribe_router).await {
            tracing::error!(error = %e, "in-VM Scribe server crashed");
        }
    });

    // --- Wonk ---
    let policy = Policy::from_toml(policy_toml).map_err(|e| anyhow::anyhow!("invalid policy TOML: {e}"))?;
    let holder = PolicyHolder::from_policy(policy);
    // There is no Big Smooth leader to negotiate with from inside this VM
    // (the runner is self-contained), so we point the negotiator at a stub
    // URL. access negotiation calls will fail closed, which is the safe
    // default — we can wire it up later.
    let negotiator = Negotiator::new("http://127.0.0.1:1/no-leader", holder.clone());
    let wonk_state = Arc::new(WonkAppState::new(holder, negotiator));
    let wonk_r = wonk_router(wonk_state);
    let wonk_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let wonk_addr = wonk_listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(wonk_listener, wonk_r).await {
            tracing::error!(error = %e, "in-VM Wonk server crashed");
        }
    });
    let wonk_url = format!("http://{wonk_addr}");

    // --- Goalie ---
    // Audit log → tmpfs under /tmp. Bind to an ephemeral localhost port.
    let audit_path = format!("/tmp/goalie-{operator_id}.jsonl");
    let audit = AuditLogger::new(&audit_path)?;
    let goalie_client = WonkClient::new(&wonk_url);
    // run_proxy binds itself, so we pre-probe for a free port the same way
    // Big Smooth's sandboxed dispatch does. Tight race, fine for a single
    // in-VM spawn.
    let goalie_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let goalie_addr = goalie_listener.local_addr()?;
    drop(goalie_listener);
    let goalie_listen = goalie_addr.to_string();
    tokio::spawn(async move {
        if let Err(e) = run_proxy(&goalie_listen, goalie_client, audit).await {
            tracing::error!(error = %e, "in-VM Goalie proxy crashed");
        }
    });

    // Give axum + hyper a beat to start accepting before the agent hits them.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    Ok(Cast {
        wonk_url,
        scribe_url: format!("http://{scribe_addr}"),
        goalie_url: format!("http://{goalie_addr}"),
        scribe_store,
        goalie_audit_path: audit_path,
    })
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

    // Pearl env cache: if /opt/smooth/cache is mounted (bind from host),
    // point build tool caches there so deps persist across VM runs for
    // the same pearl. First run compiles everything (~5 min for Rust);
    // subsequent runs find deps already compiled (~5s). This is the
    // single biggest enabler for agent iteration quality.
    let cache_root = std::path::Path::new("/opt/smooth/cache");
    if cache_root.exists() {
        let cargo_home = cache_root.join(".cargo");
        let npm_cache = cache_root.join(".npm");
        let pnpm_store = cache_root.join(".pnpm-store");
        // Create subdirs (first-run init).
        for d in [&cargo_home, &npm_cache, &pnpm_store] {
            let _ = std::fs::create_dir_all(d);
        }
        std::env::set_var("CARGO_HOME", &cargo_home);
        // Persist compiled Rust deps across workspace resets. Without this,
        // each new workspace tempdir starts a fresh target/ and recompiles
        // ALL deps from source (~5 min). With CARGO_TARGET_DIR in the cache,
        // deps compiled on the first run are reused on subsequent runs.
        let cargo_target = cache_root.join("cargo-target");
        let _ = std::fs::create_dir_all(&cargo_target);
        std::env::set_var("CARGO_TARGET_DIR", &cargo_target);
        std::env::set_var("npm_config_cache", &npm_cache);
        std::env::set_var("pnpm_store_dir", &pnpm_store);
        // Put cached cargo binaries on PATH so `cargo`, `rustfmt`, etc.
        // are available if rustup was installed to the cache in a prior run.
        if let Ok(path) = std::env::var("PATH") {
            std::env::set_var("PATH", format!("{}:{path}", cargo_home.join("bin").display()));
        }
        tracing::info!(
            cargo_home = %cargo_home.display(),
            npm_cache = %npm_cache.display(),
            "pearl env cache active at /opt/smooth/cache"
        );
    }

    // Make sure the workspace exists inside the VM.
    if let Err(e) = tokio::fs::create_dir_all(&config.workspace).await {
        emit_event(&AgentEvent::Error {
            message: format!("failed to create workspace {}: {e}", config.workspace.display()),
        });
        std::process::exit(2);
    }

    // Spawn the in-VM security cast: Wonk (policy), Goalie (proxy), Scribe
    // (log sink). All three run as tokio tasks bound to ephemeral localhost
    // ports. The agent's tool hooks will talk to Wonk + Scribe over HTTP.
    let cast = match spawn_cast(&config.policy_toml, &config.operator_id).await {
        Ok(c) => c,
        Err(e) => {
            emit_event(&AgentEvent::Error {
                message: format!("failed to spawn in-VM cast: {e}"),
            });
            std::process::exit(2);
        }
    };
    tracing::info!(
        wonk = %cast.wonk_url,
        scribe = %cast.scribe_url,
        goalie = %cast.goalie_url,
        "in-VM cast spawned"
    );

    // Only the bash tool's child processes route through Goalie. The runner
    // itself does NOT set HTTP_PROXY on its own env — if it did, WonkHook
    // and ScribeAuditHook would pick up the proxy from env and loop their
    // localhost check requests back through Goalie, deadlocking the policy
    // check. The bash tool gets the proxy URL injected per-invocation.
    let proxy_for_bash = Some(cast.goalie_url.clone());

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

    // Tools + NarcHook — register BEFORE building the system prompt so we
    // can announce all available tools to the LLM.
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
        proxy_url: proxy_for_bash,
    });

    tools.register(port_forward::ForwardPortTool);

    // Remote delegation tool — only in sandboxed mode (SMOOTH_API_URL points
    // to Big Smooth on the host). In host/in-process mode the existing
    // DelegationTool from smooth-operator handles delegation.
    if let Ok(smooth_api_url) = std::env::var("SMOOTH_API_URL") {
        if !smooth_api_url.trim().is_empty() {
            tracing::info!(api_url = %smooth_api_url, "registering RemoteDelegateTool (sandboxed mode)");
            tools.register(delegate::RemoteDelegateTool::new(&smooth_api_url, &config.operator_id));
        }
    }

    // Pearl tools — if workspace has .smooth/dolt/, register direct Dolt
    // pearl tools so the agent can create/list/close pearls locally.
    pearl_tools::register_pearl_tools(&mut tools, &config.workspace);

    // Build system prompt — keep it short. The LLM sees tool schemas automatically.
    let has_pearl_tools = tools.schemas().iter().any(|s| s.name == "create_pearl");
    let pearl_note = if has_pearl_tools {
        " You also have create_pearl, list_pearls, and close_pearl tools for tracking work items."
    } else {
        ""
    };
    let base_prompt = format!(
        "You are Smooth Operator, an AI coding agent running inside a hardware-isolated microVM. \
        Use read_file, write_file, list_files, and bash tools to complete the user task. \
        All paths are relative to your workspace.{pearl_note} Be concise and thorough.",
    );
    let workspace_path = std::path::Path::new(&config.workspace);
    let system_prompt = match smooth_operator::context::load_project_context(workspace_path) {
        Some(ctx) => format!("{base_prompt}\n\n## Project Context\n\n{ctx}"),
        None => base_prompt,
    };

    let mut agent_config = AgentConfig::new(format!("op-{}", config.operator_id), &system_prompt, llm).with_max_iterations(config.max_iterations);
    if let Some(cap) = config.budget_usd {
        agent_config = agent_config.with_budget(CostBudget {
            max_cost_usd: Some(cap),
            max_tokens: None,
        });
    }

    // Hook order is intentional:
    //   1. Narc — fastest, catches secrets/injection/dangerous writes purely
    //      in-process (no HTTP). Blocks the call outright on Block severity.
    //   2. Wonk — HTTP check against the policy for tool name, network
    //      domain, cli command, etc. Blocks the call if the policy denies.
    //   3. Scribe audit — best-effort POST of pre_call/post_call log entries
    //      to the in-VM Scribe for later aggregation.
    //
    // All three are `ToolHook` impls so they compose cleanly on the registry.
    let narc = Arc::new(NarcHook::new(config.narc_write_guard));
    tools.add_hook(SharedNarc { inner: Arc::clone(&narc) });
    tools.add_hook(WonkHook::new(&cast.wonk_url));
    tools.add_hook(ScribeAuditHook::new(&cast.scribe_url, &config.operator_id));

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

    // Cast summary on stderr: Narc alert count, Scribe log count, Goalie
    // audit entries, runtime verdict. Big Smooth forwards this verbatim
    // as a [runner stderr] TokenDelta so operators can audit what every
    // in-VM security service saw during the run. A parseable prefix
    // (`[cast-summary]`) lets tests scrape it without false matches on
    // log output.
    let narc_alerts = narc.alerts();
    let scribe_entries = cast.scribe_store.query(&LogQuery::default());
    let goalie_audit_entries: Vec<serde_json::Value> = std::fs::read_to_string(&cast.goalie_audit_path)
        .ok()
        .map(|contents| {
            contents
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .collect()
        })
        .unwrap_or_default();
    let goalie_denied_count = goalie_audit_entries
        .iter()
        .filter(|e| e.get("allowed").and_then(serde_json::Value::as_bool) == Some(false))
        .count();
    let summary = serde_json::json!({
        "narc_alert_count": narc_alerts.len(),
        "narc_alerts": narc_alerts,
        "scribe_entry_count": scribe_entries.len(),
        "scribe_entries_sample": scribe_entries.iter().take(10).collect::<Vec<_>>(),
        "goalie_audit_count": goalie_audit_entries.len(),
        "goalie_denied_count": goalie_denied_count,
        "goalie_audit": goalie_audit_entries,
        "wonk_url": cast.wonk_url,
        "scribe_url": cast.scribe_url,
        "goalie_url": cast.goalie_url,
    });
    if let Ok(line) = serde_json::to_string(&summary) {
        eprintln!("[cast-summary] {line}");
    }

    // Give the Scribe forwarder time to flush its last batch to
    // Archivist before we exit. The forwarder runs as a spawned tokio
    // task with a 500ms flush interval. `std::process::exit` kills the
    // runtime instantly, losing buffered entries. A 2-second sleep
    // before exit lets the forwarder's timer fire at least 3 times,
    // draining any pending batches to the Archivist.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    match result {
        Ok(_conv) => {
            // Emit Completed explicitly so Big Smooth sees it in stdout even
            // if the channel-based emission raced with process exit. This is
            // the authoritative "I'm done" signal — Big Smooth looks for
            // `saw_completed` before sending TaskComplete to WS clients.
            emit_event(&AgentEvent::Completed {
                agent_id: config.operator_id.clone(),
                iterations: 0, // exact count was in the channel event; this is the fallback
            });
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

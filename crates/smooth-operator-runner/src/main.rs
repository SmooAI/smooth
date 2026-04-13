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

mod bg_process;
mod lsp;
mod tool_support;

// ---------------------------------------------------------------------------
// Tools — scoped to SMOOTH_WORKSPACE.
// ---------------------------------------------------------------------------

struct ReadFileTool {
    base: PathBuf,
    file_tracker: Arc<tool_support::FileTimeTracker>,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".into(),
            description: "Read a file under the workspace. Supports line ranges via offset + limit to avoid reading huge files.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path within the workspace" },
                    "offset": { "type": "integer", "description": "1-based start line (default: 1)" },
                    "limit": { "type": "integer", "description": "Max lines to return (default: 2000)" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let rel = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("missing 'path'"))?;
        let path = self.base.join(rel);

        // "Did you mean?" on file-not-found.
        if !path.exists() {
            let suggestions = tool_support::suggest_similar_paths(&self.base, rel, 3);
            let hint = if suggestions.is_empty() {
                String::new()
            } else {
                format!(" Did you mean: {}?", suggestions.join(", "))
            };
            return Err(anyhow::anyhow!("file not found: {rel}.{hint}"));
        }

        let content = tokio::fs::read_to_string(&path).await?;
        self.file_tracker.record(&path);
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start = (offset - 1).min(total);
        let end = (start + limit).min(total);
        let slice = &lines[start..end];

        // Number each line like `cat -n` for easy reference in follow-up edits.
        let mut result = String::new();
        for (i, line) in slice.iter().enumerate() {
            result.push_str(&format!("{}\t{}\n", start + i + 1, line));
        }
        if end < total {
            result.push_str(&format!("... ({} more lines, {} total)\n", total - end, total));
        }
        Ok(result)
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

struct WriteFileTool {
    base: PathBuf,
    file_tracker: Arc<tool_support::FileTimeTracker>,
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

        // File-time conflict check.
        if let Some(warning) = self.file_tracker.check_before_write(&path) {
            return Err(anyhow::anyhow!("{warning}"));
        }

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, content).await?;
        self.file_tracker.update_after_write(&path);

        // Auto-format the written file (best-effort).
        tool_support::auto_format(&self.base, &path).await;

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
            description:
                "List files matching a glob pattern (e.g. '**/*.rs', 'src/**'). Respects .gitignore. Results sorted by modification time (newest first).".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern to match (default: '**/*' — all files)" }
                },
                "required": []
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("**/*");
        let base = self.base.clone();
        let pattern = pattern.to_string();
        tokio::task::spawn_blocking(move || {
            let mut entries: Vec<(String, std::time::SystemTime)> = Vec::new();
            let walker = ignore::WalkBuilder::new(&base).hidden(false).build();
            let glob = globset::GlobBuilder::new(&pattern)
                .literal_separator(true)
                .build()
                .and_then(|g| globset::GlobSet::builder().add(g).build())
                .ok();
            for entry in walker.flatten() {
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }
                let rel = entry.path().strip_prefix(&base).unwrap_or(entry.path());
                let rel_str = rel.to_string_lossy();
                if let Some(ref gs) = glob {
                    if !gs.is_match(rel) {
                        continue;
                    }
                }
                let mtime = entry.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
                entries.push((rel_str.to_string(), mtime));
            }
            entries.sort_by(|a, b| b.1.cmp(&a.1));
            let max = 200;
            let total = entries.len();
            let mut result = String::new();
            for (path, _) in entries.iter().take(max) {
                result.push_str(path);
                result.push('\n');
            }
            if total > max {
                result.push_str(&format!("... ({total} total, showing {max})\n"));
            }
            Ok::<String, anyhow::Error>(result)
        })
        .await?
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// EditFileTool — oldString → newString patching.
//
// Much more token-efficient than rewriting an entire file: the agent only
// sends the fragment it wants to change, and a new fragment to replace it.
// If oldString appears more than once, the edit fails unless replace_all
// is set.
// ---------------------------------------------------------------------------

struct EditFileTool {
    base: PathBuf,
    file_tracker: Arc<tool_support::FileTimeTracker>,
}

#[async_trait]
impl Tool for EditFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".into(),
            description: "Replace a specific string in a file with a new string. More efficient than rewriting the entire file — only send the changed fragment. The old_string must match exactly (including whitespace and indentation). If old_string appears more than once, set replace_all=true or provide more surrounding context to make it unique.".into(),
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

        let path = self.base.join(rel);

        // "Did you mean?" on file-not-found.
        if !path.exists() {
            let suggestions = tool_support::suggest_similar_paths(&self.base, rel, 3);
            let hint = if suggestions.is_empty() {
                String::new()
            } else {
                format!(" Did you mean: {}?", suggestions.join(", "))
            };
            return Err(anyhow::anyhow!("file not found: {rel}.{hint}"));
        }

        // File-time conflict check.
        if let Some(warning) = self.file_tracker.check_before_write(&path) {
            return Err(anyhow::anyhow!("{warning}"));
        }

        let content = tokio::fs::read_to_string(&path).await.map_err(|e| anyhow::anyhow!("read {rel}: {e}"))?;

        let count = content.matches(old_string).count();
        if count == 0 {
            return Err(anyhow::anyhow!(
                "old_string not found in {rel}. Make sure you match the exact text including whitespace and indentation."
            ));
        }
        if count > 1 && !replace_all {
            return Err(anyhow::anyhow!(
                "old_string appears {count} times in {rel}. Provide more surrounding context to make it unique, or set replace_all=true."
            ));
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };
        tokio::fs::write(&path, &new_content).await?;
        self.file_tracker.update_after_write(&path);

        // Auto-format the edited file (best-effort).
        tool_support::auto_format(&self.base, &path).await;

        let diff = tool_support::generate_diff(rel, &content, &new_content);
        let replacements = if replace_all { count } else { 1 };
        Ok(format!(
            "edited {rel}: {replacements} replacement(s), {} → {} bytes\n\n{diff}",
            content.len(),
            new_content.len()
        ))
    }
}

// ---------------------------------------------------------------------------
// ApplyPatchTool — apply a unified diff to workspace files.
// ---------------------------------------------------------------------------

struct ApplyPatchTool {
    base: PathBuf,
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "apply_patch".into(),
            description: "Apply a unified diff patch to one or more files. Accepts standard unified diff format (--- a/file, +++ b/file, @@ hunks). More powerful than edit_file for multi-hunk or multi-file changes.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": { "type": "string", "description": "The unified diff patch text" }
                },
                "required": ["patch"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let patch_text = args.get("patch").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("missing 'patch'"))?;
        let base = self.base.clone();
        let patch_text = patch_text.to_string();
        tokio::task::spawn_blocking(move || tool_support::apply_unified_patch(&base, &patch_text)).await?
    }
}

// ---------------------------------------------------------------------------
// LspTool — language server protocol integration.
//
// Spawns rust-analyzer / typescript-language-server / ty / gopls as a
// sidecar process inside the VM and exposes goToDefinition, findReferences,
// hover, documentSymbol, workspaceSymbol, and diagnostics to the agent.
// ---------------------------------------------------------------------------

struct LspTool {
    base: PathBuf,
    client: Arc<tokio::sync::Mutex<Option<lsp::LspClient>>>,
}

#[async_trait]
impl Tool for LspTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "lsp".into(),
            description: "Language server integration — semantic code intelligence. Supports goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, and diagnostics. The language server (rust-analyzer, typescript-language-server, ty, gopls) is auto-detected and lazily spawned.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["goToDefinition", "findReferences", "hover", "documentSymbol", "workspaceSymbol", "diagnostics"],
                        "description": "The LSP operation to perform"
                    },
                    "file": { "type": "string", "description": "Relative file path (required for all except workspaceSymbol)" },
                    "line": { "type": "integer", "description": "1-based line number (required for goToDefinition, findReferences, hover)" },
                    "character": { "type": "integer", "description": "1-based column number (required for goToDefinition, findReferences, hover)" },
                    "query": { "type": "string", "description": "Search query (required for workspaceSymbol)" }
                },
                "required": ["operation"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let operation = args
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'operation'"))?;
        let file = args.get("file").and_then(|v| v.as_str());
        let line = args.get("line").and_then(|v| v.as_u64()).map(|l| (l.saturating_sub(1)) as u32);
        let character = args.get("character").and_then(|v| v.as_u64()).map(|c| (c.saturating_sub(1)) as u32);
        let query = args.get("query").and_then(|v| v.as_str());

        // Lazily start the language server on first use.
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            match lsp::LspClient::start(&self.base).await {
                Ok(client) => *guard = Some(client),
                Err(e) => return Err(anyhow::anyhow!("LSP server start failed: {e}")),
            }
        }
        let client = guard.as_mut().ok_or_else(|| anyhow::anyhow!("LSP client not initialized"))?;

        match operation {
            "goToDefinition" => {
                let f = file.ok_or_else(|| anyhow::anyhow!("goToDefinition requires 'file'"))?;
                let l = line.ok_or_else(|| anyhow::anyhow!("goToDefinition requires 'line'"))?;
                let c = character.ok_or_else(|| anyhow::anyhow!("goToDefinition requires 'character'"))?;
                client.go_to_definition(&self.base, f, l, c).await
            }
            "findReferences" => {
                let f = file.ok_or_else(|| anyhow::anyhow!("findReferences requires 'file'"))?;
                let l = line.ok_or_else(|| anyhow::anyhow!("findReferences requires 'line'"))?;
                let c = character.ok_or_else(|| anyhow::anyhow!("findReferences requires 'character'"))?;
                client.find_references(&self.base, f, l, c).await
            }
            "hover" => {
                let f = file.ok_or_else(|| anyhow::anyhow!("hover requires 'file'"))?;
                let l = line.ok_or_else(|| anyhow::anyhow!("hover requires 'line'"))?;
                let c = character.ok_or_else(|| anyhow::anyhow!("hover requires 'character'"))?;
                client.hover(&self.base, f, l, c).await
            }
            "documentSymbol" => {
                let f = file.ok_or_else(|| anyhow::anyhow!("documentSymbol requires 'file'"))?;
                client.document_symbols(&self.base, f).await
            }
            "workspaceSymbol" => {
                let q = query.unwrap_or("");
                client.workspace_symbols(q).await
            }
            "diagnostics" => {
                let f = file.ok_or_else(|| anyhow::anyhow!("diagnostics requires 'file'"))?;
                client.diagnostics(&self.base, f).await
            }
            other => Err(anyhow::anyhow!(
                "unknown LSP operation: {other}. Use one of: goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, diagnostics"
            )),
        }
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// GrepTool — in-process ripgrep via grep-searcher + grep-regex.
//
// Searches file contents under the workspace for a regex pattern, respecting
// .gitignore. Returns matching lines with file path + line number. Much
// faster than shelling out to `grep` — the search runs in-process on a
// thread pool with no subprocess overhead.
// ---------------------------------------------------------------------------

struct GrepTool {
    base: PathBuf,
}

#[async_trait]
impl Tool for GrepTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "grep".into(),
            description:
                "Search file contents for a regex pattern. Fast, in-process ripgrep. Respects .gitignore. Returns matching lines with file:line:content.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for" },
                    "path": { "type": "string", "description": "Relative dir or file to search in (default: entire workspace)" },
                    "include": { "type": "string", "description": "Glob pattern to filter files, e.g. '*.rs', '*.{ts,tsx}'" }
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

        let base = self.base.clone();
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
                // The `types` system in the ignore crate maps file extensions
                // to type names. For simple globs like "*.rs" we add it as a
                // custom type and select it.
                let mut types = ignore::types::TypesBuilder::new();
                types.add("custom", inc).ok();
                if let Ok(built) = types.select("custom").build() {
                    walker_builder.types(built);
                }
            }

            let mut results = String::new();
            let mut match_count = 0usize;
            let max_matches = 250;

            for entry in walker_builder.build().flatten() {
                if match_count >= max_matches {
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
                        if match_count < max_matches {
                            let trimmed = if line.len() > 200 { &line[..200] } else { line.trim_end() };
                            results.push_str(&format!("{}:{}:{}\n", rel.display(), line_num, trimmed));
                            match_count += 1;
                        }
                        Ok(match_count < max_matches)
                    }),
                );
            }
            if match_count >= max_matches {
                results.push_str(&format!("... (showing first {max_matches} matches)\n"));
            }
            if match_count == 0 {
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
            description: "Run a shell command inside the workspace. Blocks until the command exits — no default timeout. Pass `timeout` (seconds) for commands that might hang. For long-lived processes (dev servers, watchers) use `bg_run` instead so the agent loop isn't blocked.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to run" },
                    "timeout": { "type": "integer", "description": "Optional: max seconds before the command is killed. If omitted, bash waits indefinitely." }
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
        // Timeout is OPT-IN. No default cap — long-running builds and
        // tests need to be able to finish. For genuinely long-lived
        // processes (dev servers) the agent should use bg_run instead.
        let timeout_secs = args.get("timeout").and_then(|v| v.as_u64());

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

        let output_result = match timeout_secs {
            Some(secs) => match tokio::time::timeout(std::time::Duration::from_secs(secs), cmd.output()).await {
                Ok(r) => r,
                Err(_) => {
                    return Err(anyhow::anyhow!(
                        "command timed out after {secs}s. For long-lived processes (dev servers, watchers), use `bg_run` instead so the agent isn't blocked."
                    ));
                }
            },
            None => cmd.output().await,
        };

        match output_result {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let code = out.status.code().unwrap_or(-1);
                // Truncate very long outputs to avoid blowing out the LLM context.
                let max = 50_000;
                let stdout_str = if stdout.len() > max {
                    format!("{}...\n[truncated, {} total chars]", &stdout[..max], stdout.len())
                } else {
                    stdout.to_string()
                };
                let stderr_str = if stderr.len() > max {
                    format!("{}...\n[truncated, {} total chars]", &stderr[..max], stderr.len())
                } else {
                    stderr.to_string()
                };
                Ok(format!("exit code: {code}\n--- stdout ---\n{stdout_str}\n--- stderr ---\n{stderr_str}"))
            }
            Err(e) => Err(anyhow::anyhow!("command failed to start: {e}")),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// BgRunTool + companions — manage long-lived background processes
// (dev servers, watchers, databases) that outlive a single tool call.
// Bash has a 120s timeout; these tools let the agent detach a process
// and check on it later.
// ---------------------------------------------------------------------------

struct BgRunTool {
    base: PathBuf,
    registry: Arc<bg_process::BgRegistry>,
    proxy_url: Option<String>,
}

#[async_trait]
impl Tool for BgRunTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bg_run".into(),
            description: "Run a long-lived process in the background (dev servers, watchers). Returns a handle (e.g. 'bg-1'). Use bg_status/bg_logs/bg_kill to manage it. This is the ONLY way to run commands that don't return — `bash` would time out.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to run" }
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
        let env_vars = if let Some(ref proxy) = self.proxy_url {
            vec![
                ("HTTP_PROXY".into(), proxy.clone()),
                ("http_proxy".into(), proxy.clone()),
                ("HTTPS_PROXY".into(), proxy.clone()),
                ("https_proxy".into(), proxy.clone()),
                ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
                ("no_proxy".into(), "127.0.0.1,localhost".into()),
            ]
        } else {
            Vec::new()
        };
        let handle = self.registry.run(command, &self.base.to_string_lossy(), &env_vars)?;
        Ok(format!(
            "started: {handle}\ncommand: {command}\n\nUse bg_status('{handle}'), bg_logs('{handle}'), or bg_kill('{handle}') to manage it."
        ))
    }

    fn is_concurrent_safe(&self) -> bool {
        false
    }
}

struct BgStatusTool {
    registry: Arc<bg_process::BgRegistry>,
}

#[async_trait]
impl Tool for BgStatusTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bg_status".into(),
            description: "Check status of a background process. If `handle` is omitted, lists all.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "Handle returned by bg_run (e.g. 'bg-1'). Omit to list all." }
                }
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let handle = args.get("handle").and_then(|v| v.as_str());
        if let Some(h) = handle {
            let st = self.registry.status(h)?;
            Ok(format_bg_status(&st))
        } else {
            let list = self.registry.list();
            if list.is_empty() {
                return Ok("no background processes".into());
            }
            let mut out = String::new();
            for st in list {
                out.push_str(&format_bg_status(&st));
                out.push('\n');
            }
            Ok(out)
        }
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

fn format_bg_status(st: &bg_process::BgStatus) -> String {
    let state = if st.running {
        "running".to_string()
    } else {
        match st.exit_code {
            Some(code) => format!("exited (code {code})"),
            None => "exited".to_string(),
        }
    };
    format!("{}: {} | uptime {}s | `{}`", st.handle, state, st.uptime_secs, st.command)
}

struct BgLogsTool {
    registry: Arc<bg_process::BgRegistry>,
}

#[async_trait]
impl Tool for BgLogsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bg_logs".into(),
            description: "Tail recent stdout+stderr from a background process. Default: 8KB of each stream.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "Handle returned by bg_run" },
                    "max_bytes": { "type": "integer", "description": "Max bytes to return per stream (default: 8192)" }
                },
                "required": ["handle"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let handle = args.get("handle").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("missing 'handle'"))?;
        let max_bytes = args.get("max_bytes").and_then(|v| v.as_u64()).unwrap_or(8192) as usize;
        let (stdout, stderr) = self.registry.logs(handle, max_bytes)?;
        Ok(format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

struct BgKillTool {
    registry: Arc<bg_process::BgRegistry>,
}

#[async_trait]
impl Tool for BgKillTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bg_kill".into(),
            description: "Stop a background process (SIGTERM with a short grace period, then SIGKILL if it doesn't exit).".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "Handle returned by bg_run" }
                },
                "required": ["handle"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let handle = args.get("handle").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("missing 'handle'"))?;
        self.registry.kill(handle, std::time::Duration::from_secs(3)).await?;
        Ok(format!("killed {handle}"))
    }
}

// ---------------------------------------------------------------------------
// HttpFetchTool — structured HTTP request for the agent to probe
// servers it just started in bg_run. First-class alternative to
// `bash("curl ...")` that returns status + headers + body as a
// structured summary the LLM can reason about.
// ---------------------------------------------------------------------------

struct HttpFetchTool {
    client: reqwest::Client,
}

#[async_trait]
impl Tool for HttpFetchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "http_fetch".into(),
            description: "Make an HTTP request and return status, headers, and body. Perfect for probing a dev server you started with bg_run.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to fetch (http:// or https://)" },
                    "method": { "type": "string", "description": "HTTP method (default: GET)" },
                    "body": { "type": "string", "description": "Request body (optional, use with POST/PUT/PATCH)" },
                    "headers": { "type": "object", "description": "Extra request headers as an object" },
                    "timeout": { "type": "integer", "description": "Timeout in seconds (default: 10)" }
                },
                "required": ["url"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("missing 'url'"))?;
        let method = args.get("method").and_then(|v| v.as_str()).unwrap_or("GET").to_uppercase();
        let body = args.get("body").and_then(|v| v.as_str());
        let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(10);

        let req_method = match method.as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "PATCH" => reqwest::Method::PATCH,
            "DELETE" => reqwest::Method::DELETE,
            "HEAD" => reqwest::Method::HEAD,
            other => return Err(anyhow::anyhow!("unsupported HTTP method: {other}")),
        };

        let mut builder = self.client.request(req_method, url).timeout(std::time::Duration::from_secs(timeout_secs));
        if let Some(headers_obj) = args.get("headers").and_then(|v| v.as_object()) {
            for (k, v) in headers_obj {
                if let Some(val) = v.as_str() {
                    builder = builder.header(k, val);
                }
            }
        }
        if let Some(b) = body {
            builder = builder.body(b.to_string());
        }

        let start = std::time::Instant::now();
        let resp = builder.send().await.map_err(|e| anyhow::anyhow!("HTTP {method} {url} failed: {e}"))?;
        let status = resp.status();
        let elapsed_ms = start.elapsed().as_millis();

        // Snapshot headers we care about before consuming the body.
        let mut header_summary = String::new();
        for (k, v) in resp.headers().iter().take(20) {
            header_summary.push_str(&format!("  {}: {}\n", k, v.to_str().unwrap_or("<binary>")));
        }

        let body_text = resp.text().await.unwrap_or_default();
        let body_tail = if body_text.len() > 8192 {
            format!("{}...\n[truncated, {} total bytes]", &body_text[..8192], body_text.len())
        } else {
            body_text
        };

        Ok(format!(
            "{method} {url}\n\
             status: {status} ({})\n\
             elapsed: {elapsed_ms}ms\n\
             headers:\n{header_summary}\n\
             --- body ---\n{body_tail}",
            status.canonical_reason().unwrap_or("")
        ))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// ProjectInspectTool — detect language, framework, package manager,
// and common scripts from workspace manifests. Fast cold-start for the
// agent working on an unfamiliar codebase.
// ---------------------------------------------------------------------------

struct ProjectInspectTool {
    base: PathBuf,
}

#[async_trait]
impl Tool for ProjectInspectTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "project_inspect".into(),
            description: "Detect project type (language, framework, package manager) and common scripts (dev/test/build) from manifest files. Call this FIRST on an unfamiliar project instead of grep-ing around.".into(),
            parameters: serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
        }
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<String> {
        let base = self.base.clone();
        tokio::task::spawn_blocking(move || inspect_project(&base)).await?
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

fn inspect_project(base: &std::path::Path) -> anyhow::Result<String> {
    let mut summary = String::new();
    let mut languages: Vec<&str> = Vec::new();

    // Rust
    if base.join("Cargo.toml").exists() {
        languages.push("rust");
        summary.push_str("## Rust (Cargo.toml detected)\n");
        summary.push_str("- build: cargo build\n- test: cargo test\n- check: cargo check\n- lint: cargo clippy\n- format: cargo fmt\n");
        if base.join("Cargo.lock").exists() {
            summary.push_str("- Cargo.lock present (binary/app project)\n");
        }
        summary.push('\n');
    }

    // Node / TypeScript
    let pkg_json = base.join("package.json");
    if pkg_json.exists() {
        if let Ok(contents) = std::fs::read_to_string(&pkg_json) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&contents) {
                languages.push("typescript/javascript");
                summary.push_str("## Node (package.json detected)\n");

                // Package manager
                let pm = if base.join("pnpm-lock.yaml").exists() {
                    "pnpm"
                } else if base.join("yarn.lock").exists() {
                    "yarn"
                } else if base.join("bun.lockb").exists() {
                    "bun"
                } else {
                    "npm"
                };
                summary.push_str(&format!("- package manager: {pm}\n"));

                // Scripts
                if let Some(scripts) = parsed.get("scripts").and_then(|v| v.as_object()) {
                    summary.push_str("- scripts:\n");
                    for (name, cmd) in scripts.iter().take(15) {
                        let cmd_str = cmd.as_str().unwrap_or("");
                        summary.push_str(&format!("  - {name}: `{cmd_str}`\n"));
                    }
                }

                // Framework heuristics
                let deps = parsed.get("dependencies").and_then(|v| v.as_object());
                let dev_deps = parsed.get("devDependencies").and_then(|v| v.as_object());
                let has_dep = |name: &str| -> bool { deps.is_some_and(|d| d.contains_key(name)) || dev_deps.is_some_and(|d| d.contains_key(name)) };
                let framework = if has_dep("next") {
                    Some("Next.js")
                } else if has_dep("vite") {
                    Some("Vite")
                } else if has_dep("hono") {
                    Some("Hono")
                } else if has_dep("express") {
                    Some("Express")
                } else if has_dep("fastify") {
                    Some("Fastify")
                } else if has_dep("react") {
                    Some("React")
                } else {
                    None
                };
                if let Some(fw) = framework {
                    summary.push_str(&format!("- framework: {fw}\n"));
                }
                summary.push('\n');
            }
        }
    }

    // Python
    if base.join("pyproject.toml").exists() || base.join("setup.py").exists() || base.join("requirements.txt").exists() {
        languages.push("python");
        summary.push_str("## Python\n");
        if base.join("pyproject.toml").exists() {
            summary.push_str("- pyproject.toml present\n");
            if let Ok(contents) = std::fs::read_to_string(base.join("pyproject.toml")) {
                if contents.contains("[tool.poetry]") {
                    summary.push_str("- package manager: poetry\n");
                } else if contents.contains("[tool.uv]") {
                    summary.push_str("- package manager: uv\n");
                } else if contents.contains("[tool.hatch") {
                    summary.push_str("- package manager: hatch\n");
                } else {
                    summary.push_str("- package manager: pip (PEP 621)\n");
                }
                if contents.contains("fastapi") {
                    summary.push_str("- framework: FastAPI\n");
                } else if contents.contains("django") {
                    summary.push_str("- framework: Django\n");
                } else if contents.contains("flask") {
                    summary.push_str("- framework: Flask\n");
                }
                if contents.contains("pytest") {
                    summary.push_str("- test: pytest\n");
                }
            }
        }
        if base.join("requirements.txt").exists() {
            summary.push_str("- requirements.txt present (pip install -r)\n");
        }
        summary.push('\n');
    }

    // Go
    if base.join("go.mod").exists() {
        languages.push("go");
        summary.push_str("## Go (go.mod detected)\n");
        summary.push_str("- build: go build ./...\n- test: go test ./...\n- format: gofmt -w .\n");
        summary.push('\n');
    }

    // Git
    if base.join(".git").exists() {
        summary.push_str("## Git\n- repository present\n\n");
    }

    // .env files
    let env_files: Vec<_> = [".env", ".env.local", ".env.example"].iter().filter(|f| base.join(f).exists()).collect();
    if !env_files.is_empty() {
        summary.push_str("## Env\n");
        for f in env_files {
            summary.push_str(&format!("- {f} present\n"));
        }
        summary.push('\n');
    }

    if languages.is_empty() {
        Ok("No known project manifests detected (looked for Cargo.toml, package.json, pyproject.toml, setup.py, requirements.txt, go.mod).".into())
    } else {
        Ok(format!("Project languages: {}\n\n{}", languages.join(", "), summary))
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

        // Task can come from either SMOOTH_TASK (inline) or SMOOTH_TASK_FILE
        // (path to a file). The file form is used for long task messages
        // because env vars in microsandbox flow through the kernel cmdline,
        // which has a hard size limit (~2 KB on aarch64). Big Smooth writes
        // to /opt/smooth/task.txt and sets SMOOTH_TASK_FILE when the message
        // would otherwise overflow.
        let task = if let Ok(path) = std::env::var("SMOOTH_TASK_FILE") {
            std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("read SMOOTH_TASK_FILE {path}: {e}"))?
        } else {
            require("SMOOTH_TASK")?
        };

        Ok(Self {
            task,
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
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
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
            eprintln!("[runner] policy source: SMOOTH_POLICY_TOML inline ({} bytes)", inline.len());
            return inline;
        }
    }
    if let Ok(file) = std::env::var("SMOOTH_POLICY_FILE") {
        match std::fs::read_to_string(&file) {
            Ok(contents) => {
                eprintln!("[runner] policy source: SMOOTH_POLICY_FILE={file} ({} bytes)", contents.len());
                return contents;
            }
            Err(e) => {
                eprintln!("[runner] SMOOTH_POLICY_FILE={file} read failed: {e}");
            }
        }
    } else {
        eprintln!("[runner] SMOOTH_POLICY_FILE env var not set");
    }
    if let Ok(contents) = std::fs::read_to_string("/opt/smooth/policy.toml") {
        eprintln!("[runner] policy source: /opt/smooth/policy.toml fallback ({} bytes)", contents.len());
        return contents;
    }
    eprintln!("[runner] policy source: HARDCODED DEFAULT (no SMOOTH_POLICY_TOML, SMOOTH_POLICY_FILE, or /opt/smooth/policy.toml)");
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

    // Narc escalation client — every denied /check/network and
    // /check/cli call that Wonk can't auto-approve locally gets forwarded
    // to this URL, which points at Big Smooth's `/api/narc/judge` endpoint.
    // If SMOOTH_NARC_URL isn't set (e.g. a standalone unit test), Wonk
    // runs without an arbiter and hard-denies anything its local policy
    // doesn't allow — the legacy behaviour.
    let mut wonk_state = WonkAppState::new(holder, negotiator);
    if let Ok(narc_url) = std::env::var("SMOOTH_NARC_URL") {
        if !narc_url.trim().is_empty() {
            tracing::info!(operator = operator_id, narc_url = %narc_url, "Wonk wiring Narc escalation client");
            wonk_state = wonk_state.with_narc(smooth_wonk::NarcClient::new(narc_url));
        }
    }
    let wonk_state = Arc::new(wonk_state);
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
    //
    // Diagnostic: parse the policy TOML so we can echo the network allowlist
    // and policy source path back through stdout. This makes it possible to
    // verify from a test (which only sees runner stdout) which policy the
    // sandbox is actually enforcing.
    if let Ok(parsed) = smooth_policy::Policy::from_toml(&config.policy_toml) {
        let domains: Vec<String> = parsed.network.allow.iter().map(|r| r.domain.clone()).collect();
        emit_event(&AgentEvent::TokenDelta {
            content: format!(
                "[runner] loaded policy: phase={}, allowed_domains=[{}], total_rules={}\n",
                parsed.metadata.phase,
                domains.join(", "),
                parsed.network.allow.len()
            ),
        });
    } else {
        emit_event(&AgentEvent::TokenDelta {
            content: format!("[runner] FAILED to parse policy TOML ({} bytes)\n", config.policy_toml.len()),
        });
    }

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
    //
    // Shared state: file-time tracker ensures the agent can't silently
    // clobber externally-modified files (multi-agent safety).
    let file_tracker = Arc::new(tool_support::FileTimeTracker::new());

    let mut tools = ToolRegistry::new();
    tools.register(ReadFileTool {
        base: config.workspace.clone(),
        file_tracker: Arc::clone(&file_tracker),
    });
    tools.register(WriteFileTool {
        base: config.workspace.clone(),
        file_tracker: Arc::clone(&file_tracker),
    });
    tools.register(EditFileTool {
        base: config.workspace.clone(),
        file_tracker: Arc::clone(&file_tracker),
    });
    tools.register(ApplyPatchTool {
        base: config.workspace.clone(),
    });
    tools.register(ListFilesTool {
        base: config.workspace.clone(),
    });
    tools.register(GrepTool {
        base: config.workspace.clone(),
    });
    tools.register(LspTool {
        base: config.workspace.clone(),
        client: Arc::new(tokio::sync::Mutex::new(None)),
    });
    tools.register(BashTool {
        base: config.workspace.clone(),
        proxy_url: proxy_for_bash.clone(),
    });
    tools.register(ProjectInspectTool {
        base: config.workspace.clone(),
    });

    // Background process tools — share one registry so bg_status /
    // bg_logs / bg_kill can see the processes spawned by bg_run.
    let bg_registry = Arc::new(bg_process::BgRegistry::new());
    tools.register(BgRunTool {
        base: config.workspace.clone(),
        registry: Arc::clone(&bg_registry),
        proxy_url: proxy_for_bash,
    });
    tools.register(BgStatusTool {
        registry: Arc::clone(&bg_registry),
    });
    tools.register(BgLogsTool {
        registry: Arc::clone(&bg_registry),
    });
    tools.register(BgKillTool {
        registry: Arc::clone(&bg_registry),
    });

    // HTTP probe — structured fetch for checking on servers the agent
    // started via bg_run.
    let http_client = reqwest::Client::builder().build().unwrap_or_else(|_| reqwest::Client::new());
    tools.register(HttpFetchTool { client: http_client });

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

    // System prompt is compiled in from prompts/system.md. This is the
    // agent harness — tool guidance, workflow constraints, error recovery.
    // NOT customizable per-project; AGENTS.md / CLAUDE.md handle that
    // (loaded below via load_project_context and appended as ## Project Context).
    let has_pearl_tools = tools.schemas().iter().any(|s| s.name == "create_pearl");
    let pearl_note = if has_pearl_tools {
        "\nYou also have create_pearl, list_pearls, and close_pearl tools for tracking work items."
    } else {
        ""
    };
    let base_prompt = format!("{}{pearl_note}", include_str!("../prompts/system.md"));
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

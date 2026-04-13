//! LSP client — spawns a language server as a sidecar process and
//! communicates via JSON-RPC over stdio.
//!
//! The agent uses this through the `lsp` tool to do goToDefinition,
//! findReferences, hover, documentSymbol, etc. The language server is
//! lazily spawned on the first LSP tool invocation and reused for the
//! lifetime of the runner.
//!
//! Supported servers:
//! - rust-analyzer (Rust, detected via Cargo.toml)
//! - typescript-language-server (TypeScript/JS, detected via package.json/tsconfig.json)
//! - ty (Python, detected via pyproject.toml — Astral's Rust-based type checker)
//! - gopls (Go, detected via go.mod)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use lsp_types::*;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{oneshot, Mutex};

/// Convert a filesystem path to a `file:///...` URI.
fn path_to_uri(path: &Path) -> anyhow::Result<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let uri_str = format!("file://{}", abs.display());
    Uri::from_str(&uri_str).map_err(|e| anyhow::anyhow!("invalid URI for {}: {e}", path.display()))
}

/// Convert a `file:///...` URI back to a filesystem path.
fn uri_to_path(uri: &Uri) -> PathBuf {
    let s = uri.as_str();
    if let Some(path) = s.strip_prefix("file://") {
        PathBuf::from(path)
    } else {
        PathBuf::from(s)
    }
}

// ---------------------------------------------------------------------------
// Language server detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
}

impl Language {
    /// Detect the primary language from workspace files.
    pub fn detect(workspace: &Path) -> Option<Self> {
        if workspace.join("Cargo.toml").exists() {
            Some(Self::Rust)
        } else if workspace.join("tsconfig.json").exists() || workspace.join("package.json").exists() {
            Some(Self::TypeScript)
        } else if workspace.join("pyproject.toml").exists() || workspace.join("setup.py").exists() {
            Some(Self::Python)
        } else if workspace.join("go.mod").exists() {
            Some(Self::Go)
        } else {
            None
        }
    }

    /// The command to spawn the language server.
    fn server_command(self) -> (&'static str, Vec<&'static str>) {
        match self {
            Self::Rust => ("rust-analyzer", vec![]),
            Self::TypeScript => ("typescript-language-server", vec!["--stdio"]),
            Self::Python => ("ty", vec!["server"]),
            Self::Go => ("gopls", vec!["serve"]),
        }
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC framing
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: i64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<i64>,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

// ---------------------------------------------------------------------------
// LSP Client
// ---------------------------------------------------------------------------

/// A running language server process with JSON-RPC communication.
pub struct LspClient {
    stdin: ChildStdin,
    /// Background task that reads stdout and dispatches responses.
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>>,
    next_id: AtomicI64,
    language: Language,
    #[allow(dead_code)]
    child: Child,
    initialized: bool,
}

impl LspClient {
    /// Spawn a language server for the detected language and send the
    /// LSP `initialize` handshake.
    pub async fn start(workspace: &Path) -> anyhow::Result<Self> {
        let language = Language::detect(workspace)
            .ok_or_else(|| anyhow::anyhow!("cannot detect project language (no Cargo.toml, package.json, tsconfig.json, pyproject.toml, or go.mod found)"))?;

        let (cmd, args) = language.server_command();
        tracing::info!(language = ?language, cmd, "spawning language server");

        let mut child = tokio::process::Command::new(cmd)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .current_dir(workspace)
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn {cmd}: {e}. Is it installed?"))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin for {cmd}"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout for {cmd}"))?;

        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>> = Arc::new(Mutex::new(HashMap::new()));

        // Spawn a background reader that dispatches responses to waiting callers.
        let pending_clone = Arc::clone(&pending);
        tokio::spawn(async move {
            if let Err(e) = read_responses(stdout, pending_clone).await {
                tracing::warn!(error = %e, "LSP response reader exited");
            }
        });

        let mut client = Self {
            stdin,
            pending,
            next_id: AtomicI64::new(1),
            language,
            child,
            initialized: false,
        };

        // Send initialize.
        let workspace_uri = path_to_uri(workspace)?;
        let init_params = InitializeParams {
            root_uri: Some(workspace_uri.clone()),
            capabilities: ClientCapabilities::default(),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: workspace_uri,
                name: "workspace".into(),
            }]),
            ..InitializeParams::default()
        };

        let _init_result: serde_json::Value = client.request("initialize", Some(serde_json::to_value(init_params)?)).await?;
        client.notify("initialized", Some(serde_json::json!({}))).await?;
        client.initialized = true;

        tracing::info!(language = ?language, "LSP server initialized");
        Ok(client)
    }

    /// Send a request and wait for the response.
    async fn request(&mut self, method: &str, params: Option<serde_json::Value>) -> anyhow::Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let msg = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        };

        let body = serde_json::to_string(&msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        self.stdin.write_all(header.as_bytes()).await?;
        self.stdin.write_all(body.as_bytes()).await?;
        self.stdin.flush().await?;

        // Register a oneshot channel for this request ID.
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        // Wait for the response with a timeout.
        let response = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .map_err(|_| anyhow::anyhow!("LSP request '{method}' timed out after 30s"))?
            .map_err(|_| anyhow::anyhow!("LSP response channel dropped for '{method}'"))?;

        if let Some(error) = response.error {
            return Err(anyhow::anyhow!("LSP error {}: {}", error.code, error.message));
        }

        Ok(response.result.unwrap_or(serde_json::Value::Null))
    }

    /// Send a notification (no response expected).
    async fn notify(&mut self, method: &str, params: Option<serde_json::Value>) -> anyhow::Result<()> {
        // Notifications have no id field.
        let body = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes()).await?;
        self.stdin.write_all(body.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Open a text document so the language server knows about it.
    pub async fn open_file(&mut self, workspace: &Path, rel_path: &str) -> anyhow::Result<()> {
        let path = workspace.join(rel_path);
        let content = tokio::fs::read_to_string(&path).await?;
        let uri = path_to_uri(&path)?;

        let language_id = match self.language {
            Language::Rust => "rust",
            Language::TypeScript => {
                if rel_path.ends_with(".tsx") {
                    "typescriptreact"
                } else {
                    "typescript"
                }
            }
            Language::Python => "python",
            Language::Go => "go",
        };

        self.notify(
            "textDocument/didOpen",
            Some(serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: language_id.into(),
                    version: 1,
                    text: content,
                },
            })?),
        )
        .await
    }

    // ── High-level operations ────────────────────────────────

    pub async fn go_to_definition(&mut self, workspace: &Path, rel_path: &str, line: u32, character: u32) -> anyhow::Result<String> {
        self.open_file(workspace, rel_path).await?;
        let path = workspace.join(rel_path);
        let uri = path_to_uri(&path)?;

        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self.request("textDocument/definition", Some(serde_json::to_value(params)?)).await?;
        format_location_response(workspace, &result)
    }

    pub async fn find_references(&mut self, workspace: &Path, rel_path: &str, line: u32, character: u32) -> anyhow::Result<String> {
        self.open_file(workspace, rel_path).await?;
        let path = workspace.join(rel_path);
        let uri = path_to_uri(&path)?;

        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            context: ReferenceContext { include_declaration: true },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self.request("textDocument/references", Some(serde_json::to_value(params)?)).await?;
        format_location_response(workspace, &result)
    }

    pub async fn hover(&mut self, workspace: &Path, rel_path: &str, line: u32, character: u32) -> anyhow::Result<String> {
        self.open_file(workspace, rel_path).await?;
        let path = workspace.join(rel_path);
        let uri = path_to_uri(&path)?;

        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
        };

        let result = self.request("textDocument/hover", Some(serde_json::to_value(params)?)).await?;
        if result.is_null() {
            return Ok("no hover information available at this position".into());
        }
        let hover: HoverContents = serde_json::from_value(result.get("contents").cloned().unwrap_or(serde_json::Value::Null))?;
        Ok(format_hover_contents(&hover))
    }

    pub async fn document_symbols(&mut self, workspace: &Path, rel_path: &str) -> anyhow::Result<String> {
        self.open_file(workspace, rel_path).await?;
        let path = workspace.join(rel_path);
        let uri = path_to_uri(&path)?;

        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self.request("textDocument/documentSymbol", Some(serde_json::to_value(params)?)).await?;
        format_symbols_response(&result)
    }

    pub async fn workspace_symbols(&mut self, query: &str) -> anyhow::Result<String> {
        let params = WorkspaceSymbolParams {
            query: query.into(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self.request("workspace/symbol", Some(serde_json::to_value(params)?)).await?;
        format_symbols_response(&result)
    }

    pub async fn diagnostics(&mut self, workspace: &Path, rel_path: &str) -> anyhow::Result<String> {
        // Most servers push diagnostics via notifications after didOpen.
        // We open the file, give the server a moment, then query.
        self.open_file(workspace, rel_path).await?;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // For servers that support pull diagnostics (LSP 3.17+):
        let path = workspace.join(rel_path);
        let uri = path_to_uri(&path)?;

        let params = serde_json::json!({
            "textDocument": { "uri": uri.as_str() }
        });
        match self.request("textDocument/diagnostic", Some(params)).await {
            Ok(result) => format_diagnostics_response(&result),
            Err(_) => Ok("diagnostics not available (server may not support pull diagnostics)".into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Background response reader
// ---------------------------------------------------------------------------

async fn read_responses(stdout: ChildStdout, pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>>) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stdout);
    let mut header_buf = String::new();

    loop {
        // Read headers until blank line.
        let mut content_length: Option<usize> = None;
        loop {
            header_buf.clear();
            let n = reader.read_line(&mut header_buf).await?;
            if n == 0 {
                return Ok(()); // EOF — server exited.
            }
            let trimmed = header_buf.trim();
            if trimmed.is_empty() {
                break; // End of headers.
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
                content_length = len_str.trim().parse().ok();
            }
        }

        let Some(len) = content_length else {
            continue; // Malformed message — skip.
        };

        // Read the body.
        let mut body = vec![0u8; len];
        tokio::io::AsyncReadExt::read_exact(&mut reader, &mut body).await?;
        let body_str = String::from_utf8_lossy(&body);

        // Try to parse as a response (has an `id` field).
        if let Ok(response) = serde_json::from_str::<JsonRpcResponse>(&body_str) {
            if let Some(id) = response.id {
                let mut map = pending.lock().await;
                if let Some(tx) = map.remove(&id) {
                    let _ = tx.send(response);
                }
            }
            // Notifications (no id) are silently dropped for now.
            // Future: accumulate published diagnostics.
        }
    }
}

// ---------------------------------------------------------------------------
// Response formatting helpers
// ---------------------------------------------------------------------------

fn format_location_response(workspace: &Path, result: &serde_json::Value) -> anyhow::Result<String> {
    if result.is_null() {
        return Ok("no definition found".into());
    }

    let locations: Vec<Location> = if result.is_array() {
        serde_json::from_value(result.clone())?
    } else if result.get("uri").is_some() {
        vec![serde_json::from_value(result.clone())?]
    } else if result.get("targetUri").is_some() {
        // LocationLink format
        let links: Vec<LocationLink> = if result.is_array() {
            serde_json::from_value(result.clone())?
        } else {
            vec![serde_json::from_value(result.clone())?]
        };
        links
            .into_iter()
            .map(|link| Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect()
    } else {
        return Ok(format!("unexpected response shape: {result}"));
    };

    let mut output = String::new();
    for loc in &locations {
        let path = uri_to_path(&loc.uri);
        let rel = path.strip_prefix(workspace).unwrap_or(&path);
        let line = loc.range.start.line + 1;
        let col = loc.range.start.character + 1;
        output.push_str(&format!("{}:{}:{}\n", rel.display(), line, col));
    }
    if output.is_empty() {
        output.push_str("no results");
    }
    Ok(output)
}

fn format_hover_contents(contents: &HoverContents) -> String {
    match contents {
        HoverContents::Scalar(MarkedString::String(s) | MarkedString::LanguageString(LanguageString { value: s, .. })) => s.clone(),
        HoverContents::Array(items) => items
            .iter()
            .map(|item| match item {
                MarkedString::String(s) | MarkedString::LanguageString(LanguageString { value: s, .. }) => s.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(MarkupContent { value, .. }) => value.clone(),
    }
}

fn format_symbols_response(result: &serde_json::Value) -> anyhow::Result<String> {
    if result.is_null() || (result.is_array() && result.as_array().is_some_and(|a| a.is_empty())) {
        return Ok("no symbols found".into());
    }

    let mut output = String::new();
    if let Some(arr) = result.as_array() {
        for item in arr.iter().take(50) {
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let kind = item.get("kind").and_then(|v| v.as_u64()).unwrap_or(0);
            let kind_str = symbol_kind_name(kind);
            if let Some(loc) = item.get("location") {
                let line = loc
                    .get("range")
                    .and_then(|r| r.get("start"))
                    .and_then(|s| s.get("line"))
                    .and_then(|l| l.as_u64())
                    .map(|l| l + 1)
                    .unwrap_or(0);
                output.push_str(&format!("{kind_str} {name} (line {line})\n"));
            } else if let Some(range) = item.get("range") {
                let line = range
                    .get("start")
                    .and_then(|s| s.get("line"))
                    .and_then(|l| l.as_u64())
                    .map(|l| l + 1)
                    .unwrap_or(0);
                output.push_str(&format!("{kind_str} {name} (line {line})\n"));
            } else {
                output.push_str(&format!("{kind_str} {name}\n"));
            }
        }
    }
    if output.is_empty() {
        output.push_str("no symbols found");
    }
    Ok(output)
}

fn format_diagnostics_response(result: &serde_json::Value) -> anyhow::Result<String> {
    let items = result.get("items").and_then(|v| v.as_array()).or_else(|| result.as_array());
    let Some(items) = items else {
        return Ok("no diagnostics".into());
    };
    let mut output = String::new();
    for diag in items.iter().take(30) {
        let severity = diag.get("severity").and_then(|v| v.as_u64()).unwrap_or(0);
        let sev_str = match severity {
            1 => "error",
            2 => "warning",
            3 => "info",
            4 => "hint",
            _ => "?",
        };
        let msg = diag.get("message").and_then(|v| v.as_str()).unwrap_or("");
        let line = diag
            .get("range")
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(|l| l.as_u64())
            .map(|l| l + 1)
            .unwrap_or(0);
        output.push_str(&format!("line {line}: [{sev_str}] {msg}\n"));
    }
    if output.is_empty() {
        output.push_str("no diagnostics");
    }
    Ok(output)
}

fn symbol_kind_name(kind: u64) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        22 => "struct",
        23 => "event",
        24 => "operator",
        25 => "type_param",
        _ => "symbol",
    }
}

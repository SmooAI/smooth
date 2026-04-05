//! Headless (non-interactive) mode for smooth-code.
//!
//! Runs as a client of Big Smooth — connects to the `/ws` WebSocket endpoint,
//! sends a `TaskStart` event, and streams `ServerEvent`s to stdout/stderr.
//! Falls back to the SSE `/api/tasks` endpoint if WebSocket connection fails.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite;

// ---------------------------------------------------------------------------
// WebSocket event types (mirrors smooth-bigsmooth::events)
// ---------------------------------------------------------------------------

/// Client-to-server event sent over WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ClientEvent {
    TaskStart {
        message: String,
        model: Option<String>,
        budget: Option<f64>,
        working_dir: Option<String>,
    },
}

/// Server-to-client event received over WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ServerEvent {
    TokenDelta {
        task_id: String,
        content: String,
    },
    ToolCallStart {
        task_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolCallComplete {
        task_id: String,
        tool_name: String,
        result: String,
        is_error: bool,
        duration_ms: u64,
    },
    TaskComplete {
        task_id: String,
        iterations: u32,
        cost_usd: f64,
    },
    TaskError {
        task_id: String,
        message: String,
    },
    Pong,
    Error {
        message: String,
    },
    Connected {
        session_id: String,
    },
    #[serde(other)]
    Unknown,
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
// Big Smooth lifecycle helpers
// ---------------------------------------------------------------------------

/// Start Big Smooth by spawning `th up` as a background process and waiting
/// for it to become healthy.
async fn start_bigsmooth_background() -> anyhow::Result<()> {
    // Find the `th` binary — it should be on PATH or be `self`
    let th_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("th"));

    // Spawn `th up` as a detached background process
    let _child = tokio::process::Command::new(&th_bin)
        .arg("up")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn Big Smooth (th up): {e}"))?;

    // Wait for health check (up to 10s)
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if client.get("http://localhost:4400/health").send().await.is_ok_and(|r| r.status().is_success()) {
            return Ok(());
        }
    }

    anyhow::bail!("Big Smooth failed to start within 10 seconds")
}

// ---------------------------------------------------------------------------
// Headless entry point
// ---------------------------------------------------------------------------

/// Run smooth-code in headless (non-interactive) mode.
///
/// Connects to Big Smooth over WebSocket at `ws://localhost:4400/ws`,
/// sends a `TaskStart` event, and streams `ServerEvent`s to stdout/stderr.
///
/// Falls back to the legacy SSE `/api/tasks` endpoint if WebSocket fails.
///
/// # Errors
/// Returns an error if the message is empty, Big Smooth cannot be reached,
/// or the task fails.
pub async fn run_headless(working_dir: PathBuf, message: String, model: Option<String>, budget: Option<f64>, json_output: bool) -> anyhow::Result<()> {
    if message.trim().is_empty() {
        anyhow::bail!("message must not be empty");
    }

    // 1. Ensure Big Smooth is running
    let health_client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;
    let health = health_client.get("http://localhost:4400/health").send().await;

    if health.is_err() || !health.as_ref().is_ok_and(|r| r.status().is_success()) {
        eprintln!("Starting Big Smooth...");
        start_bigsmooth_background().await?;
    }

    // 2. Try WebSocket first, fall back to SSE
    let ws_url = "ws://localhost:4400/ws";
    match tokio_tungstenite::connect_async(ws_url).await {
        Ok((ws_stream, _)) => run_headless_ws(ws_stream, working_dir, message, model, budget, json_output).await,
        Err(e) => {
            tracing::debug!(error = %e, "WebSocket connection failed, falling back to SSE");
            run_headless_sse(working_dir, message, model, budget, json_output).await
        }
    }
}

/// Run headless via WebSocket connection.
async fn run_headless_ws(
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    working_dir: PathBuf,
    message: String,
    model: Option<String>,
    budget: Option<f64>,
    json_output: bool,
) -> anyhow::Result<()> {
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Wait for Connected event
    let mut connected = false;
    if let Some(Ok(tungstenite::Message::Text(text))) = ws_rx.next().await {
        if let Ok(ServerEvent::Connected { .. }) = serde_json::from_str(&text) {
            connected = true;
        }
    }
    if !connected {
        anyhow::bail!("Did not receive Connected event from Big Smooth");
    }

    // Send TaskStart
    let task_start = ClientEvent::TaskStart {
        message,
        model,
        budget,
        working_dir: Some(working_dir.to_string_lossy().into_owned()),
    };
    let json = serde_json::to_string(&task_start)?;
    ws_tx
        .send(tungstenite::Message::Text(json.into()))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send TaskStart: {e}"))?;

    // Stream events
    let mut content_buf = String::new();
    let mut tool_calls: Vec<HeadlessToolCall> = Vec::new();
    let mut cost = 0.0_f64;

    while let Some(Ok(msg)) = ws_rx.next().await {
        let text = match msg {
            tungstenite::Message::Text(t) => t.to_string(),
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };

        let Ok(event) = serde_json::from_str::<ServerEvent>(&text) else {
            continue;
        };

        match event {
            ServerEvent::TokenDelta { content, .. } => {
                content_buf.push_str(&content);
                if !json_output {
                    print!("{content}");
                    let _ = std::io::stdout().flush();
                }
            }
            ServerEvent::ToolCallStart { tool_name, .. } => {
                eprintln!("[tool] {tool_name}(...)");
            }
            ServerEvent::ToolCallComplete { tool_name, is_error, .. } => {
                let status = if is_error { "error" } else { "ok" };
                eprintln!("[tool] {tool_name} -> {status}");
                tool_calls.push(HeadlessToolCall {
                    name: tool_name,
                    success: !is_error,
                });
            }
            ServerEvent::TaskComplete { iterations, cost_usd, .. } => {
                cost = cost_usd;
                eprintln!("[done] completed in {iterations} iterations");
                break;
            }
            ServerEvent::TaskError { message, .. } => {
                eprintln!("[error] {message}");
                anyhow::bail!("Task failed: {message}");
            }
            ServerEvent::Error { message } => {
                eprintln!("[error] {message}");
            }
            ServerEvent::Pong | ServerEvent::Connected { .. } | ServerEvent::Unknown => {}
        }
    }

    // Close WebSocket cleanly
    let _ = ws_tx.send(tungstenite::Message::Close(None)).await;

    // Trailing newline for plain text
    if !json_output {
        println!();
    }

    // JSON output mode
    if json_output {
        let output = HeadlessOutput {
            content: content_buf,
            tool_calls,
            cost,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    Ok(())
}

/// Fallback: run headless via SSE (legacy `/api/tasks` endpoint).
async fn run_headless_sse(working_dir: PathBuf, message: String, model: Option<String>, budget: Option<f64>, json_output: bool) -> anyhow::Result<()> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(300)).build()?;

    let task_req = serde_json::json!({
        "message": message,
        "model": model,
        "budget": budget,
        "working_dir": working_dir.to_string_lossy(),
    });

    let resp = client
        .post("http://localhost:4400/api/tasks")
        .json(&task_req)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to Big Smooth: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Big Smooth returned {status}: {body}");
    }

    let mut content_buf = String::new();
    let mut tool_calls: Vec<HeadlessToolCall> = Vec::new();
    let mut cost = 0.0_f64;

    let mut stream = resp.bytes_stream();
    let mut line_buf = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let text = String::from_utf8_lossy(&chunk);

        for ch in text.chars() {
            if ch == '\n' {
                process_sse_line(&line_buf, json_output, &mut content_buf, &mut tool_calls, &mut cost);
                line_buf.clear();
            } else {
                line_buf.push(ch);
            }
        }
    }

    if !line_buf.is_empty() {
        process_sse_line(&line_buf, json_output, &mut content_buf, &mut tool_calls, &mut cost);
    }

    if !json_output {
        println!();
    }

    if json_output {
        let output = HeadlessOutput {
            content: content_buf,
            tool_calls,
            cost,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    Ok(())
}

/// Process a single SSE line, dispatching based on event type.
fn process_sse_line(line: &str, json_output: bool, content_buf: &mut String, tool_calls: &mut Vec<HeadlessToolCall>, cost: &mut f64) {
    // SSE format: "data: {...json...}"
    let data = if let Some(d) = line.strip_prefix("data: ") {
        d
    } else {
        return;
    };

    let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };

    let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "TokenDelta" => {
            if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                content_buf.push_str(content);
                if !json_output {
                    print!("{content}");
                    let _ = std::io::stdout().flush();
                }
            }
        }
        "ToolCallStart" => {
            if let Some(tool_name) = event.get("tool_name").and_then(|n| n.as_str()) {
                eprintln!("[tool] {tool_name}(...)");
            }
        }
        "ToolCallComplete" => {
            if let Some(tool_name) = event.get("tool_name").and_then(|n| n.as_str()) {
                let is_error = event.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                let status = if is_error { "error" } else { "ok" };
                eprintln!("[tool] {tool_name} -> {status}");
                tool_calls.push(HeadlessToolCall {
                    name: tool_name.to_string(),
                    success: !is_error,
                });
            }
        }
        "Error" => {
            if let Some(message) = event.get("message").and_then(|m| m.as_str()) {
                eprintln!("[error] {message}");
            }
        }
        "Completed" => {
            if let Some(iterations) = event.get("iterations").and_then(|i| i.as_u64()) {
                eprintln!("[done] completed in {iterations} iterations");
            }
            if let Some(c) = event.get("cost").and_then(|c| c.as_f64()) {
                *cost = c;
            }
        }
        "MaxIterationsReached" => {
            if let Some(max) = event.get("max").and_then(|m| m.as_u64()) {
                eprintln!("[warn] hit max iterations ({max})");
            }
        }
        "BudgetExceeded" => {
            let spent = event.get("spent_usd").and_then(|s| s.as_f64()).unwrap_or(0.0);
            let limit = event.get("limit_usd").and_then(|l| l.as_f64()).unwrap_or(0.0);
            eprintln!("[warn] budget exceeded: ${spent:.4} / ${limit:.4}");
        }
        "TaskCost" => {
            if let Some(c) = event.get("cost").and_then(|c| c.as_f64()) {
                *cost = c;
            }
        }
        _ => {}
    }
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

    #[test]
    fn json_output_format_is_valid() {
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

    #[test]
    fn process_sse_line_token_delta() {
        let mut content = String::new();
        let mut tools = Vec::new();
        let mut cost = 0.0;

        process_sse_line(r#"data: {"type":"TokenDelta","content":"hello "}"#, false, &mut content, &mut tools, &mut cost);
        process_sse_line(r#"data: {"type":"TokenDelta","content":"world"}"#, false, &mut content, &mut tools, &mut cost);

        assert_eq!(content, "hello world");
    }

    #[test]
    fn process_sse_line_tool_call() {
        let mut content = String::new();
        let mut tools = Vec::new();
        let mut cost = 0.0;

        process_sse_line(
            r#"data: {"type":"ToolCallComplete","tool_name":"write_file","is_error":false,"iteration":1}"#,
            false,
            &mut content,
            &mut tools,
            &mut cost,
        );

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "write_file");
        assert!(tools[0].success);
    }

    #[test]
    fn process_sse_line_cost() {
        let mut content = String::new();
        let mut tools = Vec::new();
        let mut cost = 0.0;

        process_sse_line(r#"data: {"type":"TaskCost","cost":0.0042}"#, false, &mut content, &mut tools, &mut cost);

        assert!((cost - 0.0042).abs() < f64::EPSILON);
    }

    #[test]
    fn process_sse_line_ignores_non_data() {
        let mut content = String::new();
        let mut tools = Vec::new();
        let mut cost = 0.0;

        process_sse_line("event: message", false, &mut content, &mut tools, &mut cost);
        process_sse_line(": comment", false, &mut content, &mut tools, &mut cost);
        process_sse_line("", false, &mut content, &mut tools, &mut cost);

        assert!(content.is_empty());
        assert!(tools.is_empty());
    }
}

//! Headless (non-interactive) mode for smooth-code.
//!
//! Uses [`BigSmoothClient`] to connect to Big Smooth over WebSocket,
//! send a `TaskStart` event, and stream `ServerEvent`s to stdout/stderr.
//! Falls back to the SSE `/api/tasks` endpoint if WebSocket connection fails.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;

use crate::client::{BigSmoothClient, ServerEvent};

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
// Headless entry point
// ---------------------------------------------------------------------------

/// Run smooth-code in headless (non-interactive) mode.
///
/// Connects to Big Smooth via [`BigSmoothClient`], sends a task, and
/// streams events to stdout/stderr.
///
/// Falls back to the legacy SSE `/api/tasks` endpoint if WebSocket fails.
///
/// # Errors
/// Returns an error if the message is empty, Big Smooth cannot be reached,
/// or the task fails.
pub async fn run_headless(
    working_dir: PathBuf,
    message: String,
    model: Option<String>,
    budget: Option<f64>,
    json_output: bool,
    agent: Option<String>,
) -> anyhow::Result<()> {
    if message.trim().is_empty() {
        anyhow::bail!("message must not be empty");
    }

    let mut client = BigSmoothClient::new("http://localhost:4400");

    match client.connect().await {
        Ok(()) => run_headless_client(client, working_dir, message, model, budget, json_output, agent).await,
        Err(e) => {
            tracing::debug!(error = %e, "BigSmoothClient connection failed, falling back to SSE");
            run_headless_sse(working_dir, message, model, budget, json_output, agent).await
        }
    }
}

/// Run smooth-code headless against a specific Big Smooth URL, returning
/// structured output instead of printing to stdout.
///
/// Intended for integration tests that spawn their own Big Smooth on an
/// ephemeral port and need to drive smooth-code's real WebSocket codepath.
/// The returned `HeadlessOutput` contains the accumulated content, every
/// tool call the agent made, and the final cost.
///
/// # Errors
/// Returns an error if Big Smooth is unreachable at `url` or the task
/// fails.
pub async fn run_headless_capture(
    url: &str,
    working_dir: PathBuf,
    message: String,
    model: Option<String>,
    budget: Option<f64>,
) -> anyhow::Result<HeadlessOutput> {
    if message.trim().is_empty() {
        anyhow::bail!("message must not be empty");
    }

    let mut client = BigSmoothClient::new(url);
    // pearl th-461ab9 (Mode B fix): bounded retry for initial WebSocket connect.
    // The bench harness was racing the launchctl-managed Big Smooth's restart
    // window and its 5s Connected-event handshake; 5 attempts × exp-backoff
    // covers the ~31s window between LaunchAgent restarts.
    client
        .connect_with_retry(5)
        .await
        .map_err(|e| anyhow::anyhow!("connect to Big Smooth at {url}: {e}"))?;

    let mut events = client
        .run_task(&message, model.as_deref(), budget, Some(&working_dir.to_string_lossy()), None)
        .await?;

    let mut content_buf = String::new();
    let mut tool_calls: Vec<HeadlessToolCall> = Vec::new();
    let mut cost = 0.0_f64;

    while let Some(event) = events.recv().await {
        match event {
            ServerEvent::TokenDelta { content, .. } => {
                content_buf.push_str(&content);
            }
            ServerEvent::ToolCallComplete { tool_name, is_error, .. } => {
                tool_calls.push(HeadlessToolCall {
                    name: tool_name,
                    success: !is_error,
                });
            }
            ServerEvent::TaskComplete { cost_usd, .. } => {
                cost = cost_usd;
                break;
            }
            ServerEvent::TaskError { message, .. } => {
                anyhow::bail!("task failed: {message}");
            }
            _ => {}
        }
    }

    Ok(HeadlessOutput {
        content: content_buf,
        tool_calls,
        cost,
    })
}

/// Run headless via [`BigSmoothClient`].
async fn run_headless_client(
    mut client: BigSmoothClient,
    working_dir: PathBuf,
    message: String,
    model: Option<String>,
    budget: Option<f64>,
    json_output: bool,
    agent: Option<String>,
) -> anyhow::Result<()> {
    let mut events = client
        .run_task(&message, model.as_deref(), budget, Some(&working_dir.to_string_lossy()), agent.as_deref())
        .await?;

    let mut content_buf = String::new();
    let mut tool_calls: Vec<HeadlessToolCall> = Vec::new();
    let mut cost = 0.0_f64;

    while let Some(event) = events.recv().await {
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
                eprintln!("[done] {iterations} iterations, ${cost_usd:.4}");
                break;
            }
            ServerEvent::TaskError { message, .. } => {
                eprintln!("[error] {message}");
                anyhow::bail!("Task failed: {message}");
            }
            ServerEvent::Error { message } => {
                eprintln!("[error] {message}");
            }
            _ => {}
        }
    }

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
async fn run_headless_sse(
    working_dir: PathBuf,
    message: String,
    model: Option<String>,
    budget: Option<f64>,
    json_output: bool,
    agent: Option<String>,
) -> anyhow::Result<()> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(300)).build()?;

    let task_req = serde_json::json!({
        "message": message,
        "model": model,
        "budget": budget,
        "working_dir": working_dir.to_string_lossy(),
        "agent": agent,
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
        let result = run_headless(dir.path().to_path_buf(), String::new(), None, None, false, None).await;
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

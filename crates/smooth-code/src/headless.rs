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
        .run_task(&message, model.as_deref(), budget, Some(&working_dir.to_string_lossy()), None, Vec::new())
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
        .run_task(
            &message,
            model.as_deref(),
            budget,
            Some(&working_dir.to_string_lossy()),
            agent.as_deref(),
            Vec::new(),
        )
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
            // Pearl th-2249cf: strip ANSI escape codes from the
            // content field. The runner-stderr block gets
            // concatenated into content with raw ESC[2m / ESC[0m
            // / etc. sequences. The TUI parses them to colors
            // (th-a14138 TUI-side); --json downstream consumers
            // (bench harness, scripts) want clean text.
            content: strip_ansi_codes(&content_buf),
            tool_calls,
            cost,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    Ok(())
}

/// Strip ANSI escape sequences from text. Pearl th-2249cf — the
/// runner forwards stderr (tracing logs colored via ANSI) into the
/// assistant content stream. In TUI mode we parse those into
/// styled spans (pearl th-a14138); in headless --json mode they
/// land in the JSON `content` field as literal `[...m`
/// strings, which is noise for downstream consumers.
///
/// Matches the standard CSI sequence (ESC `[` params m) and the
/// rarer SS3 (ESC `O` letter) and OSC (ESC `]` ... BEL/ST). Pure
/// function so the unit suite can pin every variant.
fn strip_ansi_codes(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // CSI: ESC [ params <letter>
        if b == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';' || bytes[j] == b'?') {
                j += 1;
            }
            if j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                i = j + 1;
                continue;
            }
        }
        // OSC: ESC ] ... BEL or ESC \
        if b == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b']' {
            let mut j = i + 2;
            while j < bytes.len() {
                if bytes[j] == 0x07 {
                    j += 1;
                    break;
                }
                if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                    j += 2;
                    break;
                }
                j += 1;
            }
            i = j;
            continue;
        }
        // Bare-bracket SGR: `[<digits>(;<digits>)*m` — the
        // ESC-eaten variant. Only match when the bracket is
        // immediately followed by digits (with optional `;` separators)
        // and ends with `m`, so `[docs.rs]` and `vec![1, 2]` survive.
        if b == b'[' {
            let mut j = i + 1;
            let mut saw_digit = false;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                if bytes[j].is_ascii_digit() {
                    saw_digit = true;
                }
                j += 1;
            }
            if saw_digit && j < bytes.len() && bytes[j] == b'm' {
                i = j + 1;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8(out).expect("strip_ansi_codes preserves UTF-8 because it only skips ASCII control sequences")
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

    #[test]
    fn strip_ansi_removes_csi_sgr() {
        // Standard CSI SGR (ESC [...m) — the most common shape
        // tracing/eyre/etc. emit for colored terminal output.
        let raw = "\x1b[2m2026-05-10T16:11:20Z\x1b[0m \x1b[32m INFO\x1b[0m starting";
        let clean = strip_ansi_codes(raw);
        assert_eq!(clean, "2026-05-10T16:11:20Z  INFO starting");
    }

    #[test]
    fn strip_ansi_removes_bare_bracket_m_form() {
        // Pearl th-2249cf: when the runner forwards stderr through
        // a multi-stage transform, the leading ESC byte sometimes
        // gets eaten and we see literal `[2m...[0m` strings. Still
        // noise for downstream consumers; strip them too.
        let raw = "[2m2026-05-10T16:11:20Z[0m [32m INFO[0m hello";
        let clean = strip_ansi_codes(raw);
        assert_eq!(clean, "2026-05-10T16:11:20Z  INFO hello");
    }

    #[test]
    fn strip_ansi_preserves_normal_text() {
        let raw = "Plain text with no escape sequences.";
        assert_eq!(strip_ansi_codes(raw), raw);
    }

    #[test]
    fn strip_ansi_handles_realworld_runner_stderr() {
        // Excerpt from /tmp/smooth-bench-run/repo-overview/run-2.txt
        // where the runner-stderr block lands in --json content.
        let raw = "\x1b[2m2026-05-10T16:17:58.369275Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2msmooth_operative\x1b[0m\x1b[2m:\x1b[0m smooth-operative starting";
        let clean = strip_ansi_codes(raw);
        // No more ESC sequences anywhere.
        assert!(!clean.contains('\x1b'), "ESC byte still present: {clean:?}");
        assert!(clean.contains("smooth-operative starting"));
    }

    #[test]
    fn strip_ansi_does_not_overmatch_brackets() {
        // Square brackets that aren't ANSI codes (markdown links,
        // code paths, etc.) must survive.
        let raw = "see [docs.rs](url) and `vec![1, 2]` and `fn foo[T]()`";
        let clean = strip_ansi_codes(raw);
        assert_eq!(clean, raw);
    }
}

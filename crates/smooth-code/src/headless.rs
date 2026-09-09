//! Headless (non-interactive) mode for smooth-code.
//!
//! Uses [`BigSmoothClient`] to connect to Big Smooth over WebSocket,
//! send a `TaskStart` event, and stream `ServerEvent`s to stdout/stderr.
//! Falls back to the SSE `/api/tasks` endpoint if WebSocket connection fails.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context as _;
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

    let url = daemon_url();
    let mut client = BigSmoothClient::new(&url);

    match client.connect().await {
        Ok(()) => run_headless_client(client, working_dir, message, model, budget, json_output, agent).await,
        Err(e) => {
            tracing::debug!(error = %e, url, "BigSmoothClient connection failed, falling back to SSE");
            run_headless_sse(&url, working_dir, message, model, budget, json_output, agent)
                .await
                .with_context(|| format!("Big Smooth at {url}: WebSocket connect failed ({e})"))
        }
    }
}

/// Where Big Smooth is: `$SMOOTH_URL`, else the daemon advertised in
/// `~/.smooth/daemon.addr`, else `http://localhost:4400`.
///
/// th-9d4b09: the headless path (and `th run` on top of it) hard-wired the
/// last one, so a daemon on any other port — the Big Smooth app's, or `th up
/// --port` — was never even tried.
#[must_use]
pub fn daemon_url() -> String {
    daemon_url_from(
        std::env::var("SMOOTH_URL").ok(),
        dirs_next::home_dir().map(|h| h.join(".smooth").join("daemon.addr")).as_deref(),
    )
}

fn daemon_url_from(env: Option<String>, addr_file: Option<&std::path::Path>) -> String {
    if let Some(u) = env.map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| !u.is_empty()) {
        return u;
    }
    if let Some(addr) = addr_file
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return if addr.contains("://") { addr } else { format!("http://{addr}") };
    }
    "http://localhost:4400".to_string()
}

/// A canonical `error` frame that names the in-flight turn (the client's
/// read loop keeps `request_id` only for that one — th-472012) ends the
/// turn: no `TaskComplete` will follow. th-9d4b09: `th run` against a daemon
/// with no LLM key printed `LLM_UNAVAILABLE` and then waited forever for a
/// completion. Unattributed errors are chatter and keep the turn alive.
fn turn_error(names_this_turn: bool, message: &str) -> Option<anyhow::Error> {
    names_this_turn.then(|| anyhow::anyhow!("Task failed: {message}"))
}

/// Whether an `/api/tasks` reply is the SSE stream the fallback expects.
/// The daemon's SPA fallback answers *any* unknown path with `200 text/html`
/// (index.html) — reading that as an empty event stream is exactly how `th
/// run` "succeeded" without dispatching anything (th-9d4b09).
fn is_event_stream(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|ct| ct.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("text/event-stream"))
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
        .run_task(
            &message,
            model.as_deref(),
            budget,
            Some(&working_dir.to_string_lossy()),
            None,
            Vec::new(),
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
            }
            ServerEvent::ToolCallComplete { tool_name, is_error, .. } => {
                tool_calls.push(HeadlessToolCall {
                    name: tool_name,
                    success: !is_error,
                });
            }
            ServerEvent::TaskComplete { usage, .. } => {
                cost = usage.map_or(0.0, |u| u.cost_usd);
                break;
            }
            ServerEvent::TaskError { message, .. } => {
                anyhow::bail!("task failed: {message}");
            }
            ServerEvent::Error { message, request_id } => {
                if let Some(fatal) = turn_error(request_id.is_some(), &message) {
                    return Err(fatal);
                }
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
            ServerEvent::TaskComplete { iterations, usage, .. } => {
                // Report only what the server told us: `usage` is absent on
                // turns the engine didn't account for, and the canonical
                // protocol carries no iteration count at all (th-d49538).
                cost = usage.map_or(0.0, |u| u.cost_usd);
                let tally = usage.map_or_else(|| " (no usage reported)".to_string(), |u| format!(" ${:.4}", u.cost_usd));
                let iters = if iterations == 0 {
                    String::new()
                } else {
                    format!(" {iterations} iterations,")
                };
                eprintln!("[done]{iters}{tally}");
                break;
            }
            ServerEvent::TaskError { message, .. } => {
                eprintln!("[error] {message}");
                anyhow::bail!("Task failed: {message}");
            }
            ServerEvent::Error { message, request_id } => {
                eprintln!("[error] {message}");
                if let Some(fatal) = turn_error(request_id.is_some(), &message) {
                    return Err(fatal);
                }
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

/// Fallback: run headless via SSE (legacy `/api/tasks` endpoint). Loud when
/// the server has no such endpoint, and when the stream ends without a result.
async fn run_headless_sse(
    url: &str,
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
        .post(format!("{url}/api/tasks"))
        .json(&task_req)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to Big Smooth at {url}: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Big Smooth returned {status}: {body}");
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if !is_event_stream(content_type.as_deref()) {
        anyhow::bail!(
            "Big Smooth at {url} has no /api/tasks (got {}) — nothing was dispatched",
            content_type.as_deref().unwrap_or("no content-type")
        );
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
    if content_buf.is_empty() && tool_calls.is_empty() {
        anyhow::bail!("Big Smooth at {url} closed the stream without a result — nothing was dispatched");
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

    /// th-9d4b09: discovery order — `$SMOOTH_URL`, then `daemon.addr`, then :4400.
    #[test]
    fn daemon_url_prefers_env_then_advertised_addr_then_default() {
        let dir = tempfile::tempdir().unwrap();
        let addr = dir.path().join("daemon.addr");
        assert_eq!(daemon_url_from(None, Some(&addr)), "http://localhost:4400", "no env, no file");
        std::fs::write(&addr, "127.0.0.1:8899\n").unwrap();
        assert_eq!(
            daemon_url_from(None, Some(&addr)),
            "http://127.0.0.1:8899",
            "the advertised daemon wins over the default"
        );
        assert_eq!(
            daemon_url_from(Some("http://10.0.0.2:4400/".into()), Some(&addr)),
            "http://10.0.0.2:4400",
            "env wins"
        );
        assert_eq!(daemon_url_from(Some("  ".into()), Some(&addr)), "http://127.0.0.1:8899", "blank env is unset");
        std::fs::write(&addr, "").unwrap();
        assert_eq!(daemon_url_from(None, Some(&addr)), "http://localhost:4400", "empty file is unset");
        assert_eq!(daemon_url_from(None, None), "http://localhost:4400");
    }

    /// th-9d4b09: a turn-scoped `error` frame ends the turn (no completion
    /// follows); an unattributed one is chatter.
    #[test]
    fn a_turn_scoped_error_is_fatal_and_chatter_is_not() {
        let fatal = turn_error(true, "LLM_UNAVAILABLE: no gateway key").unwrap();
        assert!(fatal.to_string().contains("LLM_UNAVAILABLE"));
        assert!(turn_error(false, "late error for an abandoned turn").is_none());
    }

    /// th-9d4b09: the SPA fallback's `200 text/html` must not pass for a stream.
    #[test]
    fn only_a_real_event_stream_counts() {
        assert!(is_event_stream(Some("text/event-stream")));
        assert!(is_event_stream(Some("Text/Event-Stream; charset=utf-8")));
        assert!(!is_event_stream(Some("text/html; charset=utf-8")));
        assert!(!is_event_stream(Some("application/json")));
        assert!(!is_event_stream(None));
    }

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

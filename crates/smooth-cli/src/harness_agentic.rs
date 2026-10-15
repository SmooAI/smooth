//! `th harness add --agentic <name>` (pearl th-473294): ask Big Smooth to add
//! a coding-agent CLI to SmoothFlow by itself.
//!
//! Two halves:
//!
//! 1. **The provider gate.** `GET /api/llm/provider` on the running daemon.
//!    No model ⇒ the shared first-run picker (`onboard_provider`: the Family
//!    plan or your own key). The daemon reads its gateway at boot, so after
//!    saving one the honest answer is "restart Big Smooth and rerun", which
//!    is what this prints.
//! 2. **The turn.** One canonical-protocol turn over the daemon's `/ws`
//!    (`create_conversation_session` → `send_message` → stream →
//!    `eventual_response`), asking the agent to call its `add_harness` tool
//!    with the exact arguments. Progress (tool calls) goes to stderr, the
//!    agent's reply to stdout — pipe-safe, Presence-styled.

use std::time::Duration;

use anstream::{eprintln, println};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use owo_colors::OwoColorize;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use crate::gradient::paint;

/// A long tool call: each validation launches the CLI and waits for turns.
#[allow(clippy::duration_suboptimal_units, reason = "seconds match the rest of this crate")]
const FRAME_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The arguments `th harness add --agentic` forwards to the tool.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgenticArgs {
    pub name: String,
    pub binary: Option<String>,
    pub docs: Option<String>,
    pub iterations: u8,
    pub force: bool,
    pub install_unverified: bool,
    pub model: Option<String>,
}

impl AgenticArgs {
    /// The tool's argument object, exactly as the agent should pass it.
    #[must_use]
    pub fn tool_args(&self) -> Value {
        let mut v = json!({ "name": self.name, "max_iterations": self.iterations, "force": self.force, "install_unverified": self.install_unverified });
        if let Some(b) = &self.binary {
            v["binary_hint"] = json!(b);
        }
        if let Some(d) = &self.docs {
            v["docs_url"] = json!(d);
        }
        if let Some(m) = &self.model {
            v["model"] = json!(m);
        }
        v
    }
}

/// The message that drives the turn: explicit tool + explicit args, so the
/// agent's only judgement is relaying the report.
#[must_use]
pub fn turn_message(args: &AgenticArgs) -> String {
    format!(
        "Add the coding-agent CLI `{}` to SmoothFlow. Call your `add_harness` tool exactly once with these arguments and nothing else:\n{}\nIt can take several minutes — wait for it. Then report, in plain text: the manifest path (or that it was not installed), what was proven, what could not be proven and why, and the next steps from the report. Do not paraphrase the manifest; do not call any other tool.",
        args.name,
        serde_json::to_string_pretty(&args.tool_args()).unwrap_or_default()
    )
}

/// Run the gate: returns `Ok(true)` when the daemon can draft right now,
/// `Ok(false)` when the user must restart the daemon (or cancelled) — the
/// caller prints nothing more in that case. The picker itself is the shared
/// first-run flow in [`crate::onboard_provider`].
///
/// # Errors
/// When the daemon is unreachable or a provider step fails.
pub async fn ensure_provider() -> Result<bool> {
    use crate::onboard_provider::{gate_from_status, run_picker, Gate};
    let status = crate::flow::call(reqwest::Method::GET, "/api/llm/provider", None).await?;
    match gate_from_status(&status) {
        Gate::Ready => {
            let model = status.get("model").and_then(Value::as_str).unwrap_or("?");
            let host = status.get("gateway_host").and_then(Value::as_str).unwrap_or("?");
            eprintln!("{} provider: {model} via {host}", paint("●", |g| g.bold().to_string()));
            return Ok(true);
        }
        Gate::RestartRequired => {
            print_restart();
            return Ok(false);
        }
        Gate::NeedsSetup => {}
    }
    eprintln!(
        "{} Big Smooth has no LLM provider — drafting a manifest needs a model.",
        paint("○", |g| g.dimmed().to_string())
    );
    if run_picker().await?.is_none() {
        return Ok(false);
    }
    // Saved. The daemon booted without it — say so plainly.
    let status = crate::flow::call(reqwest::Method::GET, "/api/llm/provider", None).await?;
    match gate_from_status(&status) {
        Gate::Ready => Ok(true),
        Gate::RestartRequired => {
            print_restart();
            Ok(false)
        }
        Gate::NeedsSetup => bail!("the provider was saved but the daemon still reports none — check ~/.smooth/providers.json (th model status)"),
    }
}

fn print_restart() {
    eprintln!(
        "{} provider saved, but Big Smooth read its gateway at boot — restart it, then rerun:\n  th down && th up\n  th harness add --agentic <name>",
        paint("◐", |g| g.bold().to_string())
    );
}

// ── the turn ─────────────────────────────────────────────────────────────────

/// What one inbound frame means for the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A streamed token of the reply (stdout).
    Token(String),
    /// A tool call started (stderr).
    ToolStart(String),
    /// A tool call finished (stderr); `is_error` paints it amber.
    ToolDone { tool: String, is_error: bool },
    /// The turn is over; the reply text.
    Done(String),
    /// The server reported an error.
    Error(String),
}

/// Translate a canonical frame (`smooth-operator-server` protocol) — the same
/// mapping `th code` uses (`stream_token` / `stream_chunk.rawResponse` /
/// `eventual_response`).
#[must_use]
pub fn translate(v: &Value) -> Option<Line> {
    match v.get("type").and_then(Value::as_str)? {
        "stream_token" => Some(Line::Token(v.get("token").and_then(Value::as_str).unwrap_or_default().to_string())),
        "stream_chunk" => {
            let raw = v.pointer("/data/state/rawResponse")?;
            if let Some(call) = raw.get("toolCall") {
                return Some(Line::ToolStart(call.get("name").and_then(Value::as_str).unwrap_or("?").to_string()));
            }
            let res = raw.get("toolResult")?;
            Some(Line::ToolDone {
                tool: res.get("name").and_then(Value::as_str).unwrap_or("?").to_string(),
                is_error: res.get("isError").and_then(Value::as_bool).unwrap_or(false),
            })
        }
        "eventual_response" => Some(Line::Done(extract_reply(v.pointer("/data/data/response")))),
        "error" => Some(Line::Error(smooth_cast::wire::error_message(v))),
        _ => None,
    }
}

fn extract_reply(response: Option<&Value>) -> String {
    let Some(resp) = response else { return String::new() };
    if let Some(parts) = resp.get("responseParts").and_then(Value::as_array) {
        let text: String = parts
            .iter()
            .filter_map(|p| {
                p.as_str()
                    .or_else(|| p.get("text").and_then(Value::as_str))
                    .or_else(|| p.get("content").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("");
        if !text.trim().is_empty() {
            return text.trim().to_string();
        }
    }
    resp.as_str().map_or_else(
        || resp.get("text").and_then(Value::as_str).map_or_else(|| resp.to_string(), str::to_string),
        str::to_string,
    )
}

fn ws_url() -> Result<String> {
    let token = crate::flow::local_token()
        .map(|t| format!("?token={}", urlencoding::encode(&t)))
        .unwrap_or_default();
    Ok(format!("ws://{}/ws{token}", crate::flow::daemon_addr()?))
}

/// Drive the turn and render it. Returns the agent's final reply.
///
/// # Errors
/// When the daemon's WS is unreachable, the turn errors, or times out.
pub async fn run_turn(args: &AgenticArgs) -> Result<String> {
    let (ws, _) = tokio_tungstenite::connect_async(ws_url()?)
        .await
        .context("Big Smooth WS connect failed\n  → is Big Smooth running? (th up); the token comes from SMOOTH_LOCAL_TOKEN or ~/.smooth/operator-token")?;
    let (mut sink, mut source) = ws.split();
    let rid = |n: u32| format!("th-harness-{}-{n}", std::process::id());
    // Same opening move as `th code` / the web SPA: the local flavor wants an
    // `agentId` (any uuid) and a display name on the session.
    sink.send(Message::Text(
        json!({ "action": "create_conversation_session", "requestId": rid(1), "agentId": uuid::Uuid::new_v4().to_string(), "userName": "th harness" })
            .to_string()
            .into(),
    ))
    .await?;
    let mut session_id: Option<String> = None;
    while session_id.is_none() {
        let frame = tokio::time::timeout(Duration::from_secs(30), source.next())
            .await
            .context("timed out waiting for Big Smooth to open a conversation")?
            .ok_or_else(|| anyhow!("Big Smooth closed the connection"))??;
        if let Message::Text(t) = frame {
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            if v.get("type").and_then(Value::as_str) == Some("error") {
                bail!("Big Smooth: {}", smooth_cast::wire::error_message(&v));
            }
            if let Some(s) = v.pointer("/data/sessionId").and_then(Value::as_str) {
                session_id = Some(s.to_string());
            }
        }
    }
    let session_id = session_id.unwrap_or_default();
    sink.send(Message::Text(
        json!({ "action": "send_message", "requestId": rid(2), "sessionId": session_id, "message": turn_message(args), "stream": true })
            .to_string()
            .into(),
    ))
    .await?;
    eprintln!(
        "{} asked Big Smooth to add `{}` — this launches the CLI for real, so it takes minutes",
        paint("●", |g| g.bold().to_string()),
        args.name
    );
    let mut streamed = false;
    loop {
        let frame = tokio::time::timeout(FRAME_TIMEOUT, source.next())
            .await
            .context("timed out waiting for Big Smooth (20 min) — the daemon log has the tool's progress")?
            .ok_or_else(|| anyhow!("Big Smooth closed the connection mid-turn"))??;
        let Message::Text(t) = frame else { continue };
        let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
        match translate(&v) {
            Some(Line::Token(tok)) => {
                streamed = true;
                print!("{tok}");
                let _ = std::io::Write::flush(&mut anstream::stdout());
            }
            Some(Line::ToolStart(tool)) => eprintln!("{} {tool}", paint("⚙", |g| g.dimmed().to_string())),
            Some(Line::ToolDone { tool, is_error }) => {
                if is_error {
                    eprintln!("{} {tool} failed", paint("○", |g| g.yellow().to_string()));
                } else {
                    eprintln!("{} {tool} done", paint("●", |g| g.dimmed().to_string()));
                }
            }
            Some(Line::Done(reply)) => {
                if !streamed && !reply.is_empty() {
                    println!("{reply}");
                } else if streamed {
                    println!();
                }
                let _ = sink.close().await;
                return Ok(reply);
            }
            Some(Line::Error(e)) => bail!("Big Smooth: {e}"),
            None => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn tool_args_forward_only_what_was_given() {
        let a = AgenticArgs {
            name: "gemini".into(),
            iterations: 2,
            ..AgenticArgs::default()
        };
        let v = a.tool_args();
        assert_eq!(v["name"], "gemini");
        assert_eq!(v["max_iterations"], 2);
        assert_eq!(v["force"], false);
        assert!(v.get("binary_hint").is_none());
        assert!(v.get("docs_url").is_none());
        let a = AgenticArgs {
            binary: Some("gem".into()),
            docs: Some("https://d".into()),
            model: Some("m".into()),
            force: true,
            ..a
        };
        let v = a.tool_args();
        assert_eq!(v["binary_hint"], "gem");
        assert_eq!(v["docs_url"], "https://d");
        assert_eq!(v["model"], "m");
        assert_eq!(v["force"], true);
    }

    #[test]
    fn turn_message_names_the_tool_and_embeds_the_args() {
        let m = turn_message(&AgenticArgs {
            name: "aider".into(),
            iterations: 3,
            ..AgenticArgs::default()
        });
        assert!(m.contains("`add_harness` tool exactly once"));
        assert!(m.contains("\"name\": \"aider\""));
        assert!(m.contains("do not call any other tool"));
    }

    #[test]
    fn translate_maps_the_canonical_frames() {
        assert_eq!(translate(&json!({"type": "stream_token", "token": "hi"})), Some(Line::Token("hi".into())));
        assert_eq!(
            translate(&json!({"type": "stream_chunk", "data": {"state": {"rawResponse": {"toolCall": {"name": "add_harness"}}}}})),
            Some(Line::ToolStart("add_harness".into()))
        );
        assert_eq!(
            translate(&json!({"type": "stream_chunk", "data": {"state": {"rawResponse": {"toolResult": {"name": "add_harness", "isError": true}}}}})),
            Some(Line::ToolDone {
                tool: "add_harness".into(),
                is_error: true
            })
        );
        assert_eq!(
            translate(&json!({"type": "eventual_response", "data": {"data": {"response": {"responseParts": ["done ", {"text": "ok"}]}}}})),
            Some(Line::Done("done ok".into()))
        );
        assert!(matches!(translate(&json!({"type": "error", "error": {"message": "boom"}})), Some(Line::Error(m)) if m.contains("boom")));
        assert_eq!(translate(&json!({"type": "stream_reasoning", "token": "x"})), None, "reasoning never renders");
        assert_eq!(translate(&json!({"nope": 1})), None);
    }
}

//! Drive a benchmark task through the canonical smooth-operator
//! `LocalServer` WebSocket protocol — the schema-driven flow every
//! engine (rust/go/ts/python/dotnet) speaks.
//!
//! This replaces the retired `chat_driver` (which assumed the deleted
//! microVM "create a pearl + dispatch a teammate" model) and the even
//! older bespoke `/ws` handshake in `smooth_code::client`. The flow is
//! a straight copy of the daemon's own `OperatorTurnDriver::drive_once`
//! (`crates/smooth-daemon/src/scheduler.rs`):
//!
//!   1. connect `ws://host:port/ws?token=<url-encoded>` (token omitted
//!      for the anonymous polyglot servers; required for the rust daemon).
//!   2. send `create_conversation_session` → read until an
//!      `immediate_response` carrying `data.sessionId`.
//!   3. send `send_message` with the task prompt.
//!   4. drain events until `eventual_response` (turn complete) or `error`,
//!      collecting tool-call outcomes from `stream_chunk` events along the
//!      way.
//!
//! Cost: the polyglot servers do not surface per-turn LLM cost/usage in
//! their protocol events, so `cost` is best-effort — we scan the terminal
//! `eventual_response` for a `cost`/`costUsd`/`totalCost` number and
//! otherwise leave it `0.0`. The bench's headline number is pass-rate;
//! cost is informational and will read `$0` for engines that don't emit it.

use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// What one canonical turn surfaced back to the bench.
#[derive(Debug, Clone, Default)]
pub struct CanonicalOutput {
    /// Best-effort LLM spend for the turn. `0.0` when the engine doesn't
    /// emit cost (all polyglot servers today).
    pub cost: f64,
    /// One record per tool *result* observed on the stream (success is
    /// the negation of the engine's `isError` flag).
    pub tool_calls: Vec<CanonicalToolCall>,
    /// The assistant's spoken answer, reassembled from `stream_token`
    /// events. Empty for engines that don't stream tokens. The agentic
    /// bench feeds this to the LLM judge as evidence; the polyglot bench
    /// ignores it (it scores the workspace, not the prose).
    pub text: String,
}

/// A single tool result observed during the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalToolCall {
    pub name: String,
    pub success: bool,
}

/// Run one canonical turn end-to-end against `url`, bounded by `deadline`.
///
/// # Errors
/// Errors on WS connect failure, a server `error` event, or if the turn
/// doesn't complete within `deadline`.
pub async fn run_via_canonical(url: &str, prompt: &str, token: Option<&str>, deadline: Duration) -> Result<CanonicalOutput> {
    tokio::time::timeout(deadline, drive_once(url, prompt, token))
        .await
        .map_err(|_| anyhow::anyhow!("canonical turn timed out after {deadline:?}"))?
}

/// One connect → create-session → send → drain cycle.
async fn drive_once(url: &str, prompt: &str, token: Option<&str>) -> Result<CanonicalOutput> {
    let ws_url = ws_url(url, token);
    let (stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("operator WS connect failed ({ws_url}): {e}"))?;
    let (mut sink, mut source) = stream.split();

    // 1. Open a session.
    sink.send(Message::Text(create_session_msg(&uuid::Uuid::new_v4().to_string()).into())).await?;
    let mut session_id = None;
    while let Some(Ok(msg)) = source.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        match classify(&v) {
            Event::SessionCreated(sid) => {
                session_id = Some(sid);
                break;
            }
            Event::Error(m) => anyhow::bail!("operator error creating session: {m}"),
            _ => {}
        }
    }
    let sid = session_id.ok_or_else(|| anyhow::anyhow!("operator closed before a session was created"))?;

    // 2. Fire the task prompt.
    sink.send(Message::Text(send_message_msg(&sid, prompt).into())).await?;

    // 3. Drain until the turn completes (or errors), collecting tool
    //    results + a best-effort cost off the terminal event.
    let mut out = CanonicalOutput::default();
    while let Some(Ok(msg)) = source.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        match classify(&v) {
            Event::TurnComplete => {
                if let Some(c) = find_cost(&v) {
                    out.cost = c;
                }
                break;
            }
            Event::Error(m) => anyhow::bail!("operator error on turn: {m}"),
            Event::ToolResult { name, success } => out.tool_calls.push(CanonicalToolCall { name, success }),
            Event::Token(t) => out.text.push_str(&t),
            _ => {}
        }
    }
    let _ = sink.send(Message::Close(None)).await;
    Ok(out)
}

/// Classification of one inbound server event.
enum Event {
    /// `immediate_response` carrying a `data.sessionId`.
    SessionCreated(String),
    /// `eventual_response` — the turn is done.
    TurnComplete,
    /// `error` — carries the human-readable message.
    Error(String),
    /// A `stream_chunk` carrying a tool result.
    ToolResult { name: String, success: bool },
    /// One `stream_token` of the assistant's spoken answer.
    Token(String),
    /// Anything else (pongs, reasoning tokens, tool-call chunks, acks).
    Other,
}

/// Classify a parsed inbound frame. Pure — the whole reason the driver is
/// unit-testable without a live server.
fn classify(v: &Value) -> Event {
    match v.get("type").and_then(Value::as_str) {
        Some("immediate_response") => match v.pointer("/data/sessionId").and_then(Value::as_str) {
            Some(sid) => Event::SessionCreated(sid.to_string()),
            None => Event::Other, // send_message processing ack — no session id
        },
        Some("eventual_response") => Event::TurnComplete,
        Some("error") => Event::Error(smooth_cast::wire::error_message(v)),
        // The answer streams as top-level `token` (see the web client's
        // `operator.ts`); a couple of engines nest it under `data`.
        // `stream_reasoning` deliberately falls through to `Other` —
        // thinking is not the answer.
        Some("stream_token") => match v
            .get("token")
            .and_then(Value::as_str)
            .or_else(|| v.pointer("/data/token").and_then(Value::as_str))
        {
            Some(t) => Event::Token(t.to_string()),
            None => Event::Other,
        },
        Some("stream_chunk") => match v.pointer("/data/state/rawResponse/toolResult") {
            Some(tr) => Event::ToolResult {
                name: tr.get("name").and_then(Value::as_str).unwrap_or("unknown").to_string(),
                success: !tr.get("isError").and_then(Value::as_bool).unwrap_or(false),
            },
            None => Event::Other,
        },
        _ => Event::Other,
    }
}

/// Build the `create_conversation_session` frame.
fn create_session_msg(agent_id: &str) -> String {
    json!({
        "action": "create_conversation_session",
        "requestId": "bench-cs",
        "agentId": agent_id,
        "userName": "bench",
    })
    .to_string()
}

/// Build the `send_message` frame.
fn send_message_msg(session_id: &str, prompt: &str) -> String {
    json!({
        "action": "send_message",
        "requestId": "bench-turn",
        "sessionId": session_id,
        "message": prompt,
    })
    .to_string()
}

/// Turn a base HTTP(S) URL into the operator `/ws` URL, appending a
/// url-encoded `?token=` only when a token is supplied.
fn ws_url(base: &str, token: Option<&str>) -> String {
    let ws = base.replace("https://", "wss://").replace("http://", "ws://");
    let ws = ws.trim_end_matches('/');
    match token {
        Some(t) => format!("{ws}/ws?token={}", urlencode(t)),
        None => format!("{ws}/ws"),
    }
}

/// Best-effort recursive scan for a cost number under a `cost` / `costUsd`
/// / `totalCost` key anywhere in the terminal event. Returns the first hit.
fn find_cost(v: &Value) -> Option<f64> {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if matches!(k.as_str(), "cost" | "costUsd" | "cost_usd" | "totalCost" | "total_cost") {
                    if let Some(n) = val.as_f64() {
                        return Some(n);
                    }
                }
                if let Some(found) = find_cost(val) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => arr.iter().find_map(find_cost),
        _ => None,
    }
}

/// Percent-encode a token for a `?token=` query param (RFC 3986
/// unreserved set passes through; everything else is `%XX`).
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn ws_url_omits_token_when_none_and_strips_trailing_slash() {
        assert_eq!(ws_url("http://127.0.0.1:8792", None), "ws://127.0.0.1:8792/ws");
        assert_eq!(ws_url("http://127.0.0.1:8792/", None), "ws://127.0.0.1:8792/ws");
        assert_eq!(ws_url("https://host", None), "wss://host/ws");
    }

    #[test]
    fn ws_url_appends_url_encoded_token() {
        assert_eq!(ws_url("http://h:1", Some("abc123")), "ws://h:1/ws?token=abc123");
        // Non-unreserved bytes get escaped.
        assert_eq!(ws_url("http://h:1", Some("a b/c")), "ws://h:1/ws?token=a%20b%2Fc");
    }

    #[test]
    fn create_session_msg_shape() {
        let v: Value = serde_json::from_str(&create_session_msg("agent-uuid")).unwrap();
        assert_eq!(v["action"], "create_conversation_session");
        assert_eq!(v["agentId"], "agent-uuid");
        assert_eq!(v["userName"], "bench");
        assert_eq!(v["requestId"], "bench-cs");
    }

    #[test]
    fn send_message_msg_shape() {
        let v: Value = serde_json::from_str(&send_message_msg("sess-1", "do the thing")).unwrap();
        assert_eq!(v["action"], "send_message");
        assert_eq!(v["sessionId"], "sess-1");
        assert_eq!(v["message"], "do the thing");
        assert_eq!(v["requestId"], "bench-turn");
    }

    #[test]
    fn classify_session_created_from_immediate_response() {
        let v = json!({"type":"immediate_response","data":{"sessionId":"s-9","conversationId":"c-9"}});
        assert!(matches!(classify(&v), Event::SessionCreated(sid) if sid == "s-9"));
    }

    #[test]
    fn classify_immediate_response_ack_without_session_is_other() {
        // The send_message processing ack is an immediate_response with no sessionId.
        let v = json!({"type":"immediate_response","data":{"status":200}});
        assert!(matches!(classify(&v), Event::Other));
    }

    #[test]
    fn classify_turn_complete_from_eventual_response() {
        let v = json!({"type":"eventual_response","data":{"messageId":"m1"}});
        assert!(matches!(classify(&v), Event::TurnComplete));
    }

    /// The shape `smooth-operator-server`'s `protocol::error` actually emits:
    /// `error` is an OBJECT, at the top level and nested under `data`.
    ///
    /// This test used to assert `{"type":"error","data":{"code","message"}}`,
    /// a shape the server never sends — so it passed while the driver reported
    /// every real error as "unknown operator error" (pearl th-472012).
    #[test]
    fn classify_error_reads_the_real_server_frame() {
        let v = json!({
            "type": "error",
            "error": { "code": "VALIDATION_ERROR", "message": "missing 'action' field" },
            "data": { "error": { "code": "VALIDATION_ERROR", "message": "missing 'action' field" } },
        });
        assert!(matches!(classify(&v), Event::Error(m) if m == "VALIDATION_ERROR: missing 'action' field"));
    }

    #[test]
    fn classify_error_still_reads_the_legacy_data_message_shape() {
        let v = json!({"type":"error","data":{"message":"it broke"}});
        assert!(matches!(classify(&v), Event::Error(m) if m == "it broke"));
    }

    #[test]
    fn classify_tool_result_success_and_failure() {
        let ok = json!({"type":"stream_chunk","data":{"state":{"rawResponse":{"toolResult":{"name":"write_file","isError":false,"result":"ok"}}}}});
        assert!(matches!(classify(&ok), Event::ToolResult { name, success } if name == "write_file" && success));

        let bad = json!({"type":"stream_chunk","data":{"state":{"rawResponse":{"toolResult":{"name":"run_bash","isError":true,"result":"Error: boom"}}}}});
        assert!(matches!(classify(&bad), Event::ToolResult { name, success } if name == "run_bash" && !success));
    }

    #[test]
    fn classify_tool_call_chunk_is_other() {
        // A toolCall chunk (no toolResult) shouldn't be counted as a result.
        let v = json!({"type":"stream_chunk","data":{"state":{"rawResponse":{"toolCall":{"name":"read_file","arguments":{}}}}}});
        assert!(matches!(classify(&v), Event::Other));
    }

    #[test]
    fn classify_stream_token_yields_the_answer_text() {
        // Top-level `token` is the shape the daemon emits.
        let v = json!({"type":"stream_token","token":"hello"});
        assert!(matches!(classify(&v), Event::Token(t) if t == "hello"));
        // Nested under `data` also accepted.
        let nested = json!({"type":"stream_token","data":{"token":" world"}});
        assert!(matches!(classify(&nested), Event::Token(t) if t == " world"));
        // A token-less frame is not an empty answer, it's noise.
        assert!(matches!(classify(&json!({"type":"stream_token"})), Event::Other));
    }

    #[test]
    fn classify_stream_reasoning_is_not_the_answer() {
        // Thinking rides its own channel and must never land in `text`.
        let v = json!({"type":"stream_reasoning","token":"hmm let me think"});
        assert!(matches!(classify(&v), Event::Other));
    }

    #[test]
    fn find_cost_scans_nested_keys() {
        let v = json!({"type":"eventual_response","data":{"response":{"usage":{"costUsd":0.0123}}}});
        assert_eq!(find_cost(&v), Some(0.0123));
        // Absent → None (the common case for polyglot servers).
        let none = json!({"type":"eventual_response","data":{"messageId":"m"}});
        assert_eq!(find_cost(&none), None);
    }
}

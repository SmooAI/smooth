//! `OperatorClient` — speaks smooth-operator's **canonical** WS protocol (the one
//! the official widget + SDK clients use), so `th code` talks to
//! `th daemon operator` (`:8787`) instead of the legacy bespoke `/ws` (`:4400`).
//!
//! It is a drop-in for the TUI/headless layers: [`run_task`](OperatorClient::run_task)
//! returns a stream of the same [`ServerEvent`](crate::client::ServerEvent)s the
//! old client produced, so the rendering loops in `app.rs` / `headless.rs` stay
//! unchanged. The translation operator→bespoke lives in [`map_event`].
//!
//! Protocol shape:
//! - connect `ws://host/ws?token=…`, then `create_conversation_session` →
//!   `immediate_response { data.sessionId }` (one persistent session per client,
//!   so multi-turn history is server-side — no `prior_messages` replay needed).
//! - per turn: `send_message { requestId, sessionId, message }` → a stream of
//!   `stream_token` / `stream_chunk` (tool calls) → terminal `eventual_response`
//!   (or `error`). Events are routed back to the originating turn by `requestId`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;

use crate::client::ServerEvent;

/// Translate one operator-protocol event into the bespoke [`ServerEvent`] the TUI
/// already renders. Returns `None` for events the TUI doesn't consume (session
/// keepalives, non-tool node progress). **Pure** — unit-tested without a server.
#[must_use]
pub fn map_event(v: &Value) -> Option<ServerEvent> {
    let ty = v.get("type")?.as_str()?;
    let task_id = v.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
    match ty {
        "stream_token" => {
            let content = v.get("token").and_then(Value::as_str).unwrap_or("").to_string();
            Some(ServerEvent::TokenDelta { task_id, content })
        }
        "stream_chunk" => {
            let state = v.pointer("/data/state")?;
            if let Some(tc) = state.get("rawResponse").and_then(|r| r.get("toolCall")) {
                let tool_name = tc.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let arguments = tc.get("arguments").map_or_else(String::new, ToString::to_string);
                return Some(ServerEvent::ToolCallStart { task_id, tool_name, arguments });
            }
            if let Some(tr) = state.get("toolResult") {
                let tool_name = tr.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let is_error = tr.get("isError").and_then(Value::as_bool).unwrap_or(false);
                // `result` is usually a string; fall back to compact JSON.
                let result = tr
                    .get("result")
                    .and_then(Value::as_str)
                    .map_or_else(|| tr.get("result").map_or_else(String::new, ToString::to_string), str::to_string);
                return Some(ServerEvent::ToolCallComplete {
                    task_id,
                    tool_name,
                    result,
                    is_error,
                    duration_ms: 0,
                });
            }
            None
        }
        // The final text was already streamed via `stream_token`, so the terminal
        // event just signals completion.
        "eventual_response" => Some(ServerEvent::TaskComplete {
            task_id,
            iterations: 0,
            cost_usd: 0.0,
        }),
        "error" => {
            let message = v
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| v.pointer("/data/message").and_then(Value::as_str))
                .unwrap_or("operator error")
                .to_string();
            Some(ServerEvent::TaskError { task_id, message })
        }
        _ => None,
    }
}

/// Resolve the operator's local-flavor auth token (`SMOOTH_LOCAL_TOKEN` env, else
/// `~/.smooth/operator-token`). Empty when neither is present (a no-auth server).
fn resolve_token() -> String {
    if let Ok(t) = std::env::var("SMOOTH_LOCAL_TOKEN") {
        let t = t.trim().to_owned();
        if !t.is_empty() {
            return t;
        }
    }
    dirs_next::home_dir()
        .map(|h| h.join(".smooth").join("operator-token"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

type Pending = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<ServerEvent>>>>;

/// Process-global memo of the operator session id (`th code` is one process =
/// one conversation). Lets each per-turn [`OperatorClient`] reuse the same
/// server-side session so multi-turn history survives, without threading a
/// persistent client through the TUI.
static SESSION: Mutex<Option<String>> = Mutex::new(None);

/// The remembered operator session id from earlier turns, if any.
#[must_use]
pub fn remembered_session() -> Option<String> {
    SESSION.lock().ok().and_then(|g| g.clone())
}

/// Remember the operator session id for subsequent turns.
pub fn remember_session(session_id: &str) {
    if let Ok(mut g) = SESSION.lock() {
        *g = Some(session_id.to_string());
    }
}

/// A client to the operator's canonical WS protocol, holding one persistent
/// conversation session across turns.
pub struct OperatorClient {
    url: String,
    ws_tx: Option<mpsc::UnboundedSender<String>>,
    session_id: Option<String>,
    pending: Pending,
    connected: Arc<AtomicBool>,
    next_req: AtomicU64,
}

impl OperatorClient {
    /// Construct a client for `url` (e.g. `http://localhost:8787`).
    #[must_use]
    pub fn new(url: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            ws_tx: None,
            session_id: None,
            pending: Arc::new(Mutex::new(HashMap::new())),
            connected: Arc::new(AtomicBool::new(false)),
            next_req: AtomicU64::new(1),
        }
    }

    /// Whether the WS is connected + a session is open.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst) && self.session_id.is_some()
    }

    /// Reuse an existing operator session (so [`connect`](Self::connect) skips
    /// session creation). The operator keeps per-session conversation history
    /// server-side, so reusing the id across turns gives multi-turn memory even
    /// across fresh connections (the always-on daemon holds the session).
    pub fn set_session(&mut self, session_id: String) {
        self.session_id = Some(session_id);
    }

    /// The open conversation session id, if any (persist it to reuse next turn).
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn next_request_id(&self, prefix: &str) -> String {
        format!("{prefix}-{}", self.next_req.fetch_add(1, Ordering::SeqCst))
    }

    /// Ensure `th daemon operator` is up, connect, and open a session.
    ///
    /// # Errors
    /// Returns an error if the operator can't be started/reached or the session
    /// can't be created.
    pub async fn connect(&mut self) -> anyhow::Result<()> {
        self.ensure_server().await?;

        let token = resolve_token();
        let ws_base = self.url.replace("http://", "ws://").replace("https://", "wss://");
        let ws_url = if token.is_empty() {
            format!("{ws_base}/ws")
        } else {
            format!("{ws_base}/ws?token={}", urlencode(&token))
        };

        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| anyhow::anyhow!("operator WS connect failed: {e}"))?;
        let (mut sink, mut source) = ws_stream.split();

        let (send_tx, mut send_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(text) = send_rx.recv().await {
                if sink.send(tungstenite::Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.send(tungstenite::Message::Close(None)).await;
        });

        // The session-creation reply lands here; turn events route via `pending`.
        let (session_tx, mut session_rx) = mpsc::unbounded_channel::<String>();
        let pending = Arc::clone(&self.pending);
        let connected = Arc::clone(&self.connected);
        tokio::spawn(async move {
            while let Some(Ok(msg)) = source.next().await {
                let tungstenite::Message::Text(text) = msg else {
                    if matches!(msg, tungstenite::Message::Close(_)) {
                        break;
                    }
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                match v.get("type").and_then(Value::as_str) {
                    Some("immediate_response") => {
                        let _ = session_tx.send(text.to_string());
                    }
                    _ => route_turn_event(&pending, &v),
                }
            }
            connected.store(false, Ordering::SeqCst);
        });

        self.ws_tx = Some(send_tx);
        self.connected.store(true, Ordering::SeqCst);

        // Reuse an existing session (history is server-side) or open a new one.
        if self.session_id.is_none() {
            let req = self.next_request_id("cs");
            self.send(&json!({
                "action": "create_conversation_session",
                "requestId": req,
                "agentId": uuid::Uuid::new_v4().to_string(),
                "userName": "th code",
            }))?;
            let reply = tokio::time::timeout(Duration::from_secs(5), session_rx.recv())
                .await
                .map_err(|_| anyhow::anyhow!("timed out creating operator session"))?
                .ok_or_else(|| anyhow::anyhow!("operator closed before session was created"))?;
            let parsed: Value = serde_json::from_str(&reply)?;
            let sid = parsed
                .pointer("/data/sessionId")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("operator session reply missing sessionId: {reply}"))?;
            self.session_id = Some(sid.to_string());
        }
        // Keep `session_rx` alive for the connection's lifetime (the read loop
        // still routes any stray immediate_response to it harmlessly).
        std::mem::drop(session_rx);
        Ok(())
    }

    fn send(&self, value: &Value) -> anyhow::Result<()> {
        let tx = self.ws_tx.as_ref().ok_or_else(|| anyhow::anyhow!("not connected"))?;
        tx.send(value.to_string()).map_err(|e| anyhow::anyhow!("send failed: {e}"))
    }

    /// Run one turn: send `message` into the session and return a receiver of the
    /// turn's [`ServerEvent`]s (token deltas, tool calls, terminal complete/error).
    /// `model` / `budget` / `working_dir` / `agent` / `prior_messages` are accepted
    /// for signature-compatibility but not used — the operator session carries
    /// model + history server-side.
    ///
    /// # Errors
    /// Returns an error if no session is open or the message can't be sent.
    pub async fn run_task(
        &mut self,
        message: &str,
        _model: Option<&str>,
        _budget: Option<f64>,
        _working_dir: Option<&str>,
        _agent: Option<&str>,
        _prior_messages: Vec<crate::client::PriorMessage>,
    ) -> anyhow::Result<mpsc::UnboundedReceiver<ServerEvent>> {
        let sid = self.session_id.clone().ok_or_else(|| anyhow::anyhow!("no operator session"))?;
        let req = self.next_request_id("turn");
        let (tx, rx) = mpsc::unbounded_channel::<ServerEvent>();
        self.pending.lock().expect("pending lock").insert(req.clone(), tx);
        self.send(&json!({
            "action": "send_message",
            "requestId": req,
            "sessionId": sid,
            "message": message,
        }))?;
        Ok(rx)
    }

    /// Start `th daemon operator` if the operator isn't already reachable.
    async fn ensure_server(&self) -> anyhow::Result<()> {
        let health = format!("{}/health", self.url);
        let http = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;
        if http.get(&health).send().await.is_ok_and(|r| r.status().is_success()) {
            return Ok(());
        }
        let th_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("th"));
        tokio::process::Command::new(&th_bin)
            .args(["daemon", "operator"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to start `th daemon operator`: {e}"))?;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if http.get(&health).send().await.is_ok_and(|r| r.status().is_success()) {
                return Ok(());
            }
        }
        anyhow::bail!("`th daemon operator` did not become healthy within 10s")
    }
}

/// Route a turn event to its originating `run_task` receiver (by `requestId`),
/// dropping the registration on the terminal event so the receiver closes.
fn route_turn_event(pending: &Pending, v: &Value) {
    let Some(event) = map_event(v) else { return };
    let (task_id, terminal) = match &event {
        ServerEvent::TaskComplete { task_id, .. } | ServerEvent::TaskError { task_id, .. } => (task_id.clone(), true),
        ServerEvent::TokenDelta { task_id, .. } | ServerEvent::ToolCallStart { task_id, .. } | ServerEvent::ToolCallComplete { task_id, .. } => {
            (task_id.clone(), false)
        }
        _ => return,
    };
    let mut map = pending.lock().expect("pending lock");
    if let Some(tx) = map.get(&task_id) {
        let _ = tx.send(event);
    }
    if terminal {
        map.remove(&task_id);
    }
}

/// Minimal percent-encoding for a token in a query string (alnum + `-._~` pass).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn maps_stream_token() {
        let ev = map_event(&json!({"type":"stream_token","requestId":"turn-1","token":"hi"})).unwrap();
        assert!(matches!(ev, ServerEvent::TokenDelta { content, .. } if content == "hi"));
    }

    #[test]
    fn maps_tool_call_start_and_complete_from_chunks() {
        let start = map_event(&json!({
            "type":"stream_chunk","requestId":"turn-1","node":"bash",
            "data":{"state":{"rawResponse":{"toolCall":{"name":"bash","arguments":{"command":"ls"}}}}}
        }))
        .unwrap();
        assert!(matches!(start, ServerEvent::ToolCallStart { tool_name, .. } if tool_name == "bash"));

        let done = map_event(&json!({
            "type":"stream_chunk","requestId":"turn-1","node":"bash",
            "data":{"state":{"toolResult":{"name":"bash","isError":false,"result":"ok"}}}
        }))
        .unwrap();
        assert!(matches!(done, ServerEvent::ToolCallComplete { result, is_error: false, .. } if result == "ok"));
    }

    #[test]
    fn maps_terminal_and_error() {
        assert!(matches!(
            map_event(&json!({"type":"eventual_response","requestId":"t","status":200,"data":{}})).unwrap(),
            ServerEvent::TaskComplete { .. }
        ));
        assert!(matches!(
            map_event(&json!({"type":"error","requestId":"t","message":"boom"})).unwrap(),
            ServerEvent::TaskError { message, .. } if message == "boom"
        ));
    }

    #[test]
    fn ignores_unconsumed_events() {
        assert!(map_event(&json!({"type":"keepalive"})).is_none());
        assert!(map_event(&json!({"type":"stream_chunk","data":{"state":{"node":"plan"}}})).is_none());
    }
}

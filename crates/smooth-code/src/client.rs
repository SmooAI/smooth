//! BigSmoothClient — WebSocket client for smooth-code to talk to Big Smooth.
//!
//! Connects to the Big Smooth `/ws` endpoint, sends [`ClientEvent`]s, and
//! receives [`ServerEvent`]s.  Auto-starts Big Smooth if it is not already
//! running.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use smooth_operator::ws_resilience::{ConnectionManager, ConnectionState, MessageBuffer, ResiliencyConfig};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;

// ---------------------------------------------------------------------------
// Event types (local copies — same JSON shape as smooth-bigsmooth::events)
// ---------------------------------------------------------------------------

/// One message in the TUI's prior-conversation history sent on
/// each `TaskStart`. Mirrors the structure that
/// `smooth_operator::Conversation` expects so the runner can replay
/// the array as native `Message::user` / `Message::assistant` entries
/// without stringifying.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorMessage {
    /// `"user"` or `"assistant"`. Anything else is dropped at the
    /// runner end so a malformed entry can't poison the conversation.
    pub role: String,
    pub content: String,
}

/// Events sent from this client to Big Smooth.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientEvent {
    TaskStart {
        message: String,
        model: Option<String>,
        budget: Option<f64>,
        working_dir: Option<String>,
        /// Lead role to run under (`fixer` / `mapper` / `oracle` /
        /// `heckler`). `None` means "use the server default"
        /// (`fixer`). Unknown names surface as a TaskError.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        /// Prior turns of this TUI session as structured messages.
        /// The runner pre-populates its `Conversation` with these
        /// before pushing the current `message`, so the agent gets
        /// proper role-alternating history (pearl th-422b93). Empty
        /// or absent on the first turn.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prior_messages: Vec<PriorMessage>,
    },
    TaskCancel {
        task_id: String,
    },
    Steer {
        task_id: String,
        action: String,
        message: Option<String>,
    },
    Ping,
}

/// Events received from Big Smooth.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    TokenDelta {
        task_id: String,
        content: String,
    },
    /// Iteration boundary — fires when the agent enters a new LLM
    /// round. Pearl th-486bd0: clients use this to reset their
    /// streaming-message accumulator so successive iterations don't
    /// pile into one giant assistant bubble.
    LlmIteration {
        task_id: String,
        iteration: u32,
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
    PearlCreated {
        id: String,
        title: String,
    },
    NarcAlert {
        severity: String,
        category: String,
        message: String,
    },
    HealthUpdate {
        healthy: bool,
    },
    Connected {
        session_id: String,
        /// Conversation this session is bound to. Handing it back as
        /// `conversationId` on a later `create_conversation_session` resumes
        /// that conversation rather than starting a fresh one — this is what
        /// gives `th code` memory across turns (pearl th-255d2a).
        conversation_id: Option<String>,
    },
    Pong,
    Error {
        message: String,
    },
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Local auth token
// ---------------------------------------------------------------------------

/// Resolve the local Big Smooth auth token, mirroring the daemon's own order:
/// `SMOOTH_LOCAL_TOKEN` (env) → `~/.smooth/operator-token`.
///
/// **Read-only on purpose.** The daemon *provisions* the token (generating and
/// persisting it on first run); `th code` only ever consumes it — a client that
/// minted its own would just send a value the server never accepts. `None`
/// means "no token anywhere", and we then connect unauthenticated, which is
/// correct for a server not running strict auth.
fn local_token() -> Option<String> {
    if let Ok(env_token) = std::env::var("SMOOTH_LOCAL_TOKEN") {
        let env_token = env_token.trim().to_owned();
        if !env_token.is_empty() {
            return Some(env_token);
        }
    }
    let path: PathBuf = dirs_next::home_dir()?.join(".smooth").join("operator-token");
    let existing = std::fs::read_to_string(path).ok()?;
    let existing = existing.trim().to_owned();
    (!existing.is_empty()).then_some(existing)
}

/// Percent-encode a token for use in a query string.
///
/// The daemon's generated tokens are plain hex (nothing to escape), but
/// `SMOOTH_LOCAL_TOKEN` is user-supplied: a raw `&`, `#`, or space would
/// silently truncate the query and surface as a baffling 401. Unreserved
/// characters pass through; everything else becomes `%XX`.
fn percent_encode_token(token: &str) -> String {
    token
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Canonical operator protocol  <->  the TUI's internal event vocabulary
// ---------------------------------------------------------------------------
//
// Big Smooth speaks smooth-operator's canonical, schema-driven WS protocol —
// outbound frames are `{action, requestId, …}`, inbound are `{type, …}`. The
// TUI's `ClientEvent`/`ServerEvent` enums are the ORIGINAL bespoke
// `smooth-bigsmooth` shapes, and that crate was deleted with the microVM stack
// (th-f4a801), so nothing has spoken them since. Rather than rewrite the whole
// TUI, this layer makes `th code` a first-class canonical client — exactly like
// the web SPA (`crates/smooth-web/web/src/operator.ts`) — and translates at the
// edge, leaving `app.rs`/`render.rs` untouched (pearl th-248f33).

/// The TUI runs one turn at a time, so a fixed correlation id is enough — the
/// canonical protocol scopes streaming to the session, not a task id.
const TURN_ID: &str = "turn";

/// Translate one canonical inbound frame into the TUI's internal event.
///
/// `None` means "nothing for the UI" — an unrecognized `type`, or a frame we
/// deliberately drop (see `stream_reasoning`).
fn translate_frame(v: &serde_json::Value) -> Option<ServerEvent> {
    let ty = v.get("type")?.as_str()?;
    match ty {
        // The reply to `create_conversation_session` carries the session id and
        // is what marks us connected. Other `immediate_response`s (history,
        // conversation lists, renames) have no `sessionId` and fall through to
        // `None`, which is what we want — they aren't UI events here.
        "immediate_response" => {
            let data = v.get("data")?;
            let session_id = data.get("sessionId")?.as_str()?.to_string();
            let conversation_id = data.get("conversationId").and_then(serde_json::Value::as_str).map(str::to_string);
            Some(ServerEvent::Connected { session_id, conversation_id })
        }
        "stream_token" => Some(ServerEvent::TokenDelta {
            task_id: TURN_ID.to_string(),
            content: v.get("token").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
        }),
        // Reasoning rides its own channel and must NEVER render as the answer
        // (th-4d8682, and the persona's no-chain-of-thought rule). Dropped
        // rather than merged into the reply stream.
        "stream_reasoning" => None,
        "stream_chunk" => {
            // Both the call and its result are nested under `rawResponse` —
            // reading `state.toolResult` instead leaves every tool stuck
            // "running" forever, the bug operator.ts documents.
            let raw = v.get("data")?.get("state")?.get("rawResponse")?;
            if let Some(call) = raw.get("toolCall") {
                return Some(ServerEvent::ToolCallStart {
                    task_id: TURN_ID.to_string(),
                    tool_name: call.get("name").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                    arguments: stringify(call.get("arguments")),
                });
            }
            let res = raw.get("toolResult")?;
            Some(ServerEvent::ToolCallComplete {
                task_id: TURN_ID.to_string(),
                tool_name: res.get("name").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                result: stringify(res.get("result")),
                is_error: res.get("isError").and_then(serde_json::Value::as_bool).unwrap_or(false),
                duration_ms: 0,
            })
        }
        "eventual_response" => Some(ServerEvent::TaskComplete {
            task_id: TURN_ID.to_string(),
            iterations: 0,
            cost_usd: 0.0,
        }),
        "error" => Some(ServerEvent::Error {
            message: v
                .get("message")
                .or_else(|| v.get("error"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error")
                .to_string(),
        }),
        "pong" => Some(ServerEvent::Pong),
        _ => None,
    }
}

/// JSON value -> plain string: already-string values pass through unquoted,
/// anything else is serialized. (Tool arguments arrive both ways.)
fn stringify(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Translate an outbound TUI event into a canonical frame.
///
/// `None` means "nothing to send" (e.g. a turn was requested before the
/// session id arrived — the caller buffers instead).
fn to_canonical_frame(event: &ClientEvent, session_id: Option<&str>, request_id: &str) -> Option<serde_json::Value> {
    match event {
        ClientEvent::TaskStart { message, model, .. } => {
            let sid = session_id?;
            let mut frame = serde_json::json!({
                "action": "send_message",
                "requestId": request_id,
                "sessionId": sid,
                "message": message,
            });
            if let Some(m) = model.as_ref().filter(|m| !m.is_empty()) {
                frame["model"] = serde_json::Value::String(m.clone());
            }
            Some(frame)
        }
        ClientEvent::Ping => Some(serde_json::json!({ "action": "ping", "requestId": request_id })),
        // The canonical protocol has no cancel/steer verb yet; dropping beats
        // sending a frame the server will reject as unknown.
        ClientEvent::TaskCancel { .. } | ClientEvent::Steer { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// BigSmoothClient
// ---------------------------------------------------------------------------

/// WebSocket client for communicating with Big Smooth.
///
/// Includes connection resiliency: automatic reconnection with exponential
/// backoff, heartbeat keep-alive, and an outbound message buffer for messages
/// sent while disconnected.
pub struct BigSmoothClient {
    url: String,
    ws_tx: Option<mpsc::UnboundedSender<String>>,
    event_rx: Option<mpsc::UnboundedReceiver<ServerEvent>>,
    connected: Arc<AtomicBool>,
    conn_mgr: Arc<ConnectionManager>,
    msg_buffer: Arc<MessageBuffer>,
    /// Session id handed back by `create_conversation_session`; every
    /// `send_message` must carry it. Shared because the read loop discovers it
    /// and the send path needs it.
    session_id: Arc<std::sync::Mutex<Option<String>>>,
    /// Conversation to resume on connect, and the one the server confirmed.
    ///
    /// `th code` builds a fresh client per turn, so without this every turn
    /// would open its own conversation and the agent would remember nothing
    /// (pearl th-255d2a). Seed it with [`Self::resume_conversation`] and the
    /// handshake asks the server to append to that conversation instead.
    conversation_id: Arc<std::sync::Mutex<Option<String>>>,
    /// Monotonic source for canonical `requestId`s.
    next_request: Arc<AtomicU64>,
}

impl BigSmoothClient {
    /// Create a new client targeting the given Big Smooth base URL
    /// (e.g. `"http://localhost:4400"`).
    pub fn new(url: &str) -> Self {
        Self::with_config(url, ResiliencyConfig::default())
    }

    /// Create a new client with custom resiliency configuration.
    pub fn with_config(url: &str, config: ResiliencyConfig) -> Self {
        let buffer_size = config.message_buffer_size;
        Self {
            url: url.trim_end_matches('/').to_string(),
            ws_tx: None,
            event_rx: None,
            connected: Arc::new(AtomicBool::new(false)),
            conn_mgr: Arc::new(ConnectionManager::new(config)),
            msg_buffer: Arc::new(MessageBuffer::new(buffer_size)),
            session_id: Arc::new(std::sync::Mutex::new(None)),
            conversation_id: Arc::new(std::sync::Mutex::new(None)),
            next_request: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Ask the next [`Self::connect`] to resume `conversation_id` instead of
    /// starting a new conversation. Call before `connect`; a `None` or empty
    /// id is ignored, which makes "first turn of the session" the natural
    /// no-op case.
    pub fn resume_conversation(&mut self, conversation_id: Option<&str>) {
        if let Some(id) = conversation_id.filter(|c| !c.trim().is_empty()) {
            *self.conversation_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_string());
        }
    }

    /// The conversation the server bound this session to, once connected.
    /// Persist it across turns and feed it back via [`Self::resume_conversation`].
    #[must_use]
    pub fn conversation_id(&self) -> Option<String> {
        self.conversation_id.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Connect to Big Smooth over WebSocket.
    ///
    /// If Big Smooth is not running, attempts to start it by spawning `th up`
    /// in the background and waiting up to 10 seconds for health.
    ///
    /// On success, spawns a heartbeat task and marks the connection as
    /// `Connected`.  Any messages buffered while disconnected are drained and
    /// sent immediately.
    pub async fn connect(&mut self) -> anyhow::Result<()> {
        self.conn_mgr.set_connecting();
        self.ensure_server().await?;

        let ws_url = self.url.replace("http://", "ws://").replace("https://", "wss://");
        let ws_url = format!("{ws_url}/ws");
        // The daemon runs the operator's STRICT-auth local flavor: a `/ws`
        // upgrade without a valid local token is rejected 401 (not degraded to
        // anonymous). The token is accepted ONLY as a `token` query param — the
        // upgrade path does not consult an `Authorization` header — so it goes
        // on the URL. Without this every `th code` connect died on
        // "401 Unauthorized" even with the daemon healthy.
        let ws_url = match local_token() {
            Some(token) => format!("{ws_url}?token={}", percent_encode_token(&token)),
            None => ws_url,
        };

        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url).await.map_err(|e| {
            self.conn_mgr.disconnected();
            let hint = if e.to_string().contains("401") {
                "\nThe server is running but rejected the token. Big Smooth reads \
                 `SMOOTH_LOCAL_TOKEN`, else `~/.smooth/operator-token` — make sure this shell \
                 resolves the SAME token the running daemon did (a stale env var or a \
                 regenerated token file causes this)."
            } else {
                ""
            };
            anyhow::anyhow!("WebSocket connection failed: {e}{hint}")
        })?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        // Channel: caller -> WS write loop
        let (send_tx, mut send_rx) = mpsc::unbounded_channel::<String>();
        // Channel: WS read loop -> caller
        let (event_tx, event_rx) = mpsc::unbounded_channel::<ServerEvent>();

        let connected = Arc::clone(&self.connected);

        // Write loop
        tokio::spawn(async move {
            while let Some(text) = send_rx.recv().await {
                if ws_sink.send(tungstenite::Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            let _ = ws_sink.send(tungstenite::Message::Close(None)).await;
        });

        // Read loop — on disconnect, mark state and trigger reconnect
        let connected_read = Arc::clone(&connected);
        let conn_mgr_read = Arc::clone(&self.conn_mgr);
        let session_read = Arc::clone(&self.session_id);
        let conversation_read = Arc::clone(&self.conversation_id);
        tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_source.next().await {
                let text = match msg {
                    tungstenite::Message::Text(t) => t.to_string(),
                    tungstenite::Message::Close(_) => break,
                    _ => continue,
                };

                let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                let Some(event) = translate_frame(&frame) else {
                    continue; // not a UI-bearing frame
                };
                if let ServerEvent::Connected { session_id, conversation_id } = &event {
                    *session_read.lock().unwrap_or_else(|e| e.into_inner()) = Some(session_id.clone());
                    // Record the conversation the server actually bound us to
                    // — on a resume it echoes the one we asked for, on a fresh
                    // session it's the newly created one. Either way this is
                    // what the next turn resumes.
                    if let Some(cid) = conversation_id {
                        *conversation_read.lock().unwrap_or_else(|e| e.into_inner()) = Some(cid.clone());
                    }
                    connected_read.store(true, Ordering::SeqCst);
                }
                if event_tx.send(event).is_err() {
                    break;
                }
            }
            connected_read.store(false, Ordering::SeqCst);
            conn_mgr_read.disconnected();
        });

        self.ws_tx = Some(send_tx.clone());
        self.event_rx = Some(event_rx);

        // Canonical handshake: the server hands back a session id in reply to
        // `create_conversation_session`, and that reply is what marks us
        // connected. Same opening move the web SPA makes (operator.ts).
        let mut hello = serde_json::json!({
            "action": "create_conversation_session",
            "requestId": format!("cs-{}", self.next_request.fetch_add(1, Ordering::Relaxed)),
            "agentId": uuid::Uuid::new_v4().to_string(),
            "userName": "th code",
        });
        // Resume rather than start fresh when we already know the conversation.
        // The server binds the new session to it and `send_message` appends,
        // so the agent sees this TUI session's earlier turns. Without it every
        // turn was its own conversation and Big Smooth remembered nothing
        // (pearl th-255d2a).
        if let Some(cid) = self.conversation_id() {
            hello["conversationId"] = serde_json::Value::String(cid);
        }
        send_tx.send(hello.to_string()).map_err(|e| anyhow::anyhow!("failed to open session: {e}"))?;

        // Wait for Connected event (up to 5s)
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.connected.load(Ordering::SeqCst) {
            if tokio::time::Instant::now() >= deadline {
                self.conn_mgr.disconnected();
                anyhow::bail!("Timed out waiting for Connected event from Big Smooth");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Mark connected + reset attempts
        self.conn_mgr.connected();

        // Drain buffered messages
        for msg in self.msg_buffer.drain() {
            let _ = send_tx.send(msg);
        }

        // Spawn heartbeat task
        let hb_tx = send_tx;
        let hb_connected = Arc::clone(&self.connected);
        let hb_interval = self.conn_mgr.config().heartbeat_interval;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(hb_interval).await;
                if !hb_connected.load(Ordering::SeqCst) {
                    break;
                }
                let ping = serde_json::to_string(&ClientEvent::Ping).unwrap_or_default();
                if hb_tx.send(ping).is_err() {
                    break;
                }
            }
        });

        Ok(())
    }

    /// Attempt to reconnect using exponential backoff.
    ///
    /// Returns `Ok(())` once reconnected or `Err` if max attempts exhausted.
    pub async fn reconnect(&mut self) -> anyhow::Result<()> {
        while self.conn_mgr.should_reconnect() {
            self.conn_mgr.set_reconnecting();
            let attempt = self.conn_mgr.reconnect_attempts();
            let backoff = self.conn_mgr.backoff_duration(attempt.saturating_sub(1));
            tokio::time::sleep(backoff).await;

            match self.connect().await {
                Ok(()) => return Ok(()),
                Err(_) => continue,
            }
        }
        anyhow::bail!("Max reconnect attempts ({}) exhausted", self.conn_mgr.reconnect_attempts())
    }

    /// pearl th-461ab9 (Mode B fix): Try to connect with bounded
    /// exponential backoff for the INITIAL connection.
    ///
    /// Mirrors `OperativeClient::connect_with_retry` for the bench / headless
    /// path. `connect()` is one-shot and does a 5s wait for the
    /// `ServerEvent::Connected` handshake. When Big Smooth was just (re)started
    /// — e.g. by the LaunchAgent KeepAlive cycle, or because the bench
    /// harness's `ensure_server` raced the leader's bind — the first attempt
    /// can fail before the server's WS handler is ready. This wrapper retries
    /// with the same exponential-backoff machinery `reconnect()` uses but
    /// driven by an attempt-count cap so a permanently-unreachable Big Smooth
    /// (e.g. wrong port, crashed) still fails fast.
    ///
    /// `max_attempts` of 0 or 1 falls back to single-shot `connect()` for
    /// backwards compatibility. Recommended value is 5: with the default
    /// `ResiliencyConfig` (base 1s, max 30s) total wait before final failure
    /// is ~31s of mostly-sleeping (1+2+4+8+16s).
    pub async fn connect_with_retry(&mut self, max_attempts: u32) -> anyhow::Result<()> {
        if max_attempts <= 1 {
            return self.connect().await;
        }
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..max_attempts {
            match self.connect().await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let backoff = self.conn_mgr.backoff_duration(attempt);
                    tracing::debug!(
                        attempt = attempt + 1,
                        max_attempts,
                        backoff_ms = backoff.as_millis() as u64,
                        error = %e,
                        "BigSmoothClient connect attempt failed; sleeping before retry"
                    );
                    last_err = Some(e);
                    if attempt + 1 < max_attempts {
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("connect_with_retry exhausted {max_attempts} attempts")))
    }

    /// Send a task start and return a receiver for streaming server events.
    ///
    /// The returned receiver will yield events until the task completes or
    /// errors.  The caller should drain this receiver.
    pub async fn run_task(
        &mut self,
        message: &str,
        model: Option<&str>,
        budget: Option<f64>,
        working_dir: Option<&str>,
        agent: Option<&str>,
        prior_messages: Vec<PriorMessage>,
    ) -> anyhow::Result<mpsc::UnboundedReceiver<ServerEvent>> {
        let event = ClientEvent::TaskStart {
            message: message.to_string(),
            model: model.map(ToString::to_string),
            budget,
            working_dir: working_dir.map(ToString::to_string),
            agent: agent.map(ToString::to_string),
            prior_messages,
        };
        self.send(&event).await?;

        // Return a new channel that filters events for this task
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(mut source) = self.event_rx.take() {
            tokio::spawn(async move {
                while let Some(event) = source.recv().await {
                    let is_terminal = matches!(event, ServerEvent::TaskComplete { .. } | ServerEvent::TaskError { .. });
                    if tx.send(event).is_err() {
                        break;
                    }
                    if is_terminal {
                        break;
                    }
                }
                // Put remaining events back? No — we consume the stream for this task.
                drop(source);
            });
        }

        Ok(rx)
    }

    /// Cancel a running task.
    pub async fn cancel_task(&self, task_id: &str) -> anyhow::Result<()> {
        self.send(&ClientEvent::TaskCancel { task_id: task_id.to_string() }).await
    }

    /// Send a steering command to a running task.
    pub async fn steer(&self, task_id: &str, action: &str, message: Option<&str>) -> anyhow::Result<()> {
        self.send(&ClientEvent::Steer {
            task_id: task_id.to_string(),
            action: action.to_string(),
            message: message.map(ToString::to_string),
        })
        .await
    }

    /// Returns `true` if the WebSocket is currently connected.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Returns the current connection state for UI display.
    pub fn connection_state(&self) -> ConnectionState {
        self.conn_mgr.state()
    }

    /// Send a raw [`ClientEvent`] to Big Smooth.
    ///
    /// If the connection is down, the message is buffered (up to the configured
    /// limit) and will be sent when the connection is re-established.
    pub async fn send(&self, event: &ClientEvent) -> anyhow::Result<()> {
        let session = self.session_id.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let request_id = format!("turn-{}", self.next_request.fetch_add(1, Ordering::Relaxed));
        let Some(frame) = to_canonical_frame(event, session.as_deref(), &request_id) else {
            // Nothing the canonical protocol carries (cancel/steer), or no
            // session yet — drop rather than send a frame the server rejects.
            return Ok(());
        };
        let json = frame.to_string();

        if let Some(tx) = self.ws_tx.as_ref() {
            if self.connected.load(Ordering::SeqCst) {
                return tx.send(json).map_err(|e| anyhow::anyhow!("Failed to send: {e}"));
            }
        }

        // Disconnected — buffer the message
        if self.msg_buffer.enqueue(json) {
            Ok(())
        } else {
            anyhow::bail!("Message buffer full — cannot queue message while disconnected")
        }
    }

    /// Receive the next server event (blocking).
    pub async fn recv(&mut self) -> Option<ServerEvent> {
        if let Some(rx) = self.event_rx.as_mut() {
            rx.recv().await
        } else {
            None
        }
    }

    /// Ensure Big Smooth is running, starting it if needed.
    async fn ensure_server(&self) -> anyhow::Result<()> {
        // A live socket at the target address IS the readiness signal for an
        // externally-managed engine. The polyglot smooth-operator LocalServers
        // (go/ts/python/dotnet) serve `/ws` but no `/health`, so the HTTP probe
        // below would wrongly conclude "not started" and try to `th up` a Rust
        // daemon — never reaching the real WS connect. Short-circuit on a live
        // socket first; the `/health`+autostart path still runs for `th code`
        // when nothing is listening yet. (bench engine axis, th-4c3e2d)
        {
            use std::net::ToSocketAddrs;
            if let Some(addr) = self.url.split("://").nth(1).and_then(|s| s.split('/').next()) {
                if let Ok(mut it) = addr.to_socket_addrs() {
                    if let Some(sa) = it.next() {
                        if std::net::TcpStream::connect_timeout(&sa, Duration::from_secs(2)).is_ok() {
                            return Ok(());
                        }
                    }
                }
            }
        }

        let health_url = format!("{}/health", self.url);
        let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;

        if client.get(&health_url).send().await.is_ok_and(|r| r.status().is_success()) {
            return Ok(());
        }

        // Try to start Big Smooth
        let th_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("th"));
        let _child = tokio::process::Command::new(&th_bin)
            .arg("up")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn Big Smooth (th up): {e}"))?;

        // Wait up to 10s for health
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if client.get(&health_url).send().await.is_ok_and(|r| r.status().is_success()) {
                return Ok(());
            }
        }

        anyhow::bail!("Big Smooth failed to start within 10 seconds")
    }
}

impl std::fmt::Debug for BigSmoothClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BigSmoothClient")
            .field("url", &self.url)
            .field("connected", &self.is_connected())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_event_task_start_serialization() {
        let event = ClientEvent::TaskStart {
            message: "build the thing".into(),
            model: Some("gpt-4".into()),
            budget: Some(1.5),
            working_dir: Some("/tmp".into()),
            agent: Some("mapper".into()),
            prior_messages: vec![PriorMessage {
                role: "user".into(),
                content: "what repo is this?".into(),
            }],
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains(r#""type":"TaskStart"#));
        assert!(json.contains(r#""message":"build the thing"#));
        assert!(json.contains(r#""model":"gpt-4"#));
        assert!(json.contains(r#""budget":1.5"#));
        assert!(json.contains(r#""agent":"mapper"#));
        assert!(json.contains(r#""prior_messages""#));

        // Roundtrip
        let parsed: ClientEvent = serde_json::from_str(&json).expect("deserialize");
        if let ClientEvent::TaskStart {
            message,
            model,
            budget,
            working_dir,
            agent,
            prior_messages,
        } = parsed
        {
            assert_eq!(message, "build the thing");
            assert_eq!(model.as_deref(), Some("gpt-4"));
            assert_eq!(budget, Some(1.5));
            assert_eq!(working_dir.as_deref(), Some("/tmp"));
            assert_eq!(agent.as_deref(), Some("mapper"));
            assert_eq!(prior_messages.len(), 1);
            assert_eq!(prior_messages[0].role, "user");
            assert_eq!(prior_messages[0].content, "what repo is this?");
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn client_event_task_start_accepts_missing_agent() {
        // Back-compat: clients that don't send `agent` should still
        // deserialize (the server defaults to `fixer`).
        let json = r#"{"type":"TaskStart","message":"hi","model":null,"budget":null,"working_dir":null}"#;
        let parsed: ClientEvent = serde_json::from_str(json).expect("deserialize without agent field");
        if let ClientEvent::TaskStart { agent, .. } = parsed {
            assert!(agent.is_none(), "missing agent should deserialize as None");
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn server_event_token_delta_deserialization() {
        let json = r#"{"type":"TokenDelta","task_id":"task-1","content":"hello world"}"#;
        let event: ServerEvent = serde_json::from_str(json).expect("deserialize");
        if let ServerEvent::TokenDelta { task_id, content } = event {
            assert_eq!(task_id, "task-1");
            assert_eq!(content, "hello world");
        } else {
            panic!("unexpected variant: {event:?}");
        }
    }

    #[test]
    fn new_sets_correct_url() {
        let client = BigSmoothClient::new("http://localhost:4400");
        assert_eq!(client.url, "http://localhost:4400");

        // Trailing slash stripped
        let client2 = BigSmoothClient::new("http://localhost:4400/");
        assert_eq!(client2.url, "http://localhost:4400");
    }

    #[test]
    fn is_connected_returns_false_before_connect() {
        let client = BigSmoothClient::new("http://localhost:4400");
        assert!(!client.is_connected());
    }

    // Both `connect_with_retry` tests below aim a connection at a closed
    // loopback port and rely on it failing fast. That holds on Unix, which
    // answers with an immediate ECONNREFUSED, but not on Windows: the stack
    // silently drops the SYN there and each attempt burns the full TCP
    // connect timeout (measured at ~213s per attempt on windows-latest, so
    // 641s for the 3-attempt case — pearl th-a165b4). The behaviour under
    // test is platform-independent and covered on Unix; running them on
    // Windows would add ~14 minutes to every CI run to assert the same thing.
    #[cfg(unix)]
    #[tokio::test]
    async fn connect_with_retry_max_attempts_zero_falls_back_to_single_shot() {
        // pearl th-461ab9: bench-side mirror of OperativeClient's
        // connect_with_retry tests. Use a tight ResiliencyConfig so the test
        // doesn't pause the full default backoff (base 1s, max 30s).
        let cfg = ResiliencyConfig {
            base_backoff: std::time::Duration::from_millis(1),
            max_backoff: std::time::Duration::from_millis(2),
            ..ResiliencyConfig::default()
        };
        // Port 1 has nothing listening — `ensure_server` fails first because
        // the bench harness will try to spawn a `<smooth-bench> up` shim
        // which errors out, then health probe times out.
        let mut client = BigSmoothClient::with_config("http://127.0.0.1:1", cfg);
        let result = client.connect_with_retry(0).await;
        assert!(result.is_err(), "max_attempts=0 must surface the same error as single-shot connect()");
    }

    // Unix-only for the same reason as the test above.
    #[cfg(unix)]
    #[tokio::test]
    async fn connect_with_retry_returns_last_error_after_exhausting_attempts() {
        // pearl th-461ab9: confirm we don't hang when every attempt fails.
        let cfg = ResiliencyConfig {
            base_backoff: std::time::Duration::from_millis(1),
            max_backoff: std::time::Duration::from_millis(2),
            ..ResiliencyConfig::default()
        };
        let mut client = BigSmoothClient::with_config("http://127.0.0.1:1", cfg);
        let started = std::time::Instant::now();
        let result = client.connect_with_retry(3).await;
        let elapsed = started.elapsed();
        assert!(result.is_err(), "all attempts must fail against an unreachable URL");
        // The bench harness's `ensure_server` itself sleeps up to 10s per
        // attempt waiting for /health to come up. With 3 attempts, total
        // wall-clock should be bounded by ~30s + tiny backoff. The test
        // mostly proves the function returns rather than hangs forever.
        assert!(
            elapsed < std::time::Duration::from_secs(60),
            "connect_with_retry should not hang; took {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn send_serializes_and_sends_via_channel() {
        // Create a client with a manually wired-up channel
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let config = ResiliencyConfig::default();
        let buffer_size = config.message_buffer_size;
        let conn_mgr = Arc::new(ConnectionManager::new(config));
        conn_mgr.connected();
        let client = BigSmoothClient {
            url: "http://localhost:4400".into(),
            ws_tx: Some(tx),
            event_rx: None,
            connected: Arc::new(AtomicBool::new(true)),
            conn_mgr,
            msg_buffer: Arc::new(MessageBuffer::new(buffer_size)),
            // A live session, so `send` produces a real canonical frame.
            session_id: Arc::new(std::sync::Mutex::new(Some("sess-test".to_string()))),
            conversation_id: Arc::new(std::sync::Mutex::new(None)),
            next_request: Arc::new(AtomicU64::new(1)),
        };

        // th-248f33: the wire is now the CANONICAL operator protocol
        // (`{action, requestId, …}`), not the deleted bespoke `{"type":"Ping"}`
        // shape this test used to assert.
        let event = ClientEvent::Ping;
        client.send(&event).await.expect("send");

        let received = rx.recv().await.expect("receive");
        assert!(received.contains(r#""action":"ping""#), "canonical ping frame: {received}");

        // A turn rides `send_message` with the session id attached.
        let turn = ClientEvent::TaskStart {
            message: "hello".into(),
            model: None,
            budget: None,
            working_dir: None,
            agent: None,
            prior_messages: vec![],
        };
        client.send(&turn).await.expect("send turn");
        let received2 = rx.recv().await.expect("receive turn");
        assert!(received2.contains(r#""action":"send_message""#), "canonical turn frame: {received2}");
        assert!(received2.contains("sess-test"), "carries the session id: {received2}");

        // Cancel has no canonical verb — it must NOT put a bogus frame on the
        // wire, so nothing further arrives.
        let cancel = ClientEvent::TaskCancel { task_id: "t-42".into() };
        client.send(&cancel).await.expect("send cancel");
        assert!(rx.try_recv().is_err(), "cancel must not invent a wire frame");
    }

    /// Regression (pearl th-6dd202): `th code` connected with NO auth while the
    /// daemon runs the operator's strict-auth flavor, so every session died on
    /// "401 Unauthorized". The token must ride the query string — verified
    /// against a live daemon: no-auth=401, `Authorization: Bearer`=401,
    /// `?token=<correct>`=101 Switching Protocols.
    #[test]
    fn token_is_percent_encoded_for_the_query_string() {
        // Plain hex (what the daemon generates) passes through untouched.
        assert_eq!(percent_encode_token("0a9f4c2b7e1d"), "0a9f4c2b7e1d");
        // Unreserved characters are preserved.
        assert_eq!(percent_encode_token("a-b.c_d~e"), "a-b.c_d~e");
        // A user-supplied SMOOTH_LOCAL_TOKEN with query metacharacters would
        // otherwise truncate the URL and surface as a baffling 401.
        assert_eq!(percent_encode_token("a&b#c d"), "a%26b%23c%20d");
        assert_eq!(percent_encode_token("p/q?r=s"), "p%2Fq%3Fr%3Ds");
    }

    /// The env var wins over the token file, and whitespace is trimmed — the
    /// same resolution the daemon does when it provisions the token.
    #[test]
    fn local_token_prefers_env_and_trims() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = std::env::var("SMOOTH_LOCAL_TOKEN").ok();
        std::env::set_var("SMOOTH_LOCAL_TOKEN", "  tok-from-env  ");
        assert_eq!(local_token().as_deref(), Some("tok-from-env"));
        // An empty/whitespace env var must NOT mask the file fallback.
        std::env::set_var("SMOOTH_LOCAL_TOKEN", "   ");
        assert_ne!(local_token().as_deref(), Some(""), "blank env must not resolve to an empty token");
        match prev {
            Some(v) => std::env::set_var("SMOOTH_LOCAL_TOKEN", v),
            None => std::env::remove_var("SMOOTH_LOCAL_TOKEN"),
        }
    }

    /// Serializes the tests that mutate the process-global SMOOTH_LOCAL_TOKEN;
    /// cargo runs tests in parallel threads of one process.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}

#[cfg(test)]
mod canonical_protocol_tests {
    use super::*;
    use serde_json::json;

    /// The reply to `create_conversation_session` is what marks us connected —
    /// waiting for a bespoke `Connected` frame that no server has emitted since
    /// smooth-bigsmooth was deleted is exactly what hung every turn (th-248f33).
    #[test]
    fn session_reply_becomes_connected() {
        let ev = translate_frame(&json!({
            "type": "immediate_response",
            "status": 200,
            "data": { "sessionId": "sess-1", "agentId": "a-1" }
        }));
        match ev {
            Some(ServerEvent::Connected { session_id, conversation_id }) => {
                assert_eq!(session_id, "sess-1");
                // A server that omits `conversationId` must not break the
                // handshake — the next turn simply starts a new conversation.
                assert_eq!(conversation_id, None);
            }
            other => panic!("expected Connected, got {other:?}"),
        }
    }

    /// th-255d2a: the handshake reply carries the conversation the session was
    /// bound to, and the client must surface it — it is the only thing that
    /// lets the next turn resume instead of starting over.
    #[test]
    fn connected_carries_conversation_id_when_present() {
        let ev = translate_frame(&json!({
            "type": "immediate_response",
            "status": 200,
            "data": { "sessionId": "sess-2", "conversationId": "conv-9", "agentId": "a-1" }
        }));
        match ev {
            Some(ServerEvent::Connected { session_id, conversation_id }) => {
                assert_eq!(session_id, "sess-2");
                assert_eq!(conversation_id.as_deref(), Some("conv-9"));
            }
            other => panic!("expected Connected, got {other:?}"),
        }
    }

    /// Seeding a conversation is what makes the *next* connect a resume.
    /// Blank/absent ids are ignored so the first turn stays a fresh start.
    #[test]
    fn resume_conversation_seeds_and_ignores_blanks() {
        let mut client = BigSmoothClient::new("http://localhost:4400");
        assert_eq!(client.conversation_id(), None);

        client.resume_conversation(None);
        assert_eq!(client.conversation_id(), None, "None must not seed");

        client.resume_conversation(Some("   "));
        assert_eq!(client.conversation_id(), None, "blank must not seed");

        client.resume_conversation(Some("conv-42"));
        assert_eq!(client.conversation_id().as_deref(), Some("conv-42"));
    }

    /// Other `immediate_response`s (history, lists, renames) carry no session id
    /// and must NOT be mistaken for the handshake reply.
    #[test]
    fn immediate_response_without_session_is_not_connected() {
        assert!(translate_frame(&json!({"type":"immediate_response","data":{"title":"x"}})).is_none());
    }

    #[test]
    fn stream_token_becomes_token_delta() {
        match translate_frame(&json!({"type":"stream_token","token":"hel"})) {
            Some(ServerEvent::TokenDelta { content, .. }) => assert_eq!(content, "hel"),
            other => panic!("expected TokenDelta, got {other:?}"),
        }
    }

    /// Reasoning must never reach the answer stream.
    #[test]
    fn reasoning_is_dropped_not_rendered_as_the_answer() {
        assert!(translate_frame(&json!({"type":"stream_reasoning","token":"thinking..."})).is_none());
    }

    /// Tool call AND result both live under `rawResponse`; reading
    /// `state.toolResult` leaves every tool stuck "running" forever.
    #[test]
    fn tool_call_and_result_ride_raw_response() {
        let start = translate_frame(&json!({
            "type": "stream_chunk",
            "data": {"state": {"rawResponse": {"toolCall": {"name":"read_file","arguments":{"path":"a.rs"}}}}}
        }));
        match start {
            Some(ServerEvent::ToolCallStart { tool_name, arguments, .. }) => {
                assert_eq!(tool_name, "read_file");
                assert!(arguments.contains("a.rs"), "args serialized: {arguments}");
            }
            other => panic!("expected ToolCallStart, got {other:?}"),
        }
        let done = translate_frame(&json!({
            "type": "stream_chunk",
            "data": {"state": {"rawResponse": {"toolResult": {"name":"read_file","result":"contents","isError":false}}}}
        }));
        match done {
            Some(ServerEvent::ToolCallComplete { result, is_error, .. }) => {
                assert_eq!(result, "contents");
                assert!(!is_error);
            }
            other => panic!("expected ToolCallComplete, got {other:?}"),
        }
    }

    #[test]
    fn unknown_frames_are_ignored() {
        assert!(translate_frame(&json!({"type":"otp_sent"})).is_none());
        assert!(translate_frame(&json!({"nope":1})).is_none());
    }

    /// A turn is `send_message` carrying the session id — and is withheld
    /// entirely until the handshake supplies one.
    #[test]
    fn task_start_becomes_send_message_and_needs_a_session() {
        let ev = ClientEvent::TaskStart {
            message: "hi".into(),
            model: Some("smooth-coding".into()),
            budget: None,
            working_dir: None,
            agent: None,
            prior_messages: vec![],
        };
        let frame = to_canonical_frame(&ev, Some("sess-9"), "turn-1").expect("frame");
        assert_eq!(frame["action"], "send_message");
        assert_eq!(frame["sessionId"], "sess-9");
        assert_eq!(frame["message"], "hi");
        assert_eq!(frame["model"], "smooth-coding");
        assert_eq!(frame["requestId"], "turn-1");
        // No session yet -> nothing goes on the wire.
        assert!(to_canonical_frame(&ev, None, "turn-2").is_none());
    }

    #[test]
    fn cancel_and_steer_are_not_invented_on_the_wire() {
        let cancel = ClientEvent::TaskCancel { task_id: "t".into() };
        assert!(to_canonical_frame(&cancel, Some("s"), "r").is_none());
        let ping = to_canonical_frame(&ClientEvent::Ping, Some("s"), "r-1").expect("ping");
        assert_eq!(ping["action"], "ping");
    }
}

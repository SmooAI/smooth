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
        /// Composer attachments as full `data:<mime>;base64,…` strings —
        /// same wire shape the web SPA sends (`frame.images`). Empty for
        /// text-only turns, and omitted from the frame entirely so nothing
        /// changes there (pearl th-d16f7c).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
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

/// Per-turn token accounting and cost.
///
/// Read off the canonical `eventual_response`'s optional `data.data.usage`
/// object (`{costUsd, promptTokens, completionTokens}`). The server omits it
/// entirely when the engine reported no usage, which is why every consumer
/// takes it as an `Option` — a turn we know nothing about must render nothing,
/// not a confident `0 tok · $0` (pearl th-d49538).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub cost_usd: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
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
        /// How long the tool ran, when the *server* says so. `None` is the
        /// normal case today: the engine measures a duration but the canonical
        /// `toolResult` frame doesn't carry it, so the client falls back to its
        /// own measurement. Optional rather than `0` because "no timing" and
        /// "took no time" are different claims (pearl th-d49538).
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    TaskComplete {
        task_id: String,
        /// Agent-loop iterations. The canonical protocol carries no iteration
        /// count on `eventual_response`, so this is 0 on the WS path.
        iterations: u32,
        /// `None` when the server reported no usage for the turn.
        #[serde(default)]
        usage: Option<TurnUsage>,
    },
    TaskError {
        task_id: String,
        message: String,
    },
    PearlCreated {
        id: String,
        title: String,
    },
    /// The agent's proposed plan (Plan mode `present_plan` directive). Rendered
    /// as an accept/revise prompt; merges off `eventual_response.directive`.
    PresentPlan {
        plan: String,
    },
    /// A live task checklist (`todos` directive). Replaces the previous list.
    Todos {
        items: Vec<crate::state::TodoItem>,
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
        /// `requestId` the server echoed back, when it sent one. The read loop
        /// rewrites this to `Some(turn id)` only when the error belongs to the
        /// in-flight turn; consumers treat any other value — including `None`
        /// — as non-fatal chatter rather than a reason to abandon the turn
        /// (th-472012).
        #[serde(default)]
        request_id: Option<String>,
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
                // The server doesn't forward the engine's `duration_ms` today;
                // read it if that ever changes, and report "unknown" — never a
                // hardcoded 0 — when it doesn't (th-d49538).
                duration_ms: res.get("durationMs").or_else(|| res.get("duration_ms")).and_then(serde_json::Value::as_u64),
            })
        }
        // Token accounting + cost ride the terminal event's optional
        // `data.data.usage`. Absent for e.g. offline mock turns, in which case
        // the client must show nothing rather than claim a $0 turn.
        "eventual_response" => Some(ServerEvent::TaskComplete {
            task_id: TURN_ID.to_string(),
            iterations: 0,
            usage: v.get("data").and_then(|d| d.get("data")).and_then(|d| d.get("usage")).map(|u| TurnUsage {
                cost_usd: u.get("costUsd").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                prompt_tokens: u.get("promptTokens").and_then(serde_json::Value::as_u64).unwrap_or(0),
                completion_tokens: u.get("completionTokens").and_then(serde_json::Value::as_u64).unwrap_or(0),
            }),
        }),
        "error" => Some(ServerEvent::Error {
            message: smooth_cast::wire::error_message(v),
            request_id: v.get("requestId").and_then(serde_json::Value::as_str).map(str::to_string),
        }),
        "pong" => Some(ServerEvent::Pong),
        _ => None,
    }
}

/// Pull a `present_plan` / `todos` directive off an `eventual_response` frame.
///
/// Both ride the SAME `data.data.directive` field the `send_file` directive uses
/// (the web SPA reads the identical path — `operator.ts`). Returns `None` for
/// any other frame, a missing directive, or a directive type this face doesn't
/// render (e.g. `send_file`, which `th code` doesn't surface). Emitted as its
/// own event BEFORE the terminal `TaskComplete` so the UI applies it first.
fn directive_event(frame: &serde_json::Value) -> Option<ServerEvent> {
    if frame.get("type")?.as_str()? != "eventual_response" {
        return None;
    }
    let directive = frame.get("data")?.get("data")?.get("directive")?;
    match directive.get("type")?.as_str()? {
        "present_plan" => Some(ServerEvent::PresentPlan {
            plan: directive.get("plan")?.as_str()?.to_string(),
        }),
        "todos" => {
            let items = directive
                .get("items")?
                .as_array()?
                .iter()
                .filter_map(|it| {
                    Some(crate::state::TodoItem {
                        text: it.get("text")?.as_str()?.to_string(),
                        status: match it.get("status").and_then(serde_json::Value::as_str).unwrap_or("pending") {
                            "in_progress" => crate::state::TodoStatus::InProgress,
                            "completed" => crate::state::TodoStatus::Completed,
                            _ => crate::state::TodoStatus::Pending,
                        },
                    })
                })
                .collect();
            Some(ServerEvent::Todos { items })
        }
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
        ClientEvent::TaskStart { message, model, images, .. } => {
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
            if !images.is_empty() {
                // The engine parses `images` as `[{ url, detail? }]` objects
                // (UserImage) and fail-soft DROPS anything else — bare data-URL
                // strings were silently discarded, so the model never saw the
                // attachment. Wrap each in `{ "url": … }`.
                frame["images"] = serde_json::Value::Array(images.iter().map(|u| serde_json::json!({ "url": u })).collect());
            }
            Some(frame)
        }
        ClientEvent::Ping => Some(serde_json::json!({ "action": "ping", "requestId": request_id })),
        // The canonical protocol has no cancel/steer verb yet; dropping beats
        // sending a frame the server will reject as unknown.
        ClientEvent::TaskCancel { .. } | ClientEvent::Steer { .. } => None,
    }
}

/// The keep-alive loop, extracted from `connect` so a test can drive the code
/// the client actually runs.
///
/// pearl th-472012: this loop used to build its frame with
/// `serde_json::to_string(&ClientEvent::Ping)`, emitting the bespoke
/// `{"type":"Ping"}` instead of a canonical `{"action":"ping",…}`. The server
/// rejected it every 15 seconds with `missing 'action' field`, and that error
/// tore down whatever turn was in flight.
///
/// The existing test covered `send()` and `to_canonical_frame()` — both
/// already correct — so it sailed past the one path that was broken. Hence
/// this function exists: so the regression test drives the loop itself.
async fn heartbeat_loop(tx: mpsc::UnboundedSender<String>, connected: Arc<AtomicBool>, interval: Duration, next_request: Arc<AtomicU64>) {
    loop {
        tokio::time::sleep(interval).await;
        if !connected.load(Ordering::SeqCst) {
            break;
        }
        let request_id = format!("hb-{}", next_request.fetch_add(1, Ordering::Relaxed));
        let Some(frame) = to_canonical_frame(&ClientEvent::Ping, None, &request_id) else {
            break;
        };
        if tx.send(frame.to_string()).is_err() {
            break;
        }
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
    /// `requestId` of the in-flight turn, so the read loop can tell a failure
    /// of *this turn* from unrelated protocol chatter (a rejected heartbeat,
    /// a late error for a cancelled turn). Only a matching id is fatal —
    /// pearl th-472012, where a rejected ping tore down a turn whose answer
    /// the daemon had already produced.
    turn_request: Arc<std::sync::Mutex<Option<String>>>,
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
            turn_request: Arc::new(std::sync::Mutex::new(None)),
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
        let turn_read = Arc::clone(&self.turn_request);
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
                let Some(mut event) = translate_frame(&frame) else {
                    continue; // not a UI-bearing frame
                };
                // Attribute errors: keep `request_id` only when it names the
                // in-flight turn. Everything else — a rejected heartbeat, a
                // late error for an abandoned turn, an unattributed protocol
                // complaint — is downgraded so it can be shown without
                // discarding an answer the daemon may still be producing.
                if let ServerEvent::Error { request_id, .. } = &mut event {
                    let turn = turn_read.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if request_id.is_none() || *request_id != turn {
                        *request_id = None;
                    }
                }
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
                // A `present_plan` / `todos` directive rides the terminal
                // `eventual_response`; surface it as its own event FIRST so the
                // UI applies it before the `TaskComplete` that ends the turn.
                if let Some(dir) = directive_event(&frame) {
                    if event_tx.send(dir).is_err() {
                        break;
                    }
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
        tokio::spawn(heartbeat_loop(
            send_tx,
            Arc::clone(&self.connected),
            self.conn_mgr.config().heartbeat_interval,
            Arc::clone(&self.next_request),
        ));

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
    // ponytail: 8 args — fold into a TaskSpec struct when the 9th arrives
    // (the th-d7366d epic will add more turn fields; do the struct then).
    #[allow(clippy::too_many_arguments)]
    pub async fn run_task(
        &mut self,
        message: &str,
        model: Option<&str>,
        budget: Option<f64>,
        working_dir: Option<&str>,
        agent: Option<&str>,
        prior_messages: Vec<PriorMessage>,
        images: Vec<String>,
    ) -> anyhow::Result<mpsc::UnboundedReceiver<ServerEvent>> {
        let event = ClientEvent::TaskStart {
            message: message.to_string(),
            model: model.map(ToString::to_string),
            budget,
            working_dir: working_dir.map(ToString::to_string),
            agent: agent.map(ToString::to_string),
            prior_messages,
            images,
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
        if matches!(event, ClientEvent::TaskStart { .. }) {
            // Remember which request the turn rides on, so an error frame can
            // be attributed to it (or not) — see `turn_request`.
            *self.turn_request.lock().unwrap_or_else(|e| e.into_inner()) = Some(request_id);
        }
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
// One-shot conversation queries (pearl th-aaa53a)
// ---------------------------------------------------------------------------

/// One row for the conversation sidebar, from the daemon's canonical
/// `list_conversations` — the SAME wire the web SPA sidebar reads
/// (`operator.ts::ConversationSummary`).
#[derive(Debug, Clone)]
pub struct RemoteConversation {
    pub conversation_id: String,
    pub title: String,
    /// ISO-8601 timestamp string; parsed defensively by the caller.
    pub updated_at: String,
    pub message_count: u64,
}

/// Open a short-lived canonical WS connection to `url`, run `send_frames`,
/// and scan replies until `extract` returns a value or the timeout lapses.
/// The one-shot shape (connect → ask → parse → drop) is deliberate: sidebar
/// queries are rare and tiny, and reusing the per-turn client would tangle
/// its read loop with turn streaming.
async fn one_shot_query<T>(url: &str, send_frames: Vec<serde_json::Value>, mut extract: impl FnMut(&serde_json::Value) -> Option<T>) -> anyhow::Result<T> {
    let ws_url = url.trim_end_matches('/').replace("http://", "ws://").replace("https://", "wss://");
    let mut ws_url = format!("{ws_url}/ws");
    if let Some(token) = local_token() {
        ws_url = format!("{ws_url}?token={}", percent_encode_token(&token));
    }
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await?;
    for frame in send_frames {
        ws.send(tungstenite::Message::Text(frame.to_string().into())).await?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let msg = tokio::time::timeout_at(deadline, ws.next()).await;
        let Ok(Some(Ok(msg))) = msg else {
            anyhow::bail!("no reply from Big Smooth within 5s");
        };
        let tungstenite::Message::Text(text) = msg else { continue };
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(out) = extract(&frame) {
            let _ = ws.send(tungstenite::Message::Close(None)).await;
            return Ok(out);
        }
    }
}

/// Fetch the recent-conversation list (most-recent first, non-empty only —
/// the server filters). Sessionless: `list_conversations` needs no bound
/// session, so nothing is minted server-side by opening the sidebar.
pub async fn list_remote_conversations(url: &str) -> anyhow::Result<Vec<RemoteConversation>> {
    let frame = serde_json::json!({ "action": "list_conversations", "requestId": "th-code-lc" });
    one_shot_query(url, vec![frame], |v| {
        let convs = v.get("data")?.get("conversations")?.as_array()?;
        Some(
            convs
                .iter()
                .filter_map(|c| {
                    Some(RemoteConversation {
                        conversation_id: c.get("conversationId")?.as_str()?.to_string(),
                        title: c.get("title").and_then(serde_json::Value::as_str).unwrap_or("(untitled)").to_string(),
                        updated_at: c.get("updatedAt").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                        message_count: c.get("messageCount").and_then(serde_json::Value::as_u64).unwrap_or(0),
                    })
                })
                .collect(),
        )
    })
    .await
}

/// Fetch a conversation's stored history as oldest-first (role, text) pairs.
///
/// One socket, two rounds: resume-bind a throwaway session to the
/// conversation (a RESUME server-side — no new conversation is minted, and
/// `get_conversation_messages` is a session-scoped read), then request the
/// messages with the sessionId that reply handed back.
pub async fn fetch_conversation_history(url: &str, conversation_id: &str) -> anyhow::Result<Vec<PriorMessage>> {
    let ws_url = url.trim_end_matches('/').replace("http://", "ws://").replace("https://", "wss://");
    let mut ws_url = format!("{ws_url}/ws");
    if let Some(token) = local_token() {
        ws_url = format!("{ws_url}?token={}", percent_encode_token(&token));
    }
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await?;
    let hello = serde_json::json!({
        "action": "create_conversation_session",
        "requestId": "th-code-hist-cs",
        "agentId": uuid::Uuid::new_v4().to_string(),
        "userName": "th code",
        "conversationId": conversation_id,
    });
    ws.send(tungstenite::Message::Text(hello.to_string().into())).await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut session_id: Option<String> = None;
    let messages = loop {
        let msg = tokio::time::timeout_at(deadline, ws.next()).await;
        let Ok(Some(Ok(msg))) = msg else {
            anyhow::bail!("no reply from Big Smooth within 5s");
        };
        let tungstenite::Message::Text(text) = msg else { continue };
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(data) = frame.get("data") else { continue };

        // Round 1 reply: the bound session. Fire the history request on the
        // same socket and keep scanning.
        if session_id.is_none() {
            if let Some(sid) = data.get("sessionId").and_then(serde_json::Value::as_str) {
                session_id = Some(sid.to_string());
                let get = serde_json::json!({
                    "action": "get_conversation_messages",
                    "requestId": "th-code-hist-gm",
                    "sessionId": sid,
                    "conversationId": conversation_id,
                });
                ws.send(tungstenite::Message::Text(get.to_string().into())).await?;
                continue;
            }
        }
        // Round 2 reply: the stored messages.
        if let Some(msgs) = data.get("messages").and_then(serde_json::Value::as_array) {
            break msgs.iter().filter_map(parse_history_message).collect::<Vec<_>>();
        }
    };
    let _ = ws.send(tungstenite::Message::Close(None)).await;
    // Server returns newest-first; the transcript renders oldest-first.
    let mut messages = messages;
    messages.reverse();
    Ok(messages)
}

/// One stored domain `Message` → a transcript entry. `direction`
/// ('inbound' = user, 'outbound' = agent) with a `role` fallback; content is
/// `{items:[{text}]}`, a bare string, or a `text` field. Empty → skipped.
fn parse_history_message(m: &serde_json::Value) -> Option<PriorMessage> {
    let role = match m.get("direction").and_then(serde_json::Value::as_str) {
        Some("inbound") => "user",
        Some("outbound") => "assistant",
        _ => match m.get("role").and_then(serde_json::Value::as_str) {
            Some(r @ ("user" | "assistant")) => r,
            _ => return None,
        },
    };
    let content = match m.get("content") {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(obj) => obj
            .get("items")?
            .as_array()?
            .iter()
            .filter_map(|i| i.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        None => m.get("text")?.as_str()?.to_string(),
    };
    if content.trim().is_empty() {
        return None;
    }
    Some(PriorMessage {
        role: role.to_string(),
        content,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_history_message_maps_direction_content_and_skips_noise() {
        // Stored domain shape: direction + content.items[].text.
        let m = serde_json::json!({"direction":"inbound","content":{"items":[{"type":"text","text":"hello"},{"type":"text","text":"there"}]}});
        let p = parse_history_message(&m).unwrap();
        assert_eq!(p.role, "user");
        assert_eq!(p.content, "hello\nthere");

        let m = serde_json::json!({"direction":"outbound","content":{"items":[{"type":"text","text":"hi!"}]}});
        let p = parse_history_message(&m).unwrap();
        assert_eq!(p.role, "assistant");

        // Fallback shapes: bare-string content, role instead of direction.
        let m = serde_json::json!({"role":"assistant","content":"plain"});
        assert_eq!(parse_history_message(&m).unwrap().content, "plain");

        // Empty content and unknown roles are skipped, not rendered.
        assert!(parse_history_message(&serde_json::json!({"direction":"inbound","content":{"items":[]}})).is_none());
        assert!(parse_history_message(&serde_json::json!({"direction":"system-ish","content":"x"})).is_none());
    }

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
            images: vec![],
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
            images,
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
            assert!(images.is_empty());
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
            turn_request: Arc::new(std::sync::Mutex::new(None)),
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
            images: vec![],
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

    /// **th-d49538 regression test (durations).**
    ///
    /// The `toolResult` frame carries no timing, so the event must say so —
    /// `None`. It used to hardcode `duration_ms: 0`, and the TUI dutifully
    /// rendered `0.0s` for every tool in the transcript; the agent then read
    /// its own screen and invented hangs to explain the zeros.
    #[test]
    fn tool_result_without_timing_reports_unknown_not_zero() {
        let done = translate_frame(&json!({
            "type": "stream_chunk",
            "data": {"state": {"rawResponse": {"toolResult": {"name":"bash","result":"ok","isError":false}}}}
        }));
        match done {
            Some(ServerEvent::ToolCallComplete { duration_ms, .. }) => {
                assert_eq!(duration_ms, None, "absent timing must be None, never Some(0)");
            }
            other => panic!("expected ToolCallComplete, got {other:?}"),
        }
    }

    /// …and a server that *does* send timing must be believed over the
    /// client's own measurement.
    #[test]
    fn tool_result_timing_is_read_when_present() {
        let done = translate_frame(&json!({
            "type": "stream_chunk",
            "data": {"state": {"rawResponse": {"toolResult": {"name":"bash","result":"ok","durationMs": 1234}}}}
        }));
        match done {
            Some(ServerEvent::ToolCallComplete { duration_ms, .. }) => assert_eq!(duration_ms, Some(1234)),
            other => panic!("expected ToolCallComplete, got {other:?}"),
        }
    }

    /// **th-d49538 regression test (cost + tokens).**
    ///
    /// Usage rides `data.data.usage` on the terminal event. The client used to
    /// hardcode `cost_usd: 0.0` and never look, so the status bar read
    /// `0 tok · $0` for the life of the session no matter what was spent.
    #[test]
    fn eventual_response_usage_is_read_off_the_wire() {
        let ev = translate_frame(&json!({
            "type": "eventual_response",
            "status": 200,
            "data": {
                "status": 200,
                "data": {
                    "messageId": "m-1",
                    "response": "hi",
                    "usage": { "costUsd": 0.0421, "promptTokens": 12_000, "completionTokens": 900 }
                }
            }
        }));
        match ev {
            Some(ServerEvent::TaskComplete { usage: Some(u), .. }) => {
                assert!((u.cost_usd - 0.0421).abs() < f64::EPSILON, "cost must come from the wire: {u:?}");
                assert_eq!(u.prompt_tokens, 12_000);
                assert_eq!(u.completion_tokens, 900);
            }
            other => panic!("expected TaskComplete with usage, got {other:?}"),
        }
    }

    /// A turn the engine didn't account for reports *nothing*, so the UI can
    /// stay silent instead of claiming a free turn.
    #[test]
    fn eventual_response_without_usage_reports_none() {
        let ev = translate_frame(&json!({
            "type": "eventual_response",
            "status": 200,
            "data": { "status": 200, "data": { "messageId": "m-1", "response": "hi" } }
        }));
        match ev {
            Some(ServerEvent::TaskComplete { usage, .. }) => assert_eq!(usage, None),
            other => panic!("expected TaskComplete, got {other:?}"),
        }
    }

    #[test]
    fn unknown_frames_are_ignored() {
        assert!(translate_frame(&json!({"type":"otp_sent"})).is_none());
        assert!(translate_frame(&json!({"nope":1})).is_none());
    }

    #[test]
    fn present_plan_directive_yields_a_plan_event() {
        let ev = directive_event(&json!({
            "type": "eventual_response",
            "data": { "data": { "directive": { "type": "present_plan", "plan": "1. do X\n2. do Y" } } }
        }));
        match ev {
            Some(ServerEvent::PresentPlan { plan }) => assert_eq!(plan, "1. do X\n2. do Y"),
            other => panic!("expected PresentPlan, got {other:?}"),
        }
    }

    #[test]
    fn todos_directive_maps_every_status() {
        let ev = directive_event(&json!({
            "type": "eventual_response",
            "data": { "data": { "directive": { "type": "todos", "items": [
                { "text": "a", "status": "completed" },
                { "text": "b", "status": "in_progress" },
                { "text": "c", "status": "pending" },
                { "text": "d" }
            ] } } }
        }));
        match ev {
            Some(ServerEvent::Todos { items }) => {
                use crate::state::TodoStatus;
                assert_eq!(items.len(), 4);
                assert_eq!(items[0].status, TodoStatus::Completed);
                assert_eq!(items[1].status, TodoStatus::InProgress);
                assert_eq!(items[2].status, TodoStatus::Pending);
                // Missing status defaults to pending.
                assert_eq!(items[3].status, TodoStatus::Pending);
                assert_eq!(items[3].text, "d");
            }
            other => panic!("expected Todos, got {other:?}"),
        }
    }

    #[test]
    fn directive_event_ignores_send_file_and_non_terminal_frames() {
        // send_file is a directive `th code` doesn't surface here.
        assert!(directive_event(&json!({
            "type": "eventual_response",
            "data": { "data": { "directive": { "type": "send_file", "files": [] } } }
        }))
        .is_none());
        // No directive present.
        assert!(directive_event(&json!({ "type": "eventual_response", "data": { "data": {} } })).is_none());
        // Not a terminal frame.
        assert!(directive_event(&json!({ "type": "stream_token", "token": "x" })).is_none());
    }

    /// **The th-472012 regression test.**
    ///
    /// Drives `heartbeat_loop` — the actual code `connect` spawns — rather
    /// than `to_canonical_frame`, which was already correct while the
    /// heartbeat was broken. Asserting on the helper would have passed
    /// throughout the outage; only the loop's real output proves the fix.
    ///
    /// What went wrong: the loop emitted `{"type":"Ping"}`, the canonical
    /// server answered `VALIDATION_ERROR / missing 'action' field` every 15s,
    /// and that error killed any turn still running.
    #[tokio::test]
    async fn heartbeat_loop_emits_canonical_ping_frames() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let connected = Arc::new(AtomicBool::new(true));
        let handle = tokio::spawn(heartbeat_loop(
            tx,
            Arc::clone(&connected),
            Duration::from_millis(5),
            Arc::new(AtomicU64::new(1)),
        ));

        let raw = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("heartbeat should fire within the timeout")
            .expect("heartbeat should send a frame");
        let frame: serde_json::Value = serde_json::from_str(&raw).expect("heartbeat frame is valid JSON");

        assert_eq!(frame["action"], "ping", "the server rejects any frame without `action`: {raw}");
        assert!(frame.get("type").is_none(), "the bespoke {{\"type\":\"Ping\"}} shape must not come back: {raw}");
        assert!(
            frame["requestId"].as_str().is_some_and(|id| id.starts_with("hb-")),
            "heartbeats carry their own request id so their errors are attributable: {raw}"
        );

        // The loop must also stop once the connection is marked down.
        connected.store(false, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("loop should exit after disconnect")
            .expect("loop should not panic");
    }

    /// An error frame is only fatal to the turn it names. The read loop clears
    /// `request_id` for anything else, and `translate_frame` must surface the
    /// id in the first place for that check to be possible.
    #[test]
    fn error_frames_carry_their_request_id() {
        let attributed = translate_frame(&json!({
            "type": "error",
            "requestId": "turn-3",
            "error": { "code": "TOOL_FAILED", "message": "boom" },
        }));
        match attributed {
            Some(ServerEvent::Error { message, request_id }) => {
                assert_eq!(request_id.as_deref(), Some("turn-3"));
                assert_eq!(message, "TOOL_FAILED: boom", "the real reason, not \"unknown error\"");
            }
            other => panic!("expected Error, got {other:?}"),
        }

        // A rejected heartbeat: the server had no requestId to echo.
        let unattributed = translate_frame(&json!({
            "type": "error",
            "error": { "code": "VALIDATION_ERROR", "message": "missing 'action' field" },
        }));
        match unattributed {
            Some(ServerEvent::Error { request_id, .. }) => assert!(request_id.is_none()),
            other => panic!("expected Error, got {other:?}"),
        }
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
            images: vec![],
        };
        let frame = to_canonical_frame(&ev, Some("sess-9"), "turn-1").expect("frame");
        assert_eq!(frame["action"], "send_message");
        assert_eq!(frame["sessionId"], "sess-9");
        assert_eq!(frame["message"], "hi");
        assert_eq!(frame["model"], "smooth-coding");
        assert_eq!(frame["requestId"], "turn-1");
        // No session yet -> nothing goes on the wire.
        assert!(to_canonical_frame(&ev, None, "turn-2").is_none());
        // Text-only turn: no `images` key at all — wire parity with the web
        // composer, which omits the field rather than sending [].
        assert!(frame.get("images").is_none());
    }

    #[test]
    fn task_start_with_attachments_carries_images_like_the_web_spa() {
        let ev = ClientEvent::TaskStart {
            message: "what is in this screenshot?".into(),
            model: None,
            budget: None,
            working_dir: None,
            agent: None,
            prior_messages: vec![],
            images: vec!["data:image/png;base64,AAAA".into()],
        };
        let frame = to_canonical_frame(&ev, Some("sess-9"), "turn-1").expect("frame");
        // Engine UserImage shape: `{ url }` objects, not bare strings (bare
        // strings fail-soft-drop and the model never sees the image).
        assert_eq!(frame["images"][0]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn cancel_and_steer_are_not_invented_on_the_wire() {
        let cancel = ClientEvent::TaskCancel { task_id: "t".into() };
        assert!(to_canonical_frame(&cancel, Some("s"), "r").is_none());
        let ping = to_canonical_frame(&ClientEvent::Ping, Some("s"), "r-1").expect("ping");
        assert_eq!(ping["action"], "ping");
    }
}

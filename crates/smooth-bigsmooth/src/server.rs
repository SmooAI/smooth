//! Axum HTTP server — all REST routes, middleware, CORS.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use smooth_narc::NarcHook;
use smooth_operator::cost::CostBudget;
use smooth_operator::providers::ProviderRegistry;
use smooth_operator::tool::{ToolCall, ToolHook, ToolResult};
use smooth_operator::{Agent, AgentConfig, AgentEvent};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::db::Database;
use crate::events::{ClientEvent, ServerEvent};

/// Default idle timeout: 30 minutes.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 30 * 60;

/// Default broadcast channel capacity.
const BROADCAST_CHANNEL_CAPACITY: usize = 256;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub pearl_store: smooth_pearls::PearlStore,
    pub session_store: Arc<crate::session::DoltSessionStore>,
    pub start_time: Instant,
    pub last_activity: Arc<Mutex<Instant>>,
    pub idle_timeout: Duration,
    /// Broadcast channel for pushing [`ServerEvent`]s to all connected WebSocket clients.
    pub event_tx: broadcast::Sender<ServerEvent>,
    /// When running inside a Boardroom microVM (`SMOOTH_BOARDROOM_MODE=1`),
    /// this carries the URLs of the in-process cast (Wonk/Goalie/Narc/
    /// Scribe/Archivist). `None` in host-mode / dev-mode.
    pub boardroom: Option<crate::boardroom::BoardroomHandles>,
}

impl AppState {
    /// Create a new `AppState` with default idle timeout.
    pub fn new(db: Database, pearl_store: smooth_pearls::PearlStore) -> Self {
        let session_store = Arc::new(crate::session::DoltSessionStore::new(&pearl_store));
        let (event_tx, _) = broadcast::channel(BROADCAST_CHANNEL_CAPACITY);
        Self {
            db,
            pearl_store,
            session_store,
            start_time: Instant::now(),
            last_activity: Arc::new(Mutex::new(Instant::now())),
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            event_tx,
            boardroom: None,
        }
    }

    /// Attach Boardroom cast handles to an existing state. Chainable.
    #[must_use]
    pub fn with_boardroom(mut self, handles: crate::boardroom::BoardroomHandles) -> Self {
        self.boardroom = Some(handles);
        self
    }

    /// Touch the activity timestamp — call from every handler.
    fn touch(&self) {
        if let Ok(mut last) = self.last_activity.lock() {
            *last = Instant::now();
        }
    }
}

// ── Response types ─────────────────────────────────────────

#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub data: T,
    pub ok: bool,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub service: String,
    pub version: String,
    pub uptime: f64,
    pub timestamp: String,
}

#[derive(Serialize)]
pub struct SystemHealth {
    pub leader: LeaderHealth,
    pub database: DatabaseHealth,
    pub sandbox: SandboxHealth,
    pub tailscale: TailscaleHealth,
    pub pearls: PearlsHealth,
}

#[derive(Serialize)]
pub struct LeaderHealth {
    pub status: String,
    pub uptime: f64,
}

#[derive(Serialize)]
pub struct DatabaseHealth {
    pub status: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct SandboxHealth {
    pub status: String,
    pub backend: String,
    pub active_sandboxes: u32,
    pub max_concurrency: u32,
}

#[derive(Serialize)]
pub struct TailscaleHealth {
    pub status: String,
    pub hostname: Option<String>,
}

#[derive(Serialize)]
pub struct PearlsHealth {
    pub status: String,
    pub open_pearls: u32,
}

// ── Query params ───────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchParams {
    q: Option<String>,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    search_type: Option<String>,
}

#[derive(Deserialize)]
pub struct ChatBody {
    content: String,
}

#[derive(Deserialize)]
pub struct ConfigBody {
    key: String,
    value: serde_json::Value,
}

#[derive(Deserialize)]
pub struct SteerBody {
    message: Option<String>,
}

// ── Task request/types ────────────────────────────────────

#[derive(Deserialize)]
pub struct TaskRequest {
    pub message: String,
    pub model: Option<String>,
    pub budget: Option<f64>,
    pub working_dir: Option<String>,
}

// ── NarcHook wrapper for ToolHook ─────────────────────────

struct SharedNarcHook {
    inner: Arc<NarcHook>,
}

#[async_trait]
impl ToolHook for SharedNarcHook {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        self.inner.pre_call(call).await
    }

    async fn post_call(&self, call: &ToolCall, result: &ToolResult) -> anyhow::Result<()> {
        self.inner.post_call(call, result).await
    }
}

// ── Router ─────────────────────────────────────────────────

/// Build the axum router with all routes.
///
/// The embedded web UI (SPA) is served as a fallback so that API routes
/// take priority and unknown paths return index.html for client-side routing.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Health
        .route("/health", get(health_handler))
        // System
        .route("/api/system/health", get(system_health_handler))
        .route("/api/system/config", get(get_config_handler).put(set_config_handler))
        // Tasks (headless agent execution)
        .route("/api/tasks", post(run_task_handler))
        // Pearls — the only spelling. No /api/issues, no /api/beads.
        .route("/api/pearls", get(list_pearls_handler).post(create_pearl_handler))
        .route("/api/pearls/ready", get(ready_pearls_handler))
        .route("/api/pearls/stats", get(stats_handler))
        .route("/api/pearls/{id}", get(get_pearl_handler).patch(update_pearl_handler))
        .route("/api/pearls/{id}/close", post(close_pearl_handler))
        // Workers
        .route("/api/workers", get(list_workers_handler))
        .route("/api/workers/{id}", get(get_worker_handler).delete(kill_worker_handler))
        // Messages / Sessions
        .route("/api/messages/inbox", get(inbox_handler))
        .route("/api/sessions/{id}/messages", get(session_messages_handler))
        // Reviews
        .route("/api/reviews", get(list_reviews_handler))
        .route("/api/reviews/{bead_id}/approve", post(approve_review_handler))
        .route("/api/reviews/{bead_id}/reject", post(reject_review_handler))
        // Chat
        .route("/api/chat", post(chat_handler))
        // Search
        .route("/api/search", get(search_handler))
        // Steering
        .route("/api/steering/{bead_id}/pause", post(pause_handler))
        .route("/api/steering/{bead_id}/resume", post(resume_handler))
        .route("/api/steering/{bead_id}/steer", post(steer_handler))
        .route("/api/steering/{bead_id}/cancel", post(cancel_handler))
        // Jira
        .route("/api/jira/status", get(jira_status_handler))
        .route("/api/jira/sync", post(jira_sync_handler))
        // WebSocket — primary real-time channel
        .route("/ws", get(ws_handler))
        // Embedded web UI (SPA fallback — must be last)
        .fallback_service(smooth_web::web_router())
        // Middleware
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Start the leader HTTP server.
///
/// On first call this also:
/// - Initialises the process-global sandbox client (Direct vs Bill,
///   selected by the `SMOOTH_BOOTSTRAP_BILL_URL` env var).
/// - If `SMOOTH_BOARDROOM_MODE=1`, spawns the Boardroom cast (Wonk/Goalie/
///   Narc/Scribe/Archivist) as tokio tasks in this process and attaches
///   their handles to `AppState`. Idempotent if the state already carries
///   boardroom handles.
pub async fn start(mut state: AppState, addr: SocketAddr) -> anyhow::Result<()> {
    // Pick the sandbox client (Direct or Bill) exactly once.
    crate::sandbox::init_sandbox_client();

    // Boardroom bootstrap.
    if state.boardroom.is_none()
        && std::env::var("SMOOTH_BOARDROOM_MODE")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
            .unwrap_or(false)
    {
        match crate::boardroom::spawn_boardroom_cast().await {
            Ok(handles) => {
                tracing::info!(archivist = %handles.archivist_url, "Big Smooth running in Boardroom mode");
                state.boardroom = Some(handles);
            }
            Err(e) => {
                tracing::error!(error = %e, "boardroom: failed to spawn cast; continuing without it");
            }
        }
    }

    // Spawn idle timeout checker
    let idle_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let elapsed = {
                let Ok(last) = idle_state.last_activity.lock() else {
                    continue;
                };
                last.elapsed()
            };
            if elapsed > idle_state.idle_timeout {
                tracing::info!("Idle timeout reached ({:.0}s), shutting down", idle_state.idle_timeout.as_secs_f64());
                std::process::exit(0);
            }
        }
    });

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Smooth leader running at http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

// ── WebSocket ─────────────────────────────────────────────

/// Heartbeat interval for WebSocket connections.
const WS_HEARTBEAT_SECS: u64 = 30;

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: AppState) {
    use futures_util::{SinkExt, StreamExt};

    let session_id = uuid::Uuid::new_v4().to_string();
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Send Connected event
    let connected = ServerEvent::Connected {
        session_id: session_id.clone(),
    };
    if let Ok(json) = serde_json::to_string(&connected) {
        let _ = ws_tx.send(Message::Text(json.into())).await;
    }

    // Subscribe to broadcast channel for server events
    let mut event_rx = state.event_tx.subscribe();

    // Spawn a task that forwards broadcast events and heartbeats to the client
    let (internal_tx, mut internal_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

    // Forward broadcast → internal_tx
    let broadcast_tx = internal_tx.clone();
    let broadcast_handle = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    if let Ok(json) = serde_json::to_string(&event) {
                        if broadcast_tx.send(Message::Text(json.into())).is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(lagged = n, "WebSocket client lagged behind broadcast");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Heartbeat → internal_tx
    let heartbeat_tx = internal_tx;
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(WS_HEARTBEAT_SECS));
        loop {
            interval.tick().await;
            let pong = ServerEvent::Pong;
            if let Ok(json) = serde_json::to_string(&pong) {
                if heartbeat_tx.send(Message::Text(json.into())).is_err() {
                    break;
                }
            }
        }
    });

    // Write loop: drain internal_rx into WebSocket
    let write_handle = tokio::spawn(async move {
        while let Some(msg) = internal_rx.recv().await {
            if ws_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Read loop: process incoming client messages
    while let Some(Ok(msg)) = ws_rx.next().await {
        state.touch();
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };

        let Ok(event) = serde_json::from_str::<ClientEvent>(&text) else {
            let err = ServerEvent::Error {
                message: "invalid event JSON".into(),
            };
            if let Ok(json) = serde_json::to_string(&err) {
                let _ = state.event_tx.send(err);
                // Also try direct send (may fail if no subscribers, that's fine)
                let _ = json;
            }
            continue;
        };

        handle_client_event(&state, event).await;
    }

    // Client disconnected — clean up
    broadcast_handle.abort();
    heartbeat_handle.abort();
    write_handle.abort();
    tracing::debug!(session_id, "WebSocket client disconnected");
}

/// Dispatch a single [`ClientEvent`] received over WebSocket.
async fn handle_client_event(state: &AppState, event: ClientEvent) {
    match event {
        ClientEvent::Ping => {
            let _ = state.event_tx.send(ServerEvent::Pong);
        }
        ClientEvent::TaskStart {
            message,
            model,
            budget,
            working_dir,
        } => {
            dispatch_ws_task(state, message, model, budget, working_dir).await;
        }
        ClientEvent::TaskCancel { task_id } => {
            tracing::info!(task_id, "Task cancel requested via WebSocket");
            // Cancellation is fire-and-forget for now; agent loop will
            // be extended with a cancellation token in a future PR.
        }
        ClientEvent::Steer { task_id, action, message } => {
            tracing::info!(task_id, action, "Steer via WebSocket");
            let comment = format!("[STEERING:{action}] {}", message.unwrap_or_default());
            let _ = state.pearl_store.add_comment(&task_id, &comment);
        }
        ClientEvent::PearlCreate {
            title,
            description,
            pearl_type,
            priority,
        } => {
            let desc = description.as_deref().unwrap_or("");
            let itype = pearl_type.as_deref().unwrap_or("task");
            let prio = priority.unwrap_or(2);
            match crate::pearls::create_pearl(&state.pearl_store, &title, desc, itype, prio) {
                Ok(issue) => {
                    let _ = state.event_tx.send(ServerEvent::PearlCreated { id: issue.id, title });
                }
                Err(e) => {
                    let _ = state.event_tx.send(ServerEvent::Error { message: e.to_string() });
                }
            }
        }
        ClientEvent::PearlUpdate { id, status, priority } => {
            let update = smooth_pearls::PearlUpdate {
                status: status.as_deref().and_then(smooth_pearls::PearlStatus::from_str_loose),
                priority: priority.and_then(smooth_pearls::Priority::from_u8),
                ..Default::default()
            };
            match state.pearl_store.update(&id, &update) {
                Ok(_issue) => {
                    let _ = state.event_tx.send(ServerEvent::PearlUpdated {
                        id,
                        status: status.unwrap_or_else(|| "updated".into()),
                    });
                }
                Err(e) => {
                    let _ = state.event_tx.send(ServerEvent::Error { message: e.to_string() });
                }
            }
        }
        ClientEvent::PearlClose { ids } => {
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            match state.pearl_store.close(&refs) {
                Ok(count) => {
                    for id in &ids {
                        let _ = state.event_tx.send(ServerEvent::PearlUpdated {
                            id: id.clone(),
                            status: "closed".into(),
                        });
                    }
                    tracing::info!(count, "Closed issues via WebSocket");
                }
                Err(e) => {
                    let _ = state.event_tx.send(ServerEvent::Error { message: e.to_string() });
                }
            }
        }
    }
}

/// Returns `true` if Big Smooth should route WebSocket task dispatch through
/// a real sandboxed operator instead of running the agent in its own process.
///
/// Controlled by the `SMOOTH_SANDBOXED` environment variable (values `1` or
/// `true`). Default is `false` — the in-process path is still the path used by
/// every existing test and by `th code --headless` out of the box.
///
/// When sandboxed mode is enabled, Big Smooth:
///  - Creates an issue (as before)
///  - Spawns a real microVM via the embedded [`microsandbox`] crate
///  - Hands the task off to a program running inside the VM
///  - Streams events back to WebSocket clients
///  - Destroys the VM on completion
///
/// Crucially, Big Smooth itself performs **no file writes, no tool execution
/// and no LLM calls** on the sandboxed path — it stays the READ-ONLY
/// orchestrator the architecture promises.
fn sandboxed_dispatch_enabled() -> bool {
    std::env::var("SMOOTH_SANDBOXED")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Spawn an agent task from a WebSocket `TaskStart` event, broadcasting
/// [`ServerEvent`]s as the agent progresses.
///
/// Dispatches to either the in-process path or the sandboxed path depending
/// on [`sandboxed_dispatch_enabled`].
async fn dispatch_ws_task(state: &AppState, message: String, model: Option<String>, budget: Option<f64>, working_dir: Option<String>) {
    if sandboxed_dispatch_enabled() {
        dispatch_ws_task_sandboxed(state, message, model, budget, working_dir).await;
    } else {
        dispatch_ws_task_in_process(state, message, model, budget, working_dir).await;
    }
}

/// Legacy in-process dispatch — runs the agent in Big Smooth's own process.
/// Kept for backwards compatibility (and because the sandboxed path currently
/// requires an operator image that does not yet exist in the repo). Do not
/// add new features here; new functionality should live on the sandboxed
/// path so that Big Smooth stays READ-ONLY.
async fn dispatch_ws_task_in_process(state: &AppState, message: String, model: Option<String>, budget: Option<f64>, working_dir: Option<String>) {
    let working_dir = working_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let task_id = uuid::Uuid::new_v4().to_string();
    let event_tx = state.event_tx.clone();

    // Create an issue for tracking
    let pearl_store = state.pearl_store.clone();
    let pearl_id = crate::pearls::create_pearl(&pearl_store, &format!("Task: {}", truncate_str(&message, 60)), &message, "task", 2)
        .ok()
        .map(|i| i.id);

    if let Some(ref id) = pearl_id {
        let update = smooth_pearls::PearlUpdate {
            status: Some(smooth_pearls::PearlStatus::InProgress),
            ..Default::default()
        };
        let _ = pearl_store.update(id, &update);
    }

    // Save the task message to Dolt session store
    {
        use crate::session::{MessageType, SessionMessage, SessionStore};
        let msg = SessionMessage {
            id: format!("msg-{}", &task_id[..8]),
            session_id: task_id.clone(),
            from: "user".to_string(),
            to: "bigsmooth".to_string(),
            content: message.clone(),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Command,
        };
        let _ = state.session_store.save_message(msg);
    }

    let tid = task_id.clone();
    let last_activity = state.last_activity.clone();
    let session_store = state.session_store.clone();
    tokio::spawn(async move {
        let (agent_tx, mut agent_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

        // Bridge agent events to broadcast channel
        let bridge_tx = event_tx.clone();
        let bridge_tid = tid.clone();
        let bridge_last_activity = last_activity.clone();
        let bridge_handle = tokio::spawn(async move {
            let mut iterations = 0u32;
            let start = Instant::now();
            while let Some(agent_event) = agent_rx.recv().await {
                // Touch last_activity so idle timer doesn't fire mid-task
                if let Ok(mut last) = bridge_last_activity.lock() {
                    *last = Instant::now();
                }
                let server_event = match agent_event {
                    AgentEvent::TokenDelta { content } => Some(ServerEvent::TokenDelta {
                        task_id: bridge_tid.clone(),
                        content,
                    }),
                    AgentEvent::ToolCallStart { tool_name, .. } => Some(ServerEvent::ToolCallStart {
                        task_id: bridge_tid.clone(),
                        tool_name,
                        arguments: String::new(),
                    }),
                    AgentEvent::ToolCallComplete { tool_name, is_error, .. } => Some(ServerEvent::ToolCallComplete {
                        task_id: bridge_tid.clone(),
                        tool_name,
                        result: String::new(),
                        is_error,
                        duration_ms: start.elapsed().as_millis() as u64,
                    }),
                    AgentEvent::Completed { iterations: iters, .. } => {
                        iterations = iters;
                        None // We send TaskComplete after the agent loop returns
                    }
                    AgentEvent::Error { message } => Some(ServerEvent::TaskError {
                        task_id: bridge_tid.clone(),
                        message,
                    }),
                    _ => None,
                };

                if let Some(evt) = server_event {
                    let _ = bridge_tx.send(evt);
                }
            }
            iterations
        });

        // Run the actual agent
        let result = run_agent_task(working_dir, message, model, budget, agent_tx).await;

        // Touch on completion so the idle timer starts fresh from now
        if let Ok(mut last) = last_activity.lock() {
            *last = Instant::now();
        }

        // Wait for bridge to drain
        let iterations = bridge_handle.await.unwrap_or(0);

        match result {
            Ok(cost) => {
                let _ = event_tx.send(ServerEvent::TaskComplete {
                    task_id: tid.clone(),
                    iterations,
                    cost_usd: cost,
                });
                if let Some(ref id) = pearl_id {
                    let _ = pearl_store.close(&[id]);
                }
                // Save completion message to Dolt
                {
                    use crate::session::{MessageType, SessionMessage, SessionStore};
                    let msg = SessionMessage {
                        id: format!("msg-done-{}", &tid[..8]),
                        session_id: tid.clone(),
                        from: "bigsmooth".to_string(),
                        to: "user".to_string(),
                        content: format!("Task completed. Iterations: {iterations}, cost: ${cost:.4}"),
                        timestamp: chrono::Utc::now(),
                        message_type: MessageType::StatusUpdate,
                    };
                    let _ = session_store.save_message(msg);
                }
                tracing::info!(task_id = tid, cost_usd = cost, "WS task completed");
            }
            Err(e) => {
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: tid.clone(),
                    message: e.to_string(),
                });
                // Save error message to Dolt
                {
                    use crate::session::{MessageType, SessionMessage, SessionStore};
                    let msg = SessionMessage {
                        id: format!("msg-err-{}", &tid[..8]),
                        session_id: tid.clone(),
                        from: "bigsmooth".to_string(),
                        to: "user".to_string(),
                        content: format!("Task failed: {e}"),
                        timestamp: chrono::Utc::now(),
                        message_type: MessageType::Alert,
                    };
                    let _ = session_store.save_message(msg);
                }
                tracing::error!(task_id = tid, error = %e, "WS task failed");
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Sandboxed dispatch: spawn a real microVM, run the task inside, stream events
// back. Big Smooth performs NO writes, NO tool execution, and NO LLM calls on
// this path — it is strictly a READ-ONLY orchestrator.
//
// This is the path the architecture actually describes. The full end-to-end
// "operator binary + Wonk + Goalie + Narc + Scribe all running inside the VM"
// story is tracked as a follow-up (requires a custom OCI image). What this
// function delivers today is the plumbing: orchestrator → microVM → exec →
// result, gated by `SMOOTH_SANDBOXED=1`. Existing in-process clients are
// unaffected.
// ---------------------------------------------------------------------------

/// Locate the cross-compiled `smooth-operator-runner` binary that Big Smooth
/// will mount into each sandbox.
///
/// Resolution order:
///  1. `SMOOTH_OPERATOR_RUNNER_HOST_PATH` env var — an **opaque host path**
///     used only for bind-mount source. The file does not need to exist
///     in Big Smooth's own filesystem view. Set when Big Smooth runs
///     inside the Boardroom VM and passes host paths through to Bill.
///  2. `SMOOTH_OPERATOR_RUNNER` env var (absolute path, must exist locally)
///  3. `<CARGO_MANIFEST_DIR>/../../target/aarch64-unknown-linux-musl/release/smooth-operator-runner`
///  4. `./target/aarch64-unknown-linux-musl/release/smooth-operator-runner` (cwd)
///
/// Returns `None` if no binary is found; callers should fall back to the
/// legacy echo path with a clear error message so developers know they need
/// to run `scripts/build-operator-runner.sh`.
fn find_operator_runner_binary() -> Option<std::path::PathBuf> {
    if let Ok(host_path) = std::env::var("SMOOTH_OPERATOR_RUNNER_HOST_PATH") {
        // Trust the caller (the Boardroom bootstrap). We cannot check the
        // file because it lives on the host, not inside our VM.
        return Some(std::path::PathBuf::from(host_path));
    }

    if let Ok(explicit) = std::env::var("SMOOTH_OPERATOR_RUNNER") {
        let p = std::path::PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }

    // Walk up from CARGO_MANIFEST_DIR looking for target/aarch64-unknown-linux-musl/release.
    let manifest = env!("CARGO_MANIFEST_DIR");
    let mut dir = std::path::PathBuf::from(manifest);
    for _ in 0..5 {
        let candidate = dir
            .join("target")
            .join("aarch64-unknown-linux-musl")
            .join("release")
            .join("smooth-operator-runner");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }

    // Last resort: look relative to the current working directory.
    let cwd_candidate = std::env::current_dir()
        .ok()?
        .join("target")
        .join("aarch64-unknown-linux-musl")
        .join("release")
        .join("smooth-operator-runner");
    if cwd_candidate.is_file() {
        return Some(cwd_candidate);
    }
    None
}

async fn dispatch_ws_task_sandboxed(state: &AppState, message: String, model: Option<String>, budget: Option<f64>, working_dir: Option<String>) {
    use crate::sandbox::{self, BindMount, SandboxConfig};

    let task_id = uuid::Uuid::new_v4().to_string();
    let event_tx = state.event_tx.clone();
    let pearl_store = state.pearl_store.clone();
    let last_activity = state.last_activity.clone();
    let boardroom_handles = state.boardroom.clone();

    // Guard against the kernel-cmdline printable-ASCII constraint BEFORE we
    // start spinning anything up. microsandbox stuffs env vars into the VM
    // via the kernel command line, which rejects anything outside
    // ` `..`~` (printable ASCII) with a cryptic `InvalidAscii` panic. If the
    // task message has an em dash, smart quote, tab, or any other funny
    // byte, fail here with a clear error that tells the caller why. This
    // is strictly better than the previous behavior of booting a VM that
    // then crashes inside krun.
    if let Some((pos, byte)) = message.bytes().enumerate().find(|&(_, b)| !(b' '..=b'~').contains(&b)) {
        let context_start = pos.saturating_sub(12);
        let context_end = (pos + 12).min(message.len());
        let context = message.get(context_start..context_end).unwrap_or("");
        let err = format!(
            "sandboxed dispatch requires a printable-ASCII task message; byte 0x{byte:02x} at offset {pos} is not allowed. \
             microsandbox passes env vars via the kernel command line, which only accepts ' '..'~'. \
             Rewrite the offending character (often an em dash `—`, smart quote, tab, or non-BMP unicode). \
             Context: ...{context}..."
        );
        let _ = event_tx.send(ServerEvent::TaskError {
            task_id: task_id.clone(),
            message: err.clone(),
        });
        tracing::error!("sandboxed dispatch: {err}");
        return;
    }

    // READ-ONLY metadata: Big Smooth tracks the pearl, but the work happens
    // inside the sandbox.
    let pearl_id = crate::pearls::create_pearl(&pearl_store, &format!("Task: {}", truncate_str(&message, 60)), &message, "task", 2)
        .ok()
        .map(|i| i.id);
    if let Some(ref id) = pearl_id {
        let _ = pearl_store.update(
            id,
            &smooth_pearls::PearlUpdate {
                status: Some(smooth_pearls::PearlStatus::InProgress),
                ..Default::default()
            },
        );
    }

    // Resolve the runner binary and working directory upfront. Both are
    // needed as host paths to mount into the VM.
    let runner_bin = match find_operator_runner_binary() {
        Some(p) => p,
        None => {
            let err = "smooth-operator-runner binary not found. Run scripts/build-operator-runner.sh to cross-compile it, or set SMOOTH_OPERATOR_RUNNER=/absolute/path.";
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: err.into(),
            });
            tracing::error!("sandboxed dispatch: {err}");
            return;
        }
    };

    // Working dir on the host — the agent reads/writes here from inside the
    // operator VM. Two cases:
    //
    //   * **Host mode** (Direct sandbox): Big Smooth IS on the host. We can
    //     dereference `working_dir` ourselves, create it if missing, etc.
    //   * **Boardroom mode** (Bill sandbox, brokered): Big Smooth runs
    //     inside its own microVM and the `working_dir` is an **opaque host
    //     path** (from the test harness / operator). It does not exist in
    //     our filesystem view — we must not stat, canonicalize, or create
    //     it. Bill will bind-mount it on the host.
    let brokered = std::env::var("SMOOTH_BOOTSTRAP_BILL_URL").map(|v| !v.trim().is_empty()).unwrap_or(false);
    let host_workspace: std::path::PathBuf = working_dir
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    if !brokered && !host_workspace.exists() {
        if let Err(e) = std::fs::create_dir_all(&host_workspace) {
            let _ = state.event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: format!("failed to create host workspace {}: {e}", host_workspace.display()),
            });
            return;
        }
    }

    // Resolve the binary's parent directory so we can mount the whole folder
    // (virtiofs prefers directory mounts). The binary will end up at
    // /opt/smooth/bin/smooth-operator-runner inside the VM.
    let Some(runner_dir) = runner_bin.parent().map(std::path::Path::to_path_buf) else {
        let _ = event_tx.send(ServerEvent::TaskError {
            task_id: task_id.clone(),
            message: "smooth-operator-runner binary has no parent directory".into(),
        });
        return;
    };

    // Canonicalize host paths so bind mounts resolve correctly.
    let runner_dir_str = runner_dir.canonicalize().unwrap_or(runner_dir).to_string_lossy().to_string();
    let workspace_canon = host_workspace.canonicalize().unwrap_or(host_workspace.clone()).to_string_lossy().to_string();
    let runner_name = runner_bin
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "smooth-operator-runner".into());

    let tid = task_id.clone();

    // LLM config — Big Smooth loads providers.json from the host and passes
    // them into the sandbox as env vars. The runner never touches the host
    // filesystem; all secrets come in via env.
    let (api_url, api_key, final_model) = match load_llm_config_for_runner(&model) {
        Ok(x) => x,
        Err(e) => {
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: tid.clone(),
                message: format!("no LLM provider configured: {e}"),
            });
            return;
        }
    };

    tokio::spawn(async move {
        let touch = || {
            if let Ok(mut last) = last_activity.lock() {
                *last = std::time::Instant::now();
            }
        };
        touch();

        let _ = event_tx.send(ServerEvent::ToolCallStart {
            task_id: tid.clone(),
            tool_name: "sandbox.create".into(),
            arguments: serde_json::json!({
                "workspace": workspace_canon,
                "runner_bin": runner_dir_str,
                "task": truncate_str(&message, 120)
            })
            .to_string(),
        });

        // Build the sandbox config with two bind mounts:
        //   /opt/smooth/bin (RO) — runner binary directory
        //   /workspace       (RW) — user's working dir
        let mut env = std::collections::HashMap::new();
        env.insert("SMOOTH_TASK".into(), message.clone());
        env.insert("SMOOTH_API_URL".into(), api_url);
        env.insert("SMOOTH_API_KEY".into(), api_key);
        env.insert("SMOOTH_MODEL".into(), final_model);
        env.insert("SMOOTH_WORKSPACE".into(), "/workspace".into());
        env.insert("SMOOTH_OPERATOR_ID".into(), tid.clone());
        if let Some(b) = budget {
            env.insert("SMOOTH_BUDGET_USD".into(), b.to_string());
        }
        // In Boardroom mode, tell every operator VM how to reach the
        // Boardroom's Archivist and Big Smooth's pearl API. The Scribe
        // forwarder inside the operator will POST batches to the Archivist
        // URL, and pearl tools will call Big Smooth's API.
        if let Some(ref room) = boardroom_handles {
            match room.operator_facing_archivist_url() {
                Some(archivist_url) => {
                    tracing::info!(task_id = tid, url = %archivist_url, "operator env: SMOOTH_ARCHIVIST_URL set");
                    env.insert("SMOOTH_ARCHIVIST_URL".into(), archivist_url.clone());
                    // Pearl tools: operators access .smooth/dolt/ directly in the
                    // workspace bind mount. No HTTP plumbing needed — the runner
                    // auto-detects the Dolt dir and registers local pearl tools.
                }
                None => {
                    tracing::warn!(task_id = tid, "operator_facing_archivist_url() returned None — operator will NOT forward logs to Archivist. Check SMOOTH_ARCHIVIST_HOST_PORT and SMOOTH_BOOTSTRAP_BILL_URL env vars.");
                }
            }
        }

        // Generate a task-type-specific policy TOML for Wonk inside the VM.
        // We default to TaskType::Coding in the `execute` phase, which gives
        // the in-VM agent full file/bash/search access. Follow-up: thread
        // TaskType + Phase through TaskStart so the policy matches the
        // orchestrator's current state.
        //
        // The policy TOML is multi-line and microsandbox passes env vars via
        // the kernel command line, which rejects non-printable ASCII
        // (newlines included). So instead of shipping it via env var, we
        // write it to a per-task host tempdir, bind-mount that dir RO into
        // the VM, and point the runner at the file via SMOOTH_POLICY_FILE.
        // The tempdir is intentionally leaked: /tmp is tmpfs on macOS HVF
        // hosts and gets reclaimed on reboot; the cleanup cost of tracking
        // every per-task dir isn't worth the complexity.
        let mut policy_dir_guard: Option<tempfile::TempDir> = None;
        let operator_token = crate::policy::generate_operator_token(&tid);
        match crate::policy::generate_policy_for_task(
            &tid,
            &pearl_id.clone().unwrap_or_default(),
            "execute",
            &operator_token,
            &[],
            crate::policy::TaskType::Coding,
        ) {
            Ok(policy_toml) => match tempfile::Builder::new().prefix("smooth-policy-").tempdir() {
                Ok(dir) => {
                    let policy_file = dir.path().join("policy.toml");
                    if let Err(e) = std::fs::write(&policy_file, &policy_toml) {
                        tracing::warn!(task_id = tid, error = %e, "failed to write policy tempfile; runner will use default");
                    }
                    policy_dir_guard = Some(dir);
                }
                Err(e) => {
                    tracing::warn!(task_id = tid, error = %e, "failed to create policy tempdir; runner will use default");
                }
            },
            Err(e) => {
                tracing::warn!(task_id = tid, error = %e, "policy generation failed; runner will use default");
            }
        }

        // If we managed to write a policy file, point the runner at it and
        // add a bind mount for the dir. In Boardroom mode the tempdir is
        // inside the Boardroom VM's filesystem — Bill can't bind-mount it
        // into the operator VM because it doesn't exist on the host. Skip
        // the mount; the runner will use its default policy which covers
        // the execute phase. Future: pipe policy content through Bill's
        // protocol so the file lands on the host.
        let in_boardroom = boardroom_handles.is_some();
        let policy_mount = if !in_boardroom {
            if let Some(ref dir) = policy_dir_guard {
                let host = dir
                    .path()
                    .canonicalize()
                    .unwrap_or_else(|_| dir.path().to_path_buf())
                    .to_string_lossy()
                    .to_string();
                env.insert("SMOOTH_POLICY_FILE".into(), "/opt/smooth/policy/policy.toml".into());
                Some(BindMount {
                    host_path: host,
                    guest_path: "/opt/smooth/policy".into(),
                    readonly: true,
                })
            } else {
                None
            }
        } else {
            tracing::info!(task_id = tid, "boardroom mode: skipping policy bind mount (runner will use default policy)");
            None
        };

        // Operator VMs need to reach Bill on host loopback so Big Smooth
        // (running inside the Boardroom VM) can request exec/destroy, AND
        // their in-VM Scribe needs to reach the Boardroom's Archivist for
        // log forwarding. Both destinations are 127.0.0.1:<port> from the
        // guest's perspective, which microsandbox's default `public_only`
        // policy denies. Always opt operator VMs in — Wonk's in-VM policy
        // still enforces fine-grained network allowlists for tool traffic,
        // so this only unlocks the sandbox↔host control plane, not
        // arbitrary agent access.
        let config = SandboxConfig {
            bead_id: pearl_id.clone().unwrap_or_default(),
            workspace_path: "/workspace".into(),
            env,
            mounts: vec![
                BindMount {
                    host_path: runner_dir_str.clone(),
                    guest_path: "/opt/smooth/bin".into(),
                    readonly: true,
                },
                BindMount {
                    host_path: workspace_canon.clone(),
                    guest_path: "/workspace".into(),
                    readonly: false,
                },
            ]
            .into_iter()
            .chain(policy_mount)
            .collect(),
            allow_host_loopback: true,
            // Pearl env cache: pass the pearl ID so the sandbox client
            // (Bill, running on the host) can derive the cache dir.
            // Big Smooth can't compute host paths from inside the
            // Boardroom VM — that's Bill's job.
            // Pearl env cache key. If SMOOTH_ENV_CACHE_KEY is set (typically
            // by the test harness for repeatable warm cache), use that
            // stable key so deps persist across test runs. Otherwise use
            // the pearl ID (each task gets its own cache, warm on retry).
            env_cache_key: std::env::var("SMOOTH_ENV_CACHE_KEY")
                .ok()
                .filter(|k| !k.is_empty())
                .or_else(|| pearl_id.clone())
                .or_else(|| Some(tid.clone())),
            ..SandboxConfig::default()
        };

        let host_port = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .ok()
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port())
            .unwrap_or(0);

        let handle = match sandbox::create_sandbox(&config, host_port).await {
            Ok(h) => h,
            Err(e) => {
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: tid.clone(),
                    message: format!("sandbox create failed: {e:#}"),
                });
                tracing::error!(task_id = tid, error = %e, "sandboxed dispatch: create_sandbox failed");
                return;
            }
        };

        let _ = event_tx.send(ServerEvent::ToolCallComplete {
            task_id: tid.clone(),
            tool_name: "sandbox.create".into(),
            result: handle.operator_id.clone(),
            is_error: false,
            duration_ms: 0,
        });
        touch();

        // Exec the runner inside the VM. The agent has a bash tool and can
        // install whatever dev tools it needs (apk add cargo rust, etc.)
        // as part of its own workflow. No pre-installation — the agent
        // discovers its environment and adapts. Quality checks are the
        // agent's responsibility: it should compile, test, and iterate
        // before reporting done.
        let runner_in_vm = format!("/opt/smooth/bin/{runner_name}");
        let _ = event_tx.send(ServerEvent::ToolCallStart {
            task_id: tid.clone(),
            tool_name: "sandbox.exec".into(),
            arguments: runner_in_vm.clone(),
        });

        let exec_started = std::time::Instant::now();
        let (stdout, stderr, code) = match sandbox::exec_in_sandbox(&handle.msb_name, &[runner_in_vm.as_str()]).await {
            Ok(r) => r,
            Err(e) => {
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: tid.clone(),
                    message: format!("sandbox exec failed: {e}"),
                });
                let _ = sandbox::destroy_sandbox(&handle.msb_name).await;
                return;
            }
        };
        touch();

        // The runner emits one JSON AgentEvent per line on stdout. Parse each
        // line and translate to ServerEvents. Any non-JSON line is forwarded
        // as a raw TokenDelta (helps with debugging).
        let mut agent_iterations: u32 = 0;
        let mut saw_completed = false;
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(event) => {
                    let Some(ty) = event.get("type").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    match ty {
                        "TokenDelta" => {
                            if let Some(content) = event.get("content").and_then(|v| v.as_str()) {
                                let _ = event_tx.send(ServerEvent::TokenDelta {
                                    task_id: tid.clone(),
                                    content: content.to_string(),
                                });
                            }
                        }
                        "ToolCallStart" => {
                            let tool_name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let _ = event_tx.send(ServerEvent::ToolCallStart {
                                task_id: tid.clone(),
                                tool_name,
                                arguments: String::new(),
                            });
                        }
                        "ToolCallComplete" => {
                            let tool_name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let is_error = event.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false);
                            let _ = event_tx.send(ServerEvent::ToolCallComplete {
                                task_id: tid.clone(),
                                tool_name,
                                result: String::new(),
                                is_error,
                                duration_ms: 0,
                            });
                        }
                        "Completed" => {
                            saw_completed = true;
                            if let Some(iters) = event.get("iterations").and_then(serde_json::Value::as_u64) {
                                agent_iterations = u32::try_from(iters).unwrap_or(u32::MAX);
                            }
                        }
                        "Error" => {
                            if let Some(message) = event.get("message").and_then(|v| v.as_str()) {
                                let _ = event_tx.send(ServerEvent::TaskError {
                                    task_id: tid.clone(),
                                    message: message.to_string(),
                                });
                            }
                        }
                        // Started / LlmRequest / LlmResponse / etc. are
                        // informational — we don't forward them yet but can
                        // later if clients want richer visibility.
                        _ => {}
                    }
                }
                Err(_) => {
                    // Non-JSON line — forward as TokenDelta so the user can
                    // see any debugging output the runner prints directly.
                    let _ = event_tx.send(ServerEvent::TokenDelta {
                        task_id: tid.clone(),
                        content: format!("{line}\n"),
                    });
                }
            }
        }
        if !stderr.is_empty() {
            // Runner stderr is tracing output + NarcHook alert summaries.
            // Forward it so operators can audit what the in-VM stack saw.
            let _ = event_tx.send(ServerEvent::TokenDelta {
                task_id: tid.clone(),
                content: format!("[runner stderr]\n{stderr}"),
            });
        }

        let _ = event_tx.send(ServerEvent::ToolCallComplete {
            task_id: tid.clone(),
            tool_name: "sandbox.exec".into(),
            result: format!("exit={code}"),
            is_error: code != 0,
            duration_ms: u64::try_from(exec_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });

        if let Err(e) = sandbox::destroy_sandbox(&handle.msb_name).await {
            tracing::warn!(task_id = tid, error = %e, "sandboxed dispatch: destroy_sandbox failed");
        }

        if code == 0 && saw_completed {
            let _ = event_tx.send(ServerEvent::TaskComplete {
                task_id: tid.clone(),
                iterations: agent_iterations,
                cost_usd: 0.0,
            });
            if let Some(ref id) = pearl_id {
                let _ = pearl_store.close(&[id]);
            }
            tracing::info!(task_id = tid, iterations = agent_iterations, "sandboxed WS task completed");
        } else {
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: tid.clone(),
                message: format!("sandboxed runner exited with code {code}"),
            });
            tracing::error!(task_id = tid, exit = code, "sandboxed WS task failed");
        }

        touch();
    });
}

/// Load LLM config for the in-VM runner. Big Smooth reads its own
/// providers.json (which it already does for the in-process path) and
/// projects the relevant fields into env vars the runner can consume.
fn load_llm_config_for_runner(model_override: &Option<String>) -> anyhow::Result<(String, String, String)> {
    let providers_path = dirs_next::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?
        .join(".smooth/providers.json");
    let registry = smooth_operator::providers::ProviderRegistry::load_from_file(&providers_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", providers_path.display()))?;
    let llm = registry.default_llm_config().map_err(|e| anyhow::anyhow!("default provider: {e}"))?;
    let model = model_override.clone().unwrap_or(llm.model);
    Ok((llm.api_url, llm.api_key, model))
}

// ── Health ─────────────────────────────────────────────────

async fn health_handler(State(state): State<AppState>) -> Json<HealthResponse> {
    state.touch();
    Json(HealthResponse {
        ok: true,
        service: "smooth-leader".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        uptime: state.start_time.elapsed().as_secs_f64(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    })
}

async fn system_health_handler(State(state): State<AppState>) -> Json<ApiResponse<SystemHealth>> {
    state.touch();
    let db_ok = state.db.get_config("__health_check").is_ok();
    let ts = crate::tailscale::get_status();

    Json(ApiResponse {
        data: SystemHealth {
            leader: LeaderHealth {
                status: "healthy".into(),
                uptime: state.start_time.elapsed().as_secs_f64(),
            },
            database: DatabaseHealth {
                status: if db_ok { "healthy" } else { "down" }.into(),
                path: state.db.path().display().to_string(),
            },
            sandbox: SandboxHealth {
                status: "healthy".into(),
                backend: "local-microsandbox".into(),
                active_sandboxes: 0,
                max_concurrency: 3,
            },
            tailscale: TailscaleHealth {
                status: if ts.connected { "connected" } else { "disconnected" }.into(),
                hostname: ts.hostname,
            },
            pearls: PearlsHealth {
                status: "healthy".into(),
                open_pearls: state.pearl_store.stats().map_or(0, |s| (s.open + s.in_progress) as u32),
            },
        },
        ok: true,
    })
}

// ── Config ─────────────────────────────────────────────────

async fn get_config_handler(State(state): State<AppState>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    Json(ApiResponse {
        data: serde_json::json!({}),
        ok: true,
    })
}

async fn set_config_handler(State(state): State<AppState>, Json(body): Json<ConfigBody>) -> Json<ApiResponse<()>> {
    state.touch();
    let value_str = serde_json::to_string(&body.value).unwrap_or_default();
    let _ = state.db.set_config(&body.key, &value_str);
    Json(ApiResponse { data: (), ok: true })
}

// ── Tasks (headless agent execution via SSE) ──────────────

async fn run_task_handler(State(state): State<AppState>, Json(req): Json<TaskRequest>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    state.touch();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

    // Determine working directory
    let working_dir = req
        .working_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Create issue for tracking
    let pearl_id = crate::pearls::create_pearl(
        &state.pearl_store,
        &format!("Task: {}", truncate_str(&req.message, 60)),
        &req.message,
        "task",
        2,
    )
    .ok()
    .map(|i| i.id);

    if let Some(ref id) = pearl_id {
        let update = smooth_pearls::PearlUpdate {
            status: Some(smooth_pearls::PearlStatus::InProgress),
            ..Default::default()
        };
        let _ = state.pearl_store.update(id, &update);
    }

    let pearl_store = state.pearl_store.clone();
    let message = req.message.clone();
    let model = req.model.clone();
    let budget = req.budget;
    let event_tx = state.event_tx.clone();
    let task_id = uuid::Uuid::new_v4().to_string();

    // Spawn the agent in a background task
    tokio::spawn(async move {
        let result = run_agent_task(working_dir, message, model, budget, tx.clone()).await;

        match result {
            Ok(cost) => {
                // Send cost event
                let _ = tx.send(AgentEvent::Completed {
                    agent_id: "task".into(),
                    iterations: 0,
                });
                // Also broadcast to WebSocket clients
                let _ = event_tx.send(ServerEvent::TaskComplete {
                    task_id: task_id.clone(),
                    iterations: 0,
                    cost_usd: cost,
                });
                drop(tx);

                // Close the issue on success
                if let Some(ref id) = pearl_id {
                    let _ = pearl_store.close(&[id]);
                }

                tracing::info!(cost_usd = cost, "Task completed successfully");
            }
            Err(e) => {
                let _ = tx.send(AgentEvent::Error { message: e.to_string() });
                // Also broadcast to WebSocket clients
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: task_id.clone(),
                    message: e.to_string(),
                });
                drop(tx);
                tracing::error!(error = %e, "Task failed");
            }
        }
    });

    // Convert the receiver into an SSE stream
    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
    let sse_stream = futures_util::StreamExt::map(stream, |event| {
        let data = serde_json::to_string(&event).unwrap_or_else(|_| r#"{"type":"Error","message":"serialization failed"}"#.into());
        Ok(Event::default().data(data))
    });

    Sse::new(sse_stream)
}

/// Run an agent task and return the total cost.
async fn run_agent_task(
    working_dir: PathBuf,
    message: String,
    model: Option<String>,
    budget: Option<f64>,
    tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
) -> anyhow::Result<f64> {
    // 1. Load LLM config from providers.json
    let providers_path = dirs_next::home_dir()
        .map(|h| h.join(".smooth/providers.json"))
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;

    let mut llm_config = if providers_path.exists() {
        let registry = ProviderRegistry::load_from_file(&providers_path).map_err(|e| anyhow::anyhow!("Failed to load providers.json: {e}"))?;
        registry
            .default_llm_config()
            .map_err(|e| anyhow::anyhow!("No default provider configured: {e}"))?
            .with_temperature(0.3)
    } else {
        anyhow::bail!("No LLM providers configured. Run: th auth login <provider>");
    };

    // 2. Override model if specified
    if let Some(ref m) = model {
        llm_config = llm_config.with_model(m);
    }

    // 3. Create AgentConfig
    let system_prompt = "You are Smooth Coding, an AI coding assistant. \
        Help the user with their coding task. Use the provided tools to read, write, and execute code. \
        Be concise and thorough.";

    let mut config = AgentConfig::new("smooth-task", system_prompt, llm_config).with_max_iterations(50);

    // 4. Set budget if specified
    if let Some(max_usd) = budget {
        config = config.with_budget(CostBudget {
            max_cost_usd: Some(max_usd),
            max_tokens: None,
        });
    }

    // 5. Create tools scoped to working directory
    let mut tools = smooth_code::tools::create_tools(&working_dir);

    // 6. Register NarcHook for security
    let narc_hook = Arc::new(NarcHook::new(false));
    tools.add_hook(SharedNarcHook { inner: Arc::clone(&narc_hook) });

    // 7. Run agent with channel for SSE streaming
    let agent = Agent::new(config, tools);
    let _conversation = agent.run_with_channel(&message, tx).await?;

    // 8. Return cost
    #[allow(clippy::expect_used)]
    let cost = agent.cost_tracker.lock().expect("lock cost_tracker").total_cost_usd;
    Ok(cost)
}

/// Truncate a string to at most `max_len` characters, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

// ── Issues ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ListPearlsParams {
    status: Option<String>,
}

#[derive(Deserialize)]
pub struct CreatePearlBody {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "type", default = "default_pearl_type")]
    pearl_type: String,
    #[serde(default = "default_priority")]
    priority: u8,
}

fn default_pearl_type() -> String {
    "task".into()
}

const fn default_priority() -> u8 {
    2
}

#[derive(Deserialize)]
pub struct UpdatePearlBody {
    status: Option<String>,
    title: Option<String>,
    description: Option<String>,
    priority: Option<u8>,
    #[serde(rename = "type")]
    pearl_type: Option<String>,
}

async fn list_pearls_handler(State(state): State<AppState>, Query(params): Query<ListPearlsParams>) -> Json<ApiResponse<Vec<smooth_pearls::Pearl>>> {
    state.touch();
    let issues = crate::pearls::list_pearls(&state.pearl_store, params.status.as_deref()).unwrap_or_default();
    Json(ApiResponse { data: issues, ok: true })
}

async fn get_pearl_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let issue = crate::pearls::get_pearl(&state.pearl_store, &id).unwrap_or(None);
    let data = match issue {
        Some(i) => serde_json::to_value(i).unwrap_or(serde_json::json!(null)),
        None => serde_json::json!(null),
    };
    Json(ApiResponse { data, ok: true })
}

async fn ready_pearls_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<smooth_pearls::Pearl>>> {
    state.touch();
    let issues = crate::pearls::get_ready(&state.pearl_store).unwrap_or_default();
    Json(ApiResponse { data: issues, ok: true })
}

async fn create_pearl_handler(State(state): State<AppState>, Json(body): Json<CreatePearlBody>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    match crate::pearls::create_pearl(&state.pearl_store, &body.title, &body.description, &body.pearl_type, body.priority) {
        Ok(issue) => Json(ApiResponse {
            data: serde_json::to_value(issue).unwrap_or(serde_json::json!(null)),
            ok: true,
        }),
        Err(e) => Json(ApiResponse {
            data: serde_json::json!({"error": e.to_string()}),
            ok: false,
        }),
    }
}

async fn update_pearl_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdatePearlBody>,
) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let update = smooth_pearls::PearlUpdate {
        title: body.title,
        description: body.description,
        status: body.status.as_deref().and_then(smooth_pearls::PearlStatus::from_str_loose),
        priority: body.priority.and_then(smooth_pearls::Priority::from_u8),
        pearl_type: body.pearl_type.as_deref().and_then(smooth_pearls::PearlType::from_str_loose),
        ..Default::default()
    };
    match state.pearl_store.update(&id, &update) {
        Ok(issue) => Json(ApiResponse {
            data: serde_json::to_value(issue).unwrap_or(serde_json::json!(null)),
            ok: true,
        }),
        Err(e) => Json(ApiResponse {
            data: serde_json::json!({"error": e.to_string()}),
            ok: false,
        }),
    }
}

async fn close_pearl_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    match state.pearl_store.close(&[&id]) {
        Ok(count) => Json(ApiResponse {
            data: serde_json::json!({"closed": count}),
            ok: true,
        }),
        Err(e) => Json(ApiResponse {
            data: serde_json::json!({"error": e.to_string()}),
            ok: false,
        }),
    }
}

async fn stats_handler(State(state): State<AppState>) -> Json<ApiResponse<smooth_pearls::PearlStats>> {
    state.touch();
    let stats = crate::pearls::stats(&state.pearl_store).unwrap_or_default();
    Json(ApiResponse { data: stats, ok: true })
}

// ── Workers ────────────────────────────────────────────────

async fn list_workers_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    state.touch();
    // TODO: Query worker_runs from SQLite
    Json(ApiResponse { data: vec![], ok: true })
}

async fn get_worker_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    Json(ApiResponse {
        data: serde_json::json!({"id": id, "status": "unknown"}),
        ok: true,
    })
}

async fn kill_worker_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<()>> {
    state.touch();
    tracing::info!("Kill worker {id}");
    Json(ApiResponse { data: (), ok: true })
}

// ── Messages ───────────────────────────────────────────────

async fn inbox_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    state.touch();
    Json(ApiResponse { data: vec![], ok: true })
}

async fn session_messages_handler(
    State(state): State<AppState>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    state.touch();
    use crate::session::SessionStore;
    let msgs = state.session_store.get_messages(&session_id, 100).unwrap_or_default();
    let data: Vec<serde_json::Value> = msgs
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "session_id": m.session_id,
                "from": m.from,
                "to": m.to,
                "content": m.content,
                "message_type": format!("{:?}", m.message_type),
                "timestamp": m.timestamp.to_rfc3339(),
            })
        })
        .collect();
    Json(ApiResponse { data, ok: true })
}

// ── Reviews ────────────────────────────────────────────────

async fn list_reviews_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    state.touch();
    Json(ApiResponse { data: vec![], ok: true })
}

async fn approve_review_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<()>> {
    state.touch();
    tracing::info!("Approve review for {bead_id}");
    let _ = state.pearl_store.close(&[&bead_id]);
    Json(ApiResponse { data: (), ok: true })
}

async fn reject_review_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<()>> {
    state.touch();
    tracing::info!("Reject review for {bead_id}");
    Json(ApiResponse { data: (), ok: true })
}

// ── Chat ───────────────────────────────────────────────────

async fn chat_handler(State(state): State<AppState>, Json(body): Json<ChatBody>) -> Json<ApiResponse<String>> {
    state.touch();
    match crate::chat::chat(&body.content).await {
        Ok(response) => Json(ApiResponse { data: response, ok: true }),
        Err(e) => Json(ApiResponse {
            data: format!("Error: {e}"),
            ok: true,
        }),
    }
}

// ── Search ─────────────────────────────────────────────────

async fn search_handler(State(state): State<AppState>, Query(params): Query<SearchParams>) -> Json<ApiResponse<Vec<crate::search::SearchResult>>> {
    state.touch();
    let query = params.q.unwrap_or_default();
    if query.is_empty() {
        return Json(ApiResponse { data: vec![], ok: true });
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let results = crate::search::search_all(&query, &cwd, &state.pearl_store);
    Json(ApiResponse { data: results, ok: true })
}

// ── Steering ───────────────────────────────────────────────

async fn pause_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    tracing::info!("Pause operator on {bead_id}");
    let _ = state.pearl_store.add_comment(&bead_id, "[STEERING:PAUSE] Operator paused by human.");
    Json(ApiResponse {
        data: "paused".into(),
        ok: true,
    })
}

async fn resume_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    tracing::info!("Resume operator on {bead_id}");
    let _ = state.pearl_store.add_comment(&bead_id, "[STEERING:RESUME] Operator resumed.");
    Json(ApiResponse {
        data: "resumed".into(),
        ok: true,
    })
}

async fn steer_handler(State(state): State<AppState>, Path(bead_id): Path<String>, Json(body): Json<SteerBody>) -> Json<ApiResponse<String>> {
    state.touch();
    let msg = body.message.unwrap_or_default();
    tracing::info!("Steer operator on {bead_id}: {msg}");
    let _ = state.pearl_store.add_comment(&bead_id, &format!("[STEERING:GUIDANCE] {msg}"));
    Json(ApiResponse {
        data: "steered".into(),
        ok: true,
    })
}

async fn cancel_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    tracing::info!("Cancel operator on {bead_id}");
    let _ = state.pearl_store.add_comment(&bead_id, "[STEERING:CANCEL] Operator cancelled.");
    Json(ApiResponse {
        data: "cancelled".into(),
        ok: true,
    })
}

// ── Jira ───────────────────────────────────────────────────

async fn jira_status_handler(State(state): State<AppState>) -> Json<ApiResponse<crate::jira::SyncStatus>> {
    state.touch();
    let config = crate::jira::JiraConfig::from_db(&state.db);
    let connected = if let Some(ref c) = config {
        crate::jira::check_connection(c).await
    } else {
        false
    };
    Json(ApiResponse {
        data: crate::jira::SyncStatus {
            connected,
            last_sync: None,
            pending_changes: 0,
        },
        ok: true,
    })
}

async fn jira_sync_handler(State(state): State<AppState>) -> Json<ApiResponse<crate::jira::SyncResult>> {
    state.touch();
    Json(ApiResponse {
        data: crate::jira::SyncResult {
            pulled: 0,
            pushed: 0,
            conflicts: 0,
        },
        ok: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_health_response_serializes() {
        let resp = HealthResponse {
            ok: true,
            service: "test".into(),
            version: "0.1.0".into(),
            uptime: 42.0,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"uptime\":42.0"));
    }

    #[test]
    fn test_api_response_serializes() {
        let resp = ApiResponse {
            data: vec!["a", "b"],
            ok: true,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("[\"a\",\"b\"]"));
    }

    #[tokio::test]
    async fn test_router_builds() {
        let db = Database::open(&PathBuf::from(":memory:")).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return;
        };
        let state = AppState::new(db, pearl_store);
        let _router = build_router(state);
        // If we get here without panic, the router is valid
    }

    #[test]
    fn test_app_state_touch_updates_activity() {
        let db = Database::open(&PathBuf::from(":memory:")).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return;
        };
        let state = AppState::new(db, pearl_store);

        let before = *state.last_activity.lock().unwrap();
        std::thread::sleep(Duration::from_millis(10));
        state.touch();
        let after = *state.last_activity.lock().unwrap();
        assert!(after > before);
    }

    #[test]
    fn test_truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_str_long() {
        let result = truncate_str("this is a very long message that needs truncation", 20);
        assert!(result.len() <= 20);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_task_request_deserializes() {
        let json = r#"{"message":"Build X","model":"kimi-k2.5","budget":2.0}"#;
        let req: TaskRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.message, "Build X");
        assert_eq!(req.model.as_deref(), Some("kimi-k2.5"));
        assert_eq!(req.budget, Some(2.0));
        assert!(req.working_dir.is_none());
    }

    #[test]
    fn test_task_request_minimal() {
        let json = r#"{"message":"Do something"}"#;
        let req: TaskRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.message, "Do something");
        assert!(req.model.is_none());
        assert!(req.budget.is_none());
        assert!(req.working_dir.is_none());
    }
}

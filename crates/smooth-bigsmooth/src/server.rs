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
    pub issue_store: smooth_issues::IssueStore,
    pub start_time: Instant,
    pub last_activity: Arc<Mutex<Instant>>,
    pub idle_timeout: Duration,
    /// Broadcast channel for pushing [`ServerEvent`]s to all connected WebSocket clients.
    pub event_tx: broadcast::Sender<ServerEvent>,
}

impl AppState {
    /// Create a new `AppState` with default idle timeout.
    pub fn new(db: Database, issue_store: smooth_issues::IssueStore) -> Self {
        let (event_tx, _) = broadcast::channel(BROADCAST_CHANNEL_CAPACITY);
        Self {
            db,
            issue_store,
            start_time: Instant::now(),
            last_activity: Arc::new(Mutex::new(Instant::now())),
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            event_tx,
        }
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
    pub beads: BeadsHealth,
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
pub struct BeadsHealth {
    pub status: String,
    pub open_issues: u32,
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
        // Issues
        .route("/api/issues", get(list_issues_handler).post(create_issue_handler))
        .route("/api/issues/ready", get(ready_issues_handler))
        .route("/api/issues/stats", get(stats_handler))
        .route("/api/issues/{id}", get(get_issue_handler).patch(update_issue_handler))
        .route("/api/issues/{id}/close", post(close_issue_handler))
        // Backward-compat aliases for /api/beads
        .route("/api/beads", get(list_issues_handler))
        .route("/api/beads/{id}", get(get_issue_handler))
        .route("/api/beads/ready", get(ready_issues_handler))
        // Workers
        .route("/api/workers", get(list_workers_handler))
        .route("/api/workers/{id}", get(get_worker_handler).delete(kill_worker_handler))
        // Messages
        .route("/api/messages/inbox", get(inbox_handler))
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
pub async fn start(state: AppState, addr: SocketAddr) -> anyhow::Result<()> {
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
            let _ = state.issue_store.add_comment(&task_id, &comment);
        }
        ClientEvent::IssueCreate {
            title,
            description,
            issue_type,
            priority,
        } => {
            let desc = description.as_deref().unwrap_or("");
            let itype = issue_type.as_deref().unwrap_or("task");
            let prio = priority.unwrap_or(2);
            match crate::issues::create_issue(&state.issue_store, &title, desc, itype, prio) {
                Ok(issue) => {
                    let _ = state.event_tx.send(ServerEvent::IssueCreated { id: issue.id, title });
                }
                Err(e) => {
                    let _ = state.event_tx.send(ServerEvent::Error { message: e.to_string() });
                }
            }
        }
        ClientEvent::IssueUpdate { id, status, priority } => {
            let update = smooth_issues::IssueUpdate {
                status: status.as_deref().and_then(smooth_issues::IssueStatus::from_str_loose),
                priority: priority.and_then(smooth_issues::Priority::from_u8),
                ..Default::default()
            };
            match state.issue_store.update(&id, &update) {
                Ok(_issue) => {
                    let _ = state.event_tx.send(ServerEvent::IssueUpdated {
                        id,
                        status: status.unwrap_or_else(|| "updated".into()),
                    });
                }
                Err(e) => {
                    let _ = state.event_tx.send(ServerEvent::Error { message: e.to_string() });
                }
            }
        }
        ClientEvent::IssueClose { ids } => {
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            match state.issue_store.close(&refs) {
                Ok(count) => {
                    for id in &ids {
                        let _ = state.event_tx.send(ServerEvent::IssueUpdated {
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

/// Spawn an agent task from a WebSocket `TaskStart` event, broadcasting
/// [`ServerEvent`]s as the agent progresses.
async fn dispatch_ws_task(state: &AppState, message: String, model: Option<String>, budget: Option<f64>, working_dir: Option<String>) {
    let working_dir = working_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let task_id = uuid::Uuid::new_v4().to_string();
    let event_tx = state.event_tx.clone();

    // Create an issue for tracking
    let issue_store = state.issue_store.clone();
    let issue_id = crate::issues::create_issue(&issue_store, &format!("Task: {}", truncate_str(&message, 60)), &message, "task", 2)
        .ok()
        .map(|i| i.id);

    if let Some(ref id) = issue_id {
        let update = smooth_issues::IssueUpdate {
            status: Some(smooth_issues::IssueStatus::InProgress),
            ..Default::default()
        };
        let _ = issue_store.update(id, &update);
    }

    let tid = task_id.clone();
    let last_activity = state.last_activity.clone();
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
                if let Some(ref id) = issue_id {
                    let _ = issue_store.close(&[id]);
                }
                tracing::info!(task_id = tid, cost_usd = cost, "WS task completed");
            }
            Err(e) => {
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: tid.clone(),
                    message: e.to_string(),
                });
                tracing::error!(task_id = tid, error = %e, "WS task failed");
            }
        }
    });
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
            beads: BeadsHealth {
                status: "healthy".into(),
                open_issues: state.issue_store.stats().map_or(0, |s| (s.open + s.in_progress) as u32),
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
    let issue_id = crate::issues::create_issue(
        &state.issue_store,
        &format!("Task: {}", truncate_str(&req.message, 60)),
        &req.message,
        "task",
        2,
    )
    .ok()
    .map(|i| i.id);

    if let Some(ref id) = issue_id {
        let update = smooth_issues::IssueUpdate {
            status: Some(smooth_issues::IssueStatus::InProgress),
            ..Default::default()
        };
        let _ = state.issue_store.update(id, &update);
    }

    let issue_store = state.issue_store.clone();
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
                if let Some(ref id) = issue_id {
                    let _ = issue_store.close(&[id]);
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
pub struct ListIssuesParams {
    status: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateIssueBody {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "type", default = "default_issue_type")]
    issue_type: String,
    #[serde(default = "default_priority")]
    priority: u8,
}

fn default_issue_type() -> String {
    "task".into()
}

const fn default_priority() -> u8 {
    2
}

#[derive(Deserialize)]
pub struct UpdateIssueBody {
    status: Option<String>,
    title: Option<String>,
    description: Option<String>,
    priority: Option<u8>,
    #[serde(rename = "type")]
    issue_type: Option<String>,
}

async fn list_issues_handler(State(state): State<AppState>, Query(params): Query<ListIssuesParams>) -> Json<ApiResponse<Vec<smooth_issues::Issue>>> {
    state.touch();
    let issues = crate::issues::list_issues(&state.issue_store, params.status.as_deref()).unwrap_or_default();
    Json(ApiResponse { data: issues, ok: true })
}

async fn get_issue_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let issue = crate::issues::get_issue(&state.issue_store, &id).unwrap_or(None);
    let data = match issue {
        Some(i) => serde_json::to_value(i).unwrap_or(serde_json::json!(null)),
        None => serde_json::json!(null),
    };
    Json(ApiResponse { data, ok: true })
}

async fn ready_issues_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<smooth_issues::Issue>>> {
    state.touch();
    let issues = crate::issues::get_ready(&state.issue_store).unwrap_or_default();
    Json(ApiResponse { data: issues, ok: true })
}

async fn create_issue_handler(State(state): State<AppState>, Json(body): Json<CreateIssueBody>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    match crate::issues::create_issue(&state.issue_store, &body.title, &body.description, &body.issue_type, body.priority) {
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

async fn update_issue_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateIssueBody>,
) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let update = smooth_issues::IssueUpdate {
        title: body.title,
        description: body.description,
        status: body.status.as_deref().and_then(smooth_issues::IssueStatus::from_str_loose),
        priority: body.priority.and_then(smooth_issues::Priority::from_u8),
        issue_type: body.issue_type.as_deref().and_then(smooth_issues::IssueType::from_str_loose),
        ..Default::default()
    };
    match state.issue_store.update(&id, &update) {
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

async fn close_issue_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    match state.issue_store.close(&[&id]) {
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

async fn stats_handler(State(state): State<AppState>) -> Json<ApiResponse<smooth_issues::IssueStats>> {
    state.touch();
    let stats = crate::issues::stats(&state.issue_store).unwrap_or_default();
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

// ── Reviews ────────────────────────────────────────────────

async fn list_reviews_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    state.touch();
    Json(ApiResponse { data: vec![], ok: true })
}

async fn approve_review_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<()>> {
    state.touch();
    tracing::info!("Approve review for {bead_id}");
    let _ = state.issue_store.close(&[&bead_id]);
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
    let results = crate::search::search_all(&query, &cwd, &state.issue_store);
    Json(ApiResponse { data: results, ok: true })
}

// ── Steering ───────────────────────────────────────────────

async fn pause_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    tracing::info!("Pause operator on {bead_id}");
    let _ = state.issue_store.add_comment(&bead_id, "[STEERING:PAUSE] Operator paused by human.");
    Json(ApiResponse {
        data: "paused".into(),
        ok: true,
    })
}

async fn resume_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    tracing::info!("Resume operator on {bead_id}");
    let _ = state.issue_store.add_comment(&bead_id, "[STEERING:RESUME] Operator resumed.");
    Json(ApiResponse {
        data: "resumed".into(),
        ok: true,
    })
}

async fn steer_handler(State(state): State<AppState>, Path(bead_id): Path<String>, Json(body): Json<SteerBody>) -> Json<ApiResponse<String>> {
    state.touch();
    let msg = body.message.unwrap_or_default();
    tracing::info!("Steer operator on {bead_id}: {msg}");
    let _ = state.issue_store.add_comment(&bead_id, &format!("[STEERING:GUIDANCE] {msg}"));
    Json(ApiResponse {
        data: "steered".into(),
        ok: true,
    })
}

async fn cancel_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    tracing::info!("Cancel operator on {bead_id}");
    let _ = state.issue_store.add_comment(&bead_id, "[STEERING:CANCEL] Operator cancelled.");
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
        let issue_store = smooth_issues::IssueStore::open_in_memory().unwrap();
        let state = AppState::new(db, issue_store);
        let _router = build_router(state);
        // If we get here without panic, the router is valid
    }

    #[test]
    fn test_app_state_touch_updates_activity() {
        let db = Database::open(&PathBuf::from(":memory:")).unwrap();
        let issue_store = smooth_issues::IssueStore::open_in_memory().unwrap();
        let state = AppState::new(db, issue_store);

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

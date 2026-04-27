//! Axum HTTP server — all REST routes, middleware, CORS.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
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
use smooth_operator::providers::ProviderRegistry;
use smooth_operator::tool::{ToolCall, ToolHook, ToolResult};
use smooth_operator::AgentEvent;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::events::{ClientEvent, ServerEvent};

/// Default idle timeout: 30 minutes.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 30 * 60;

/// Default broadcast channel capacity.
const BROADCAST_CHANNEL_CAPACITY: usize = 256;

/// Default max concurrent Smooth Operators. Each is a real microVM
/// with its own RAM allocation, so the conservative default keeps a
/// dev laptop from thrashing. Override via `SMOOTH_SANDBOX_MAX_CONCURRENCY`
/// env var (or `th up --max-operators N` on the CLI, which sets it).
const DEFAULT_SANDBOX_MAX_CONCURRENCY: usize = 3;

/// Resolve the sandbox pool cap from `SMOOTH_SANDBOX_MAX_CONCURRENCY`,
/// falling back to the default. Values <= 0 or unparseable are treated
/// as unset.
fn max_sandbox_concurrency() -> usize {
    match std::env::var("SMOOTH_SANDBOX_MAX_CONCURRENCY").ok().and_then(|v| v.parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => DEFAULT_SANDBOX_MAX_CONCURRENCY,
    }
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub pearl_store: smooth_pearls::PearlStore,
    /// Per-project [`PearlStore`]s, each backed by a long-running
    /// `smooth-dolt serve` subprocess spawned in [`AppState::new`]. See
    /// `smooth_pearls::SmoothDoltServer` for why we don't open fresh
    /// stores from inside tokio handlers (pearl `th-1a61a7`).
    pub project_pearl_stores: Arc<HashMap<std::path::PathBuf, smooth_pearls::PearlStore>>,
    /// `SmoothDoltServer` handles for each cached project. Held to keep
    /// the spawned children alive — `Drop` SIGTERMs them and removes
    /// their socket files. The `PearlStore`s above route their queries
    /// through these via `SmoothDolt::from_server`.
    pub project_dolt_servers: Arc<HashMap<std::path::PathBuf, Arc<smooth_pearls::SmoothDoltServer>>>,
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
    /// Diver client — available when running in Boardroom mode with Diver.
    /// When present, dispatch/complete go through Diver's HTTP API (with
    /// Jira sync, cost tracking, etc.) instead of direct PearlStore calls.
    pub diver: Option<crate::diver_client::DiverClient>,
    /// The orchestration state machine. Runs as a background loop picking up
    /// ready pearls and dispatching operators. Behind `Arc<tokio::sync::Mutex<>>`
    /// since the background loop and API handlers both need access.
    pub orchestrator: Arc<tokio::sync::Mutex<crate::orchestrator::Orchestrator>>,
    /// Boardroom Narc — central LLM-judge-backed access arbiter. Every
    /// per-VM Wonk escalates to this when its local policy can't
    /// auto-approve a `/check/*` request. Always present (constructed with
    /// or without an LLM backend) so the `/api/narc/*` routes can unwrap
    /// unconditionally.
    pub boardroom_narc: crate::boardroom_narc::BoardroomNarc,
    /// Registry of live teammates (operators) — populated on dispatch,
    /// idled when the comment-tap sees `[IDLE]`. Powers `/api/teammates`
    /// and the chat-agent's `teammate_list` tool. See `crate::teammates`.
    pub teammates: Arc<crate::teammates::OperatorRegistry>,
}

impl AppState {
    /// Create a new `AppState` with default idle timeout.
    ///
    /// Reads `SMOOTH_SANDBOX_MAX_CONCURRENCY` from the environment to
    /// size the sandbox pool (defaults to 3 — each microVM eats real
    /// RAM so the conservative default keeps dev laptops happy).
    pub fn new(pearl_store: smooth_pearls::PearlStore) -> Self {
        // Bootstrap the per-process host-tool bearer token. Sandbox
        // teammates use this when calling /api/host/exec so we know the
        // call is from a legit dispatch and not a stray network reach.
        // Set in the env so BOTH the host-exec handler (reads
        // `SMOOTH_HOST_TOKEN`) and the dispatch path (passes the same
        // env into the sandbox) see the same value.
        if std::env::var_os("SMOOTH_HOST_TOKEN").is_none() {
            std::env::set_var("SMOOTH_HOST_TOKEN", crate::host_tools::generate_host_token());
        }
        let max_operators = max_sandbox_concurrency();
        let session_store = Arc::new(crate::session::DoltSessionStore::new(&pearl_store));
        let (event_tx, _) = broadcast::channel(BROADCAST_CHANNEL_CAPACITY);
        let orchestrator = crate::orchestrator::Orchestrator::new(max_operators, pearl_store.clone()).with_event_tx(event_tx.clone());

        // Pre-spawn a `smooth-dolt serve` per registered project, wrap each
        // in a server-mode `PearlStore`, cache. Synchronous code; runs
        // BEFORE axum starts so the smooth-dolt subprocess hang documented
        // in pearl `th-1a61a7` doesn't fire here. Failures are logged
        // and skipped — a single broken project shouldn't take the
        // service down.
        let (project_pearl_stores, project_dolt_servers) = match smooth_pearls::Registry::load() {
            Ok(registry) => {
                let mut stores: HashMap<std::path::PathBuf, smooth_pearls::PearlStore> = HashMap::new();
                let mut servers: HashMap<std::path::PathBuf, Arc<smooth_pearls::SmoothDoltServer>> = HashMap::new();
                for entry in registry.list() {
                    let path_str = entry.path.to_string_lossy().to_string();
                    if is_invalid_project(&path_str) {
                        continue;
                    }
                    let dolt_dir = entry.path.join(".smooth").join("dolt");
                    match smooth_pearls::SmoothDoltServer::spawn(&dolt_dir) {
                        Ok(server) => {
                            let server = Arc::new(server);
                            let dolt = smooth_pearls::SmoothDolt::from_server(server.clone(), &dolt_dir);
                            let store = smooth_pearls::PearlStore::from_dolt(dolt);
                            tracing::info!(path = %path_str, "spawned smooth-dolt serve for project");
                            stores.insert(entry.path.clone(), store);
                            servers.insert(entry.path.clone(), server);
                        }
                        Err(e) => {
                            tracing::warn!(path = %path_str, error = %e, "failed to spawn smooth-dolt serve; project unavailable until restart");
                        }
                    }
                }
                (Arc::new(stores), Arc::new(servers))
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load project registry");
                (Arc::new(HashMap::new()), Arc::new(HashMap::new()))
            }
        };

        // Construct the Boardroom Narc. If the host has an LLM provider
        // configured, Narc uses the default provider for its judge; otherwise
        // it runs rule-engine-only and escalates any unhandled request to a
        // human. Load is best-effort — a missing providers.json is fine in
        // dev + tests.
        let narc_llm_config = dirs_next::home_dir().and_then(|home| {
            let providers_path = home.join(".smooth/providers.json");
            if !providers_path.exists() {
                return None;
            }
            match smooth_operator::providers::ProviderRegistry::load_from_file(&providers_path) {
                // Route the Narc arbiter through the `Judge` slot (smooth-judge).
                // This is what the slot was named for — a cheap, fast
                // judge-class model. Falls back to the Default slot when the
                // Judge slot isn't configured (older providers.json files
                // without routing).
                Ok(registry) => match registry
                    .llm_config_for(smooth_operator::providers::Activity::Judge)
                    .or_else(|_| registry.default_llm_config())
                {
                    Ok(cfg) => Some(cfg),
                    Err(e) => {
                        tracing::warn!(error = %e, "boardroom narc: no judge/default LLM provider; Narc will escalate unknown requests to humans");
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "boardroom narc: failed to load providers.json; Narc will escalate unknown requests to humans");
                    None
                }
            }
        });
        let boardroom_narc = crate::boardroom_narc::BoardroomNarc::new(narc_llm_config);

        Self {
            pearl_store,
            project_pearl_stores,
            project_dolt_servers,
            session_store,
            start_time: Instant::now(),
            last_activity: Arc::new(Mutex::new(Instant::now())),
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            event_tx,
            boardroom: None,
            diver: None,
            orchestrator: Arc::new(tokio::sync::Mutex::new(orchestrator)),
            boardroom_narc,
            teammates: Arc::new(crate::teammates::OperatorRegistry::new()),
        }
    }

    /// Attach Boardroom cast handles to an existing state. Chainable.
    #[must_use]
    pub fn with_boardroom(mut self, handles: crate::boardroom::BoardroomHandles) -> Self {
        if !handles.diver_url.is_empty() {
            self.diver = Some(crate::diver_client::DiverClient::new(&handles.diver_url));
        }
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
    pub orchestrator: OrchestratorHealth,
}

#[derive(Serialize)]
pub struct OrchestratorHealth {
    pub state: String,
    pub active_workers: u32,
    pub completed: u32,
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
    /// Optional override for the chat agent's LLM. When unset the agent
    /// runs on the reasoning slot (smooth-reasoning-kimi by default).
    /// Set to e.g. "smooth-fast-gemini" for one-liner queries.
    #[serde(default)]
    model: Option<String>,
    /// Optional per-request USD cap on the chat agent. Stops the
    /// inner Agent loop when total cost exceeds this. Useful for
    /// keeping a bounded ceiling on tool-call recursion (chat-agent
    /// dispatches teammate → teammate ask_smooths → chat-agent
    /// answers → recurse). Defaults to no cap.
    #[serde(default)]
    budget_usd: Option<f64>,
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
    /// OCI image for the operator VM. Overrides the server's
    /// `SMOOTH_WORKER_IMAGE` env default. Usual value is
    /// `smooai/smooth-operator:latest` (unified image; agent
    /// installs toolchains at runtime via mise).
    #[serde(default)]
    pub image: Option<String>,
    /// Keep the operator VM alive after the agent emits Completed so
    /// the user can poke at a running dev server / REPL. Must be
    /// explicitly torn down via `th operators stop <id>`.
    #[serde(default)]
    pub keep_alive: bool,
    /// Per-run memory override in MB. `None` falls back to the
    /// `SandboxConfig::default()` 4096. Next.js + a couple workers on
    /// a big monorepo want 6–8 GB; smaller tasks can stay at 4.
    #[serde(default)]
    pub memory_mb: Option<u32>,
    /// Lead role to run under. Mapped directly to
    /// [`DispatchOptions::agent`]; the runner applies the
    /// corresponding [`smooth_operator::Clearance`].
    #[serde(default)]
    pub agent: Option<String>,
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
        // Projects (multi-project pearl support)
        .route("/api/projects", get(list_projects_handler))
        .route("/api/projects/pearls", get(project_pearls_handler))
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
        .route("/api/chat/sessions", get(list_chat_sessions_handler).post(create_chat_session_handler))
        .route("/api/chat/sessions/{id}", get(get_chat_session_handler).delete(delete_chat_session_handler))
        .route(
            "/api/chat/sessions/{id}/messages",
            get(get_chat_messages_handler).post(post_chat_message_handler),
        )
        // Search
        .route("/api/search", get(search_handler))
        // Steering
        .route("/api/steering/{bead_id}/pause", post(pause_handler))
        .route("/api/steering/{bead_id}/resume", post(resume_handler))
        .route("/api/steering/{bead_id}/steer", post(steer_handler))
        .route("/api/steering/{bead_id}/cancel", post(cancel_handler))
        // Phase 4: live-teammate registry + direct chat with a selected teammate.
        .route("/api/teammates", get(list_teammates_handler))
        .route(
            "/api/teammates/{name}/messages",
            get(get_teammate_messages_handler).post(post_teammate_message_handler),
        )
        .route("/api/teammates/{name}/shutdown", post(shutdown_teammate_handler))
        // Delegation — operator-to-operator delegation via sub-pearls
        .route("/api/delegate", post(delegate_handler))
        .route("/api/delegate/{id}/status", get(delegate_status_handler))
        // Orchestrator
        .route("/api/orchestrator/status", get(orchestrator_status_handler))
        // Jira
        .route("/api/jira/status", get(jira_status_handler))
        .route("/api/jira/sync", post(jira_sync_handler))
        // Boardroom Narc — central LLM-judge access arbiter. Per-VM Wonks
        // POST their uncertain /check/* decisions here; Narc applies the
        // rule engine, its decision cache, and (when unresolved) the LLM
        // judge, then returns an approve/deny/escalate verdict.
        .route("/api/narc/judge", post(narc_judge_handler))
        .route("/api/host/exec", post(crate::host_tools::host_exec_handler))
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
        match crate::boardroom::spawn_boardroom_cast(None).await {
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

    // Spawn orchestrator loop — continuously picks up ready pearls and dispatches operators
    let orch = state.orchestrator.clone();
    tokio::spawn(async move {
        loop {
            {
                let mut o = orch.lock().await;
                if let Err(e) = o.step().await {
                    tracing::debug!(error = %e, state = ?o.state, "orchestrator step error");
                }
            }
            // Poll interval — 5s default. The lock is released between polls
            // so API handlers can inspect orchestrator state without blocking.
            tokio::time::sleep(Duration::from_millis(5000)).await;
        }
    });
    tracing::info!("Orchestrator loop started (poll every 5s)");

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // Print runtime-identifying facts on startup so anyone staring
    // at a long-running Big Smooth can confirm which binary + modes
    // are in effect without having to inspect the process env.
    let workflow_mode = if std::env::var("SMOOTH_WORKFLOW")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
    {
        "disabled (SMOOTH_WORKFLOW=0)"
    } else {
        "enabled (default)"
    };
    let skip_test = std::env::var("SMOOTH_WORKFLOW_SKIP_TEST")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let dispatch_mode = if std::env::var("SMOOTH_WORKFLOW_DIRECT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        "direct (no sandbox; agent runs as host subprocess)"
    } else {
        "sandboxed (microVM per task)"
    };
    tracing::info!(
        version = env!("TH_VERSION"),
        workflow = workflow_mode,
        skip_test_phase = skip_test,
        dispatch = dispatch_mode,
        "Smooth leader running at http://{addr}"
    );
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
            agent,
        } => {
            // WebSocket callers don't currently carry image / keep_alive /
            // memory_mb; HTTP /api/tasks is the dispatch path for those.
            dispatch_ws_task(
                state,
                DispatchOptions {
                    message,
                    model,
                    budget,
                    working_dir,
                    agent,
                    ..DispatchOptions::default()
                },
            )
            .await;
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

/// Spawn an agent task from a WebSocket `TaskStart` event, broadcasting
/// [`ServerEvent`]s as the agent progresses.
///
/// ALL dispatch goes through the sandboxed path — Big Smooth stays
/// READ-ONLY. The operator runner inside the microVM hosts the real tools
/// (read_file, write_file, edit_file, grep, bash, etc.) with the full
/// security cast (Wonk/Goalie/Narc/Scribe) watching every call.
/// Options for dispatching an agent task. Bundled so the dispatch
/// helpers don't balloon past clippy's argument limit and so new
/// knobs (image, keep-alive, memory, …) can be added without
/// touching every call site.
#[derive(Debug, Clone, Default)]
pub struct DispatchOptions {
    pub message: String,
    pub model: Option<String>,
    pub budget: Option<f64>,
    pub working_dir: Option<String>,
    pub image: Option<String>,
    pub keep_alive: bool,
    pub memory_mb: Option<u32>,
    /// Lead role to run under. Propagated to the microVM runner
    /// as `SMOOTH_AGENT`; the runner resolves it against
    /// `Cast::builtin()` and installs a `PermissionHook`
    /// that blocks denied tool calls before they execute.
    pub agent: Option<String>,
    /// Reuse an existing pearl instead of creating a fresh one.
    /// The chat-agent's `teammate_spawn` tool sets this to the pearl
    /// id that `pearls_create` just returned, so dispatch doesn't
    /// duplicate it. When set:
    ///   - Diver (when present) reconciles status to `in_progress`
    ///     instead of dispatching a new entry.
    ///   - PearlStore-only path updates status to `in_progress` so
    ///     the orchestrator's "ready pearls" sweep doesn't double-
    ///     dispatch the same pearl.
    pub pearl_id: Option<String>,
}

pub async fn dispatch_ws_task(state: &AppState, opts: DispatchOptions) {
    // Direct mode: spawn the operator runner as a host subprocess
    // instead of a microVM. No Narc/Wonk/Goalie enforcement — the
    // agent has raw host access. Gated behind `SMOOTH_WORKFLOW_DIRECT=1`
    // so the default behaviour is the sandboxed path. Intended for
    // bench runs, fast dev loops, and environments that can't do
    // nested virtualisation (GitHub Actions, most cloud VMs, k8s
    // pods) — NOT for untrusted code.
    let direct_mode = std::env::var("SMOOTH_WORKFLOW_DIRECT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if direct_mode {
        dispatch_ws_task_direct(state, opts).await;
    } else {
        dispatch_ws_task_sandboxed(state, opts).await;
    }
}

/// Find the NATIVE operator-runner binary (built for the host
/// triple, not cross-compiled to `aarch64-unknown-linux-musl`).
/// Used by the direct-dispatch path where we exec the runner as
/// a regular subprocess rather than inside a microVM.
///
/// Resolution order:
/// 1. `SMOOTH_OPERATOR_RUNNER_NATIVE` env var (explicit override).
/// 2. Walk up from `CARGO_MANIFEST_DIR` looking for
///    `target/release/smooth-operator-runner`, then
///    `target/debug/smooth-operator-runner`.
/// 3. Same walk from `std::env::current_dir`.
fn find_native_operator_runner_binary() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("SMOOTH_OPERATOR_RUNNER_NATIVE") {
        let p = std::path::PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    let check = |base: &std::path::Path| -> Option<std::path::PathBuf> {
        for profile in ["release", "debug"] {
            let candidate = base.join("target").join(profile).join("smooth-operator-runner");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    };
    let manifest = env!("CARGO_MANIFEST_DIR");
    let mut dir = std::path::PathBuf::from(manifest);
    for _ in 0..5 {
        if let Some(p) = check(&dir) {
            return Some(p);
        }
        if !dir.pop() {
            break;
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(p) = check(&cwd) {
            return Some(p);
        }
    }
    None
}

/// Build a human-readable resumption context block from prior session
/// messages. Empty string if `pearl_id` is None or the pearl has no
/// prior messages — caller treats empty as "no resume".
///
/// Capped at `max_messages` so the context doesn't grow unbounded
/// across many iterations on the same pearl. Messages are tagged with
/// role + timestamp so the agent can see the sequence.
fn build_resumption_context(store: &crate::session::DoltSessionStore, pearl_id: Option<&str>, max_messages: usize) -> String {
    use crate::session::SessionStore;
    let Some(pearl_id) = pearl_id else {
        return String::new();
    };
    let Ok(messages) = store.get_messages(pearl_id, max_messages) else {
        return String::new();
    };
    if messages.is_empty() {
        return String::new();
    }
    let mut ctx = String::new();
    ctx.push_str("## Resumption context\n\n");
    ctx.push_str("You are continuing work on this pearl. The following is a condensed log of what happened in prior sessions on this same pearl. Use it to understand what has already been done and avoid repeating yourself. The workspace files persist between sessions, so anything you see referenced should already exist on disk — verify with read_file before making assumptions.\n\n");
    for msg in messages.iter().rev().take(max_messages).rev() {
        let trimmed = if msg.content.chars().count() > 400 {
            let truncated: String = msg.content.chars().take(400).collect();
            format!("{truncated}…")
        } else {
            msg.content.clone()
        };
        ctx.push_str(&format!(
            "- [{}] {} → {}: {}\n",
            msg.timestamp.format("%Y-%m-%d %H:%M"),
            msg.from,
            msg.to,
            trimmed
        ));
    }
    ctx
}

fn find_operator_runner_binary() -> Option<std::path::PathBuf> {
    if let Ok(host_path) = std::env::var("SMOOTH_OPERATOR_RUNNER_HOST_PATH") {
        return Some(std::path::PathBuf::from(host_path));
    }
    if let Ok(explicit) = std::env::var("SMOOTH_OPERATOR_RUNNER") {
        let p = std::path::PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
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

/// Create a tempdir whose host path microsandbox can bind-mount into a
/// microVM. On macOS, `$TMPDIR` resolves to a SIP-protected location
/// (`/var/folders/.../T`) that microsandbox silently fails to mount;
/// `control_root` (typically `~/.smooth/control/`) avoids that trap. If
/// no root is provided we fall back to the system default — the caller
/// has already logged a warning in that case.
fn make_control_tempdir(prefix: &str, control_root: Option<&std::path::Path>) -> std::io::Result<tempfile::TempDir> {
    let mut b = tempfile::Builder::new();
    b.prefix(prefix);
    match control_root {
        Some(root) => b.tempdir_in(root),
        None => b.tempdir(),
    }
}

/// pearl th-461ab9 (diag): Decide whether an env-var value enables a flag.
/// Truthy: `1`, `true`, `TRUE`, `yes`, `on`. Anything else (incl. unset) → false.
fn diag_flag_is_truthy(value: &str) -> bool {
    matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "on")
}

/// pearl th-461ab9 (diag): Build the `/bin/sh -c` argv that wraps the runner
/// invocation with diagnostic capture. Writes 8 files into
/// `/var/log/smooth-runner/` (must be a RW bind-mount or guest-writable
/// tmpfs):
///
///   00-started.txt     UTC start timestamp
///   01-mounts.txt      `mount` output (proves bind mounts landed)
///   02-listing.txt     ls -la /opt/smooth/{,bin,policy} /workspace
///   03-env.txt         env var snapshot
///   04-runner-check.txt   runner binary `-x` test
///   05-stdout.log      runner stdout transcript
///   06-stderr.log      runner stderr transcript
///   07-exit-code.txt   runner exit code
///
/// Streaming back to bigsmooth's AgentEvent parser is preserved: tee
/// runs in the background reading FIFOs, with its OWN stdout/stderr
/// passed through the parent shell — sandbox.exec captures both as
/// they're emitted, just like the un-wrapped path.
fn build_runner_diag_wrapper_argv(runner_in_vm: &str) -> Vec<String> {
    // The wrapper:
    //   1. Snapshots pre-flight diagnostics (mounts, listings, env, runner-check)
    //   2. Pipes runner stdout through `tee` to 05-stdout.log AND parent stdout
    //      (so sandbox.exec → bigsmooth's AgentEvent parser keeps streaming)
    //   3. Pipes runner stderr through `tee` to 06-stderr.log AND parent stderr
    //   4. Captures the runner's exit code via `${PIPESTATUS[0]}` (bash) or by
    //      using a temp-file dance for plain POSIX `sh`
    //   5. Writes 07-exit-code.txt and exits with the runner's code
    //
    // POSIX sh doesn't have PIPESTATUS, so we round-trip the runner's exit
    // code through a per-run file inside the diag dir. The runner is wrapped
    // in a subshell that writes its $? to that file; the parent reads the
    // file at the end. This avoids FIFOs entirely (no tee-deadlock-on-SIGKILL
    // risk that bench harness flagged on run #2 of th-461ab9 reproduction).
    // NOTE: This script is concatenated onto a single logical line by the
    // surrounding `\\\n         ` continuation; do NOT add `# comments` —
    // they swallow the following `;`-separated commands. Keep all design
    // notes in this Rust comment block, not in the shell.
    //
    // Pipeline shape:
    //   { ( runner; echo $? > rcfile ) 2>&1 1>&3 | tee -a stderr.log >&2 ; } 3>&1 \
    //       | tee -a stdout.log
    //   rc=$(cat rcfile); exit $rc
    // - Inner subshell runs the runner, captures its exit status into rcfile
    //   (POSIX pipelines lose the leftmost stage's exit code, so we round-trip
    //   through a file).
    // - 2>&1 1>&3 swaps stdout/stderr so the LEFT branch of the inner pipe
    //   carries stderr, which is `tee -a stderr.log >&2` — appears on the
    //   parent's stderr. The 3>&1 redirect on the outer group brings the
    //   original stdout out as the group's stdout, which the OUTER pipe's
    //   tee then duplicates to stdout.log AND the parent's stdout.
    // - sandbox.exec captures both streams as they're emitted (streaming
    //   preserved). Crucially, NO FIFOs / no background tee — earlier FIFO
    //   approach could deadlock if the runner was SIGKILL'd before tee
    //   read EOF, hanging sandbox.exec and breaking subsequent grading
    //   exec calls (caught by bench harness, run #2 of th-461ab9 repro).
    let script = format!(
        "set +e; \
         mkdir -p /var/log/smooth-runner 2>/dev/null; \
         date -u +%Y-%m-%dT%H:%M:%SZ > /var/log/smooth-runner/00-started.txt 2>/dev/null; \
         mount > /var/log/smooth-runner/01-mounts.txt 2>&1; \
         ls -la /opt/smooth/ /opt/smooth/bin/ /opt/smooth/policy/ /workspace/ /var/log/smooth-runner/ \
             > /var/log/smooth-runner/02-listing.txt 2>&1; \
         env > /var/log/smooth-runner/03-env.txt 2>&1; \
         if [ -x {runner} ]; then echo runner-exists-and-executable; else echo MISSING-RUNNER; ls -la {runner} 2>&1; fi \
             > /var/log/smooth-runner/04-runner-check.txt 2>&1; \
         rcfile=/var/log/smooth-runner/07-exit-code.txt; \
         rm -f \"$rcfile\" 2>/dev/null; \
         {{ ( {runner}; echo $? > \"$rcfile\" ) 2>&1 1>&3 | tee -a /var/log/smooth-runner/06-stderr.log >&2; }} 3>&1 \
             | tee -a /var/log/smooth-runner/05-stdout.log; \
         rc=$(cat \"$rcfile\" 2>/dev/null); \
         [ -z \"$rc\" ] && rc=1; \
         exit $rc",
        runner = runner_in_vm
    );
    vec!["/bin/sh".into(), "-c".into(), script]
}

async fn dispatch_ws_task_sandboxed(state: &AppState, opts: DispatchOptions) {
    let DispatchOptions {
        message,
        model,
        budget,
        working_dir,
        image,
        keep_alive,
        memory_mb,
        agent,
        pearl_id: pearl_id_in,
    } = opts;
    use crate::sandbox::{self, BindMount, SandboxConfig};

    let task_id = uuid::Uuid::new_v4().to_string();
    let event_tx = state.event_tx.clone();
    let pearl_store = state.pearl_store.clone();
    let last_activity = state.last_activity.clone();
    let boardroom_handles = state.boardroom.clone();
    let orchestrator = state.orchestrator.clone();

    // Note: the old printable-ASCII guard was removed — the task message
    // is now delivered via SMOOTH_TASK_FILE (a bind-mounted tempfile), not
    // the kernel command line, so non-ASCII characters (em dashes, smart
    // quotes, Unicode, etc.) are safe. The kernel cmdline size limit
    // concern was also resolved by the task file approach.

    // Pearl lifecycle:
    //   1. If the caller (typically the chat-agent's `teammate_spawn` tool)
    //      already created a pearl and passed `pearl_id`, REUSE it. We just
    //      flip its status to in_progress (via Diver if available) so the
    //      orchestrator's ready-pearls sweep doesn't dispatch a duplicate.
    //   2. Otherwise dispatch through Diver when available (Boardroom mode).
    //   3. Fall back to direct PearlStore when neither applies.
    let diver = state.diver.clone();
    let pearl_id: Option<String> = if let Some(supplied) = pearl_id_in {
        // Pearl already exists — reconcile status so the orchestrator
        // doesn't pick it up as "ready" and dispatch a second teammate.
        if let Some(ref diver_client) = diver {
            // Diver knows about the pearl already; ask it to mark in_progress.
            // If Diver doesn't expose a status update (current state), the
            // direct PearlStore fallback below covers us.
            tracing::info!(pearl_id = %supplied, "dispatch: reusing caller-supplied pearl (Diver mode)");
        } else {
            tracing::info!(pearl_id = %supplied, "dispatch: reusing caller-supplied pearl");
        }
        let _ = pearl_store.update(
            &supplied,
            &smooth_pearls::PearlUpdate {
                status: Some(smooth_pearls::PearlStatus::InProgress),
                ..Default::default()
            },
        );
        Some(supplied)
    } else if let Some(ref diver_client) = diver {
        match diver_client.dispatch(&format!("Task: {}", truncate_str(&message, 60)), &message, None).await {
            Ok(id) => {
                tracing::info!(pearl_id = %id, "dispatch: pearl created via Diver");
                Some(id)
            }
            Err(e) => {
                tracing::warn!(error = %e, "dispatch: Diver dispatch failed, falling back to direct PearlStore");
                crate::pearls::create_pearl(&pearl_store, &format!("Task: {}", truncate_str(&message, 60)), &message, "task", 2)
                    .ok()
                    .map(|i| i.id)
            }
        }
    } else {
        let id = crate::pearls::create_pearl(&pearl_store, &format!("Task: {}", truncate_str(&message, 60)), &message, "task", 2)
            .ok()
            .map(|i| i.id);
        if let Some(ref id) = id {
            let _ = pearl_store.update(
                id,
                &smooth_pearls::PearlUpdate {
                    status: Some(smooth_pearls::PearlStatus::InProgress),
                    ..Default::default()
                },
            );
        }
        id
    };

    // Register the teammate + spawn the per-pearl comment tap so the web
    // UI's sidebar / sidebar's chat scope sees live operator output.
    if let Some(ref pid) = pearl_id {
        let title = pearl_store
            .get(pid)
            .ok()
            .flatten()
            .map(|p| p.title)
            .unwrap_or_else(|| truncate_str(&message, 60));
        let teammate_name = crate::teammates::slug_from_pearl(pid);
        let now = chrono::Utc::now();
        state
            .teammates
            .insert(crate::teammates::TeammateView {
                name: teammate_name.clone(),
                pearl_id: pid.clone(),
                title: title.clone(),
                status: "running".into(),
                started_at: now,
                last_event_at: now,
            })
            .await;
        let _ = state.event_tx.send(crate::events::ServerEvent::TeammateSpawned {
            teammate_name: teammate_name.clone(),
            pearl_id: pid.clone(),
            title,
        });
        let _tap = crate::teammates::spawn_comment_tap(pearl_store.clone(), pid.clone(), teammate_name, state.event_tx.clone(), state.teammates.clone()).await;
        // Tap exits naturally on `[IDLE]`; we don't await it here.
    }

    // Close the task pearl if we early-return before the tokio::spawn
    // reaches the runner. Otherwise the pearl leaks as permanent
    // in_progress — that's the E2E-"Task:" leak we cleaned up in th-28edd8.
    // Clone the store for the closure so the original can move into the
    // later tokio::spawn; both point at the same Arc<Dolt>.
    let pearl_store_for_abort = pearl_store.clone();
    let pearl_id_for_abort = pearl_id.clone();
    let close_pearl_on_abort = |reason: &str| {
        if let Some(ref id) = pearl_id_for_abort {
            tracing::warn!(pearl_id = %id, reason, "closing task pearl due to early-return failure");
            let _ = pearl_store_for_abort.close(&[id]);
        }
    };

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
            close_pearl_on_abort(err);
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
            let msg = format!("failed to create host workspace {}: {e}", host_workspace.display());
            let _ = state.event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: msg.clone(),
            });
            close_pearl_on_abort(&msg);
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
        close_pearl_on_abort("runner has no parent dir");
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
            let msg = format!("no LLM provider configured: {e}");
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: tid.clone(),
                message: msg.clone(),
            });
            close_pearl_on_abort(&msg);
            return;
        }
    };

    // Build session-resume context BEFORE the tokio::spawn so we don't
    // have to smuggle a state reference through the 'static boundary.
    // Reads the pearl's prior SessionMessages (if any) and renders them
    // as a "## Resumption context" block that gets prepended to the
    // task message so the agent can pick up where prior invocations
    // left off.
    let resumption_context = build_resumption_context(&state.session_store, pearl_id.as_deref(), 20);

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
        // Note: SMOOTH_TASK is set later, *after* we know whether the task
        // message is small enough to fit in an env var or needs to land in a
        // tempfile mounted at /opt/smooth/policy/task.txt. The kernel cmdline
        // microsandbox builds for the VM has a hard size limit (~2 KB on
        // aarch64), and a long task message (e.g. 1.5 KB of agent
        // instructions) will overflow it and panic msb_krun_vmm with
        // `TooLarge` before the VM ever boots.
        // SMOOTH_API_URL stays in plain env — it's a URL, not a secret.
        // SMOOTH_API_KEY is wired as a microsandbox Secret below so the
        // guest only ever sees a placeholder; the real value is
        // substituted by the network layer on outbound requests to the
        // LLM gateway host. That defuses the "agent reads its own env
        // and exfils the credential" attack.
        env.insert("SMOOTH_API_URL".into(), api_url.clone());
        env.insert("SMOOTH_MODEL".into(), final_model);
        // Role selection — the runner falls back to `fixer` on unset
        // or empty, so we only plumb this when the caller explicitly
        // picked one.
        if let Some(ref agent_name) = agent {
            if !agent_name.trim().is_empty() {
                env.insert("SMOOTH_AGENT".into(), agent_name.clone());
            }
        }

        // Derive the LLM gateway host from the API URL so we only
        // substitute the secret for that destination. Minimal
        // string parse — avoids pulling in a full URL crate just
        // for host extraction.
        let llm_host = extract_host_from_url(&api_url);
        let api_key_secret = crate::sandbox::SecretConfig {
            env_var: "SMOOTH_API_KEY".into(),
            value: api_key,
            // A deterministic sentinel — if this ever shows up on a
            // server log, something routed past the microsandbox
            // substitution and we want it obvious.
            placeholder: "SMOOTH_PLACEHOLDER_API_KEY_NOT_SUBSTITUTED".into(),
            allowed_hosts: if llm_host.is_empty() { Vec::new() } else { vec![llm_host] },
        };
        env.insert("SMOOTH_WORKSPACE".into(), "/workspace".into());
        env.insert("SMOOTH_OPERATOR_ID".into(), tid.clone());

        // The multi-phase workflow is the product — always on. The
        // routing config itself is serialized as a file further down
        // (after policy_dir_guard is set up) because the JSON is
        // several kilobytes and would overflow the kernel cmdline if
        // passed as an env var.
        //
        // `SMOOTH_WORKFLOW=0` is kept as an opt-OUT for debugging
        // and regression bisects — it falls back to the classic
        // single-Agent loop in `smooth-operator-runner`. Anything
        // else (unset, "1", "true") turns it on.
        let workflow_disabled = std::env::var("SMOOTH_WORKFLOW")
            .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
            .unwrap_or(false);
        if !workflow_disabled {
            env.insert("SMOOTH_WORKFLOW".into(), "1".into());
            // Pass through the TEST-phase skip flag for benchmark
            // runs where adding extra tests would change the score.
            if let Ok(skip) = std::env::var("SMOOTH_WORKFLOW_SKIP_TEST") {
                env.insert("SMOOTH_WORKFLOW_SKIP_TEST".into(), skip);
            }
            if let Ok(iters) = std::env::var("SMOOTH_WORKFLOW_MAX_ITERATIONS") {
                env.insert("SMOOTH_WORKFLOW_MAX_ITERATIONS".into(), iters);
            }
            if let Ok(iters) = std::env::var("SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS") {
                env.insert("SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS".into(), iters);
            }
        }
        // Tell the operator where ~/.smooth is mounted inside the VM.
        env.insert("SMOOTH_HOME".into(), "/root/.smooth".into());
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

        // Every operator's Wonk escalates uncertain /check/* decisions to
        // the central Boardroom Narc via this URL. From inside the VM, the
        // host's loopback is reachable as `host.containers.internal` in
        // Boardroom mode (Bill passes it through) and as `127.0.0.1` in
        // host-mode (Direct sandbox backend on the same machine). The port
        // is Big Smooth's listening port, which at the time of this writing
        // is always 4400. An override via SMOOTH_NARC_URL short-circuits
        // both cases — useful for tests and for pointing several boards at
        // a shared Narc.
        let narc_url = if let Ok(override_url) = std::env::var("SMOOTH_NARC_URL") {
            if override_url.trim().is_empty() {
                None
            } else {
                Some(override_url)
            }
        } else {
            let host = if boardroom_handles.is_some() {
                "host.containers.internal"
            } else {
                "127.0.0.1"
            };
            Some(format!("http://{host}:4400"))
        };
        if let Some(ref url) = narc_url {
            tracing::info!(task_id = tid, url = %url, "operator env: SMOOTH_NARC_URL set");
            env.insert("SMOOTH_NARC_URL".into(), url.clone());
        }

        // Hand the operator its pearl id so the in-VM mailbox poller knows
        // which pearl to read steering/chat comments from. When this is unset
        // (e.g. dispatch ran without Diver and PearlStore creation failed),
        // the runner falls through to legacy behaviour with no mailbox.
        if let Some(ref pid) = pearl_id {
            tracing::info!(task_id = tid, pearl_id = %pid, "operator env: SMOOTH_PEARL_ID set");
            env.insert("SMOOTH_PEARL_ID".into(), pid.clone());
        }

        // Pass through the host-tool bearer so the teammate's `host_tool`
        // can hit Big Smooth's `/api/host/exec` for whitelisted CLIs (gh,
        // git, kubectl, …). See `crate::host_tools` for the security
        // model — secrets stay on the host, sandbox sees only output.
        if let Ok(host_token) = std::env::var("SMOOTH_HOST_TOKEN") {
            env.insert("SMOOTH_HOST_TOKEN".into(), host_token);
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
        //
        // CRITICAL: the tempdir lives under `~/.smooth/control/`, NOT
        // the system `$TMPDIR`. On macOS `$TMPDIR` resolves to
        // `/var/folders/…/T` which is SIP-protected — microsandbox silently
        // drops bind-mounts rooted there, and the operator-runner boots to
        // find `/opt/smooth/policy` empty. The project-cache bind-mount
        // uses user-home paths for the same reason. See th-b1f040 for the
        // full debugging trail.
        let mut policy_dir_guard: Option<tempfile::TempDir> = None;
        // Best-effort — if we can't find a home dir or mkdir fails, we
        // fall back to the system tempdir (older broken behavior, still
        // better than panicking).
        let control_root: Option<std::path::PathBuf> = dirs_next::home_dir().and_then(|h| {
            let p = h.join(".smooth").join("control");
            match std::fs::create_dir_all(&p) {
                Ok(()) => Some(p),
                Err(e) => {
                    tracing::warn!(error = %e, path = %p.display(), "could not mkdir control root; falling back to $TMPDIR (microsandbox bind-mounts will likely fail on macOS)");
                    None
                }
            }
        });
        let operator_token = crate::policy::generate_operator_token(&tid);
        // Build mount mappings so Wonk can translate guest paths to host
        // paths when checking filesystem deny patterns.
        let policy_mounts = {
            let mut pm = vec![smooth_policy::MountMapping {
                guest_path: "/workspace".into(),
                host_path: workspace_canon.clone(),
            }];
            // Mirror the ~/.smooth mount if it exists on the host.
            if let Some(host_smooth) = std::env::var("SMOOTH_HOME_HOST_PATH").ok().or_else(|| {
                if brokered {
                    None
                } else {
                    dirs_next::home_dir()
                        .map(|h| h.join(".smooth").to_string_lossy().to_string())
                        .filter(|p| std::path::Path::new(p).exists())
                }
            }) {
                pm.push(smooth_policy::MountMapping {
                    guest_path: "/root/.smooth".into(),
                    host_path: host_smooth,
                });
            }
            pm
        };
        match crate::policy::generate_policy_for_task(
            &tid,
            &pearl_id.clone().unwrap_or_default(),
            "execute",
            &operator_token,
            &[],
            crate::policy::TaskType::Coding,
            policy_mounts,
        ) {
            Ok(policy_toml) => match make_control_tempdir("smooth-policy-", control_root.as_deref()) {
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

        // Make sure we have *some* tempdir in non-boardroom mode so we can
        // hand the task message to the runner via a file (avoids the
        // kernel-cmdline size limit on long messages). If policy generation
        // failed earlier, fall back to a bare tempdir here.
        if !in_boardroom && policy_dir_guard.is_none() {
            if let Ok(dir) = make_control_tempdir("smooth-control-", control_root.as_deref()) {
                policy_dir_guard = Some(dir);
            }
        }

        // Combine the user's task with any resumption context we loaded
        // up top. Empty `resumption_context` means no prior session, so
        // we just use the message as-is.
        let full_task_message = if resumption_context.is_empty() {
            message.clone()
        } else {
            format!("{message}\n\n{resumption_context}")
        };

        // Write the task message to a file in the control tempdir so the
        // runner can read it via SMOOTH_TASK_FILE. The kernel cmdline that
        // microsandbox builds for the VM has a hard size limit (~2 KB on
        // aarch64) and a long task (e.g. 1.5 KB of agent instructions) will
        // overflow it and panic msb_krun_vmm before the VM boots. The file
        // path keeps the cmdline tiny regardless of message size.
        let task_file_set = if let Some(ref dir) = policy_dir_guard {
            let task_path = dir.path().join("task.txt");
            match std::fs::write(&task_path, full_task_message.as_bytes()) {
                Ok(()) => {
                    env.insert("SMOOTH_TASK_FILE".into(), "/opt/smooth/policy/task.txt".into());
                    true
                }
                Err(e) => {
                    tracing::warn!(task_id = tid, error = %e, "failed to write task tempfile; falling back to SMOOTH_TASK env var");
                    false
                }
            }
        } else {
            false
        };
        if !task_file_set {
            // Boardroom mode or tempdir creation failed: stuff the task in an
            // env var. This still works for short messages but will overflow
            // the kernel cmdline for long ones — Boardroom mode needs a
            // brokered task-file path eventually.
            env.insert("SMOOTH_TASK".into(), full_task_message.clone());
        }

        // When the workflow is enabled, write the full ProviderRegistry
        // JSON to the policy bind-mount so the runner can load it at
        // `/opt/smooth/policy/routing.json`. Too big (~3 KB) to fit
        // in the kernel cmdline via an env var; same reason task.txt
        // is mounted rather than inlined.
        if !workflow_disabled {
            if let Some(ref dir) = policy_dir_guard {
                if let Some(home) = dirs_next::home_dir() {
                    let providers_path = home.join(".smooth/providers.json");
                    match smooth_operator::providers::ProviderRegistry::load_from_file(&providers_path) {
                        Ok(mut registry) => {
                            // See dispatch_ws_task_direct for why: the workflow uses
                            // routing.coding to resolve the LLM, so opts.model only
                            // takes effect when we patch the slot here.
                            if let Some(ref m) = model {
                                registry.routing.coding.model = m.clone();
                                tracing::info!(task_id = tid, model = %m, "sandboxed dispatch: overrode routing.coding.model from opts.model");
                            }
                            match registry.to_json() {
                                Ok(json) => {
                                    let routing_path = dir.path().join("routing.json");
                                    if let Err(e) = std::fs::write(&routing_path, json) {
                                        tracing::warn!(task_id = tid, error = %e, "could not write routing.json; workflow will fall back to single-Agent");
                                    } else {
                                        env.insert("SMOOTH_ROUTING_JSON_FILE".into(), "/opt/smooth/policy/routing.json".into());
                                        tracing::info!(task_id = tid, "coding workflow enabled: routing.json mounted for runner");
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(task_id = tid, error = %e, "could not serialize routing config; workflow disabled");
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(task_id = tid, error = %e, "could not load providers.json; workflow disabled");
                        }
                    }
                }
            } else {
                tracing::warn!(
                    task_id = tid,
                    "SMOOTH_WORKFLOW=1 but no policy dir (Boardroom mode); routing config path not plumbed yet"
                );
            }
        }

        let policy_mount = if !in_boardroom {
            if let Some(ref dir) = policy_dir_guard {
                let host = dir
                    .path()
                    .canonicalize()
                    .unwrap_or_else(|_| dir.path().to_path_buf())
                    .to_string_lossy()
                    .to_string();
                // Only point at the policy file if we actually wrote one
                // (the task tempfile may live in a bare control tempdir
                // when policy generation failed earlier).
                if dir.path().join("policy.toml").exists() {
                    env.insert("SMOOTH_POLICY_FILE".into(), "/opt/smooth/policy/policy.toml".into());
                }
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
        // Pre-assign host ports for port forwarding declared in the policy.
        // We parse the generated policy TOML to extract the port config, then
        // pre-bind host ports so we can inject SMOOTH_PORT_MAP into the VM's
        // env at creation time (env vars can't be added after boot).
        let mut extra_ports = Vec::new();
        let mut port_map_entries: Vec<String> = Vec::new();
        if let Some(ref dir) = policy_dir_guard {
            let policy_file = dir.path().join("policy.toml");
            if let Ok(toml_str) = std::fs::read_to_string(&policy_file) {
                if let Ok(policy) = smooth_policy::Policy::from_toml(&toml_str) {
                    if policy.ports.enabled {
                        // Load any cached mapping for this pearl. If a previous
                        // task on the same pearl forwarded guest_port=3000 to
                        // host_port=54321, we'll try to reserve 54321 again so
                        // "check on the dev server tomorrow" gets the same URL.
                        let cache_key = pearl_id.clone().unwrap_or_else(|| tid.clone());
                        let mut cache = crate::port_cache::load(&cache_key);

                        // Pre-declare common dev server ports so `th run --keep-alive`
                        // exposes them to the host without the agent needing to call
                        // forward_port. If you start `pnpm dev` inside the VM on
                        // port 3000, you can hit it on `http://localhost:<host_port>`.
                        //
                        // Covers: Next.js/Express (3000/3001), Nuxt/Vite preview (4000),
                        // Angular (4200), Flask/FastAPI (5000), Vite (5173),
                        // Django/Python http (8000), generic (8080/8888).
                        let common_ports: Vec<u16> = vec![3000, 3001, 4000, 4200, 5000, 5173, 8000, 8080, 8888];
                        for guest_port in common_ports {
                            if !policy.ports.can_forward(guest_port) || extra_ports.len() >= policy.ports.max_forwards as usize {
                                continue;
                            }
                            // Prefer the cached host port if still free. Fall
                            // back to an ephemeral port otherwise.
                            let host_port = cache
                                .get(&guest_port)
                                .and_then(|p| crate::port_cache::try_reserve(*p))
                                .or_else(crate::port_cache::reserve_ephemeral);
                            let Some(host_port) = host_port else {
                                continue;
                            };
                            extra_ports.push(smooth_bootstrap_bill::protocol::PortMapping {
                                host_port,
                                guest_port,
                                bind_all: false,
                            });
                            port_map_entries.push(format!("{guest_port}:{host_port}"));
                            cache.insert(guest_port, host_port);
                        }
                        // Persist the updated mapping. Subsequent dispatches on
                        // the same pearl will try these host ports first.
                        crate::port_cache::save(&cache_key, &cache);
                        if !port_map_entries.is_empty() {
                            env.insert("SMOOTH_PORT_MAP".into(), port_map_entries.join(","));
                            tracing::info!(task_id = tid, pearl = %cache_key, ports = %port_map_entries.join(","), "port forwarding: pre-mapped ports (persisted per-pearl)");
                        }
                    }
                }
            }
        }

        // pearl th-461ab9 (diag): when SMOOTH_DIAG_RUNNER_LOGS=1, allocate a
        // host-side dir at ~/.smooth/runner-logs/<task_id>/ that the wrapper
        // script will tee runner stdout/stderr into. Bind-mounted into the
        // VM at /var/log/smooth-runner. Lets us recover guest output even
        // when sandbox.exec returns no data (Mode B's "VM booted but
        // nothing happened" pattern), AND tells us whether bind-mounts
        // landed at all (presence-of-file in the host path proves the
        // wrapper ran inside the guest).
        let diag_runner_logs_enabled = std::env::var("SMOOTH_DIAG_RUNNER_LOGS").map(|v| diag_flag_is_truthy(&v)).unwrap_or(false);
        let diag_runner_logs_host: Option<String> = if diag_runner_logs_enabled {
            dirs_next::home_dir().and_then(|h| {
                let p = h.join(".smooth").join("runner-logs").join(&tid);
                match std::fs::create_dir_all(&p) {
                    Ok(()) => {
                        tracing::info!(task_id = tid, path = %p.display(), "diag runner-logs: host dir ready");
                        Some(p.to_string_lossy().to_string())
                    }
                    Err(e) => {
                        tracing::warn!(task_id = tid, error = %e, "diag runner-logs: could not create host dir; logs disabled");
                        None
                    }
                }
            })
        } else {
            None
        };

        let config = SandboxConfig {
            bead_id: pearl_id.clone().unwrap_or_default(),
            workspace_path: "/workspace".into(),
            env,
            extra_ports,
            mounts: {
                let mut m = vec![
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
                ];
                // pearl th-461ab9 (diag): runner-logs bind mount. RW because
                // we need the wrapper to tee into it.
                if let Some(ref host_path) = diag_runner_logs_host {
                    m.push(BindMount {
                        host_path: host_path.clone(),
                        guest_path: "/var/log/smooth-runner".into(),
                        readonly: false,
                    });
                }
                // Mount ~/.smooth for global config, registry, and pearl access.
                // RW so operators can update pearls, write audit logs, etc.
                //
                // In brokered mode (Boardroom VM), we can't resolve the host
                // home directory — dirs_next gives the guest /root. Use
                // SMOOTH_HOME_HOST_PATH if set (the launcher/test harness sets
                // it to the real host ~/.smooth path). In host mode, resolve
                // directly.
                let smooth_home_host = std::env::var("SMOOTH_HOME_HOST_PATH").ok().or_else(|| {
                    if brokered {
                        None // can't resolve host path from inside VM
                    } else {
                        dirs_next::home_dir()
                            .map(|h| h.join(".smooth").to_string_lossy().to_string())
                            .filter(|p| std::path::Path::new(p).exists())
                    }
                });
                if let Some(host_path) = smooth_home_host {
                    m.push(BindMount {
                        host_path,
                        guest_path: "/root/.smooth".into(),
                        readonly: false,
                    });
                }
                m.into_iter().chain(policy_mount).collect()
            },
            allow_host_loopback: true,
            // Project-scoped cache key. Resolution order:
            //   1. SMOOTH_ENV_CACHE_KEY env var (test harness stable key)
            //   2. project_cache_key(workspace) — hash of the
            //      canonical workspace path. This means the
            //      budgeting-app repo always gets the same cache
            //      directory across pearls, and the smooth-monorepo
            //      repo gets its own. Much more useful than
            //      pearl-id-per-cache which lost all prior install
            //      state between tasks.
            //   3. Task id fallback (ephemeral, only when workspace is
            //      absent — e.g. one-off chat dispatch).
            env_cache_key: std::env::var("SMOOTH_ENV_CACHE_KEY")
                .ok()
                .filter(|k| !k.is_empty())
                .or_else(|| project_cache_key(&workspace_canon))
                .or_else(|| Some(tid.clone())),
            image: image.clone(),
            memory_mb: memory_mb.unwrap_or(SandboxConfig::default().memory_mb),
            secrets: vec![api_key_secret],
            // Named microsandbox Volume is the default backend for
            // the project cache (th-266809 flipped this after the CLI
            // learned both backends in th-fb7bec). Setting
            // `SMOOTH_USE_VOLUMES=0|false|no|off` opts back into the
            // legacy bind-mount path (~/.smooth/project-cache/<key>).
            use_named_volume_for_cache: std::env::var("SMOOTH_USE_VOLUMES")
                .ok()
                .map(|v| !matches!(v.trim(), "0" | "false" | "FALSE" | "no" | "off"))
                .unwrap_or(true),
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

        // Preflight: exec a trivial shell command to verify the
        // bind-mount landed and the guest can exec anything at all.
        // When the runner fails with code=-1 and empty stdout/stderr,
        // this tells us whether it's a runner-binary issue or an
        // exec-layer issue.
        let preflight_script = format!(
            "echo '/opt contents:'; ls -la /opt/ 2>&1 | head -10; \
             echo '/opt/smooth contents:'; ls -la /opt/smooth/ 2>&1 | head -10; \
             echo '/opt/smooth/policy contents:'; ls -la /opt/smooth/policy/ 2>&1 | head -20; \
             echo '/opt/smooth/bin contents:'; ls -la /opt/smooth/bin/ 2>&1 | head -5; \
             echo '/workspace contents:'; ls /workspace/ 2>&1 | head -8; \
             echo 'mounts:'; mount 2>&1 | grep -E 'opt/smooth|workspace' | head -10; \
             echo 'runner check:'; test -x {runner_in_vm} && echo 'runner is executable' || echo 'runner missing or not executable'"
        );
        match sandbox::exec_in_sandbox(&handle.msb_name, &["/bin/sh", "-c", preflight_script.as_str()]).await {
            Ok((out, err, code)) => {
                tracing::info!(
                    task_id = tid,
                    preflight_code = code,
                    preflight_stdout = %out.chars().take(2000).collect::<String>(),
                    preflight_stderr = %err.chars().take(2000).collect::<String>(),
                    "sandboxed dispatch: preflight shell check"
                );
            }
            Err(e) => {
                tracing::warn!(task_id = tid, error = %e, "sandboxed dispatch: preflight failed outright");
            }
        }

        let _ = event_tx.send(ServerEvent::ToolCallStart {
            task_id: tid.clone(),
            tool_name: "sandbox.exec".into(),
            arguments: runner_in_vm.clone(),
        });

        let exec_started = std::time::Instant::now();
        // pearl th-461ab9 (diag): when runner-logs is enabled, wrap the
        // runner exec in a sh script that snapshots /opt mounts + env,
        // then runs the runner with stdout/stderr tee'd into the
        // bind-mounted /var/log/smooth-runner/. The runner's stdout
        // (JSON AgentEvents) still flows back through agent.sock so the
        // existing parser keeps working — we only ADD a copy on disk.
        // If the bind-mount silently fails to land, the host dir stays
        // empty and that itself is a diagnostic signal.
        let exec_argv: Vec<String> = if diag_runner_logs_host.is_some() {
            tracing::info!(task_id = tid, "diag runner-logs: dispatching runner via tee wrapper script");
            build_runner_diag_wrapper_argv(&runner_in_vm)
        } else {
            vec![runner_in_vm.clone()]
        };
        let exec_argv_refs: Vec<&str> = exec_argv.iter().map(String::as_str).collect();
        let (stdout, stderr, code) = match sandbox::exec_in_sandbox(&handle.msb_name, &exec_argv_refs).await {
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
        // Accumulates the final cost reported by the runner's
        // `AgentEvent::Completed`. Runner emits the real cost; a
        // fallback emit (exit-path race) reports 0.0 so we don't
        // over-count — we take the max across all Completed events.
        let mut final_cost_usd: f64 = 0.0;
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
                            if let Some(c) = event.get("cost_usd").and_then(serde_json::Value::as_f64) {
                                if c > final_cost_usd {
                                    final_cost_usd = c;
                                }
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
                        "PortForwardActive" => {
                            let guest = event.get("guest_port").and_then(serde_json::Value::as_u64).unwrap_or(0) as u16;
                            let host = event.get("host_port").and_then(serde_json::Value::as_u64).unwrap_or(0) as u16;
                            tracing::info!(task_id = tid, guest_port = guest, host_port = host, "port forward active");
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

        if keep_alive {
            // Keep the VM alive so the user can poke at running
            // dev servers etc. via forwarded ports. Register in the
            // orchestrator's active_workers so DELETE /api/workers/<id>
            // can find and tear it down.
            {
                let mut orch = orchestrator.lock().await;
                orch.active_workers.insert(handle.operator_id.clone(), handle.clone());
            }
            let _ = event_tx.send(ServerEvent::TokenDelta {
                task_id: tid.clone(),
                content: format!(
                    "[keep-alive] operator VM {} staying up; stop with `th operators kill {}`.\n",
                    handle.operator_id, handle.operator_id
                ),
            });
            tracing::info!(task_id = tid, operator = %handle.operator_id, "sandboxed dispatch: keep-alive mode, VM left running");
            touch();
            return;
        }

        if let Err(e) = sandbox::destroy_sandbox(&handle.msb_name).await {
            tracing::warn!(task_id = tid, error = %e, "sandboxed dispatch: destroy_sandbox failed");
        }

        // Exit code 0 = runner finished successfully. `saw_completed` is the
        // in-band signal from AgentEvent::Completed, but the runner also emits
        // it explicitly before exit. Treat exit 0 as success regardless — a
        // clean exit means the agent loop returned Ok.
        if code == 0 {
            if !saw_completed {
                tracing::warn!(task_id = tid, "runner exited 0 but no Completed event seen in stdout — treating as success");
            }
            let _ = event_tx.send(ServerEvent::TaskComplete {
                task_id: tid.clone(),
                iterations: agent_iterations,
                cost_usd: final_cost_usd,
            });
            // Close pearl via Diver or directly
            if let Some(ref id) = pearl_id {
                if let Some(ref diver_client) = diver {
                    if let Err(e) = diver_client.complete(id, Some("Task completed successfully"), None).await {
                        tracing::warn!(error = %e, "diver complete failed, falling back to direct close");
                        let _ = pearl_store.close(&[id]);
                    }
                } else {
                    let _ = pearl_store.close(&[id]);
                }
            }
            tracing::info!(task_id = tid, iterations = agent_iterations, "sandboxed WS task completed");
        } else {
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: tid.clone(),
                message: format!("sandboxed runner exited with code {code}"),
            });
            tracing::error!(
                task_id = tid,
                exit = code,
                stderr = %stderr.lines().take(20).collect::<Vec<_>>().join("\n"),
                "sandboxed WS task failed"
            );
            // Close the pearl on failure too, otherwise E2E runs leak
            // "Task: ..." pearls that stay in_progress forever.
            if let Some(ref id) = pearl_id {
                if let Some(ref diver_client) = diver {
                    if let Err(e) = diver_client
                        .complete(id, Some(&format!("sandboxed runner exited with code {code}")), None)
                        .await
                    {
                        tracing::warn!(error = %e, "diver complete failed on task error, falling back to direct close");
                        let _ = pearl_store.close(&[id]);
                    }
                } else {
                    let _ = pearl_store.close(&[id]);
                }
            }
        }

        touch();
    });
}

/// Spawn the operator runner as a direct subprocess against the
/// host workspace. No microVM, no bind mounts, no Narc/Wonk/Goalie
/// enforcement — the agent has host-level tool access. Intended
/// for bench runs and fast dev loops. Opt-in via
/// `SMOOTH_WORKFLOW_DIRECT=1` (checked by `dispatch_ws_task`).
///
/// Trade-off vs. `dispatch_ws_task_sandboxed`:
///   + No VM boot (~10-30s faster per task)
///   + Runs in nested-virt environments where microsandbox can't
///     (GitHub Actions Linux runners, k8s pods, most cloud VMs)
///   + No bind-mount overhead
///   - No hardware isolation — agent can reach the whole host
///   - No Goalie/Wonk/Narc enforcement of tool policy
///
/// Wiring: we stream stdout line-by-line via tokio's async pipe
/// reader and translate each `AgentEvent` JSON line to the same
/// `ServerEvent` shape the sandboxed path emits. The output-parsing
/// logic is intentionally identical to the sandboxed branch — the
/// runner is the same binary emitting the same event stream; we
/// just don't wrap the exec in a VM.
#[allow(clippy::too_many_lines)]
async fn dispatch_ws_task_direct(state: &AppState, opts: DispatchOptions) {
    use std::process::Stdio;
    use tokio::io::AsyncBufReadExt;

    let DispatchOptions {
        message,
        model,
        budget,
        working_dir,
        image: _,
        keep_alive: _,
        memory_mb: _,
        agent,
        pearl_id: pearl_id_in,
    } = opts;

    let task_id = uuid::Uuid::new_v4().to_string();
    let event_tx = state.event_tx.clone();
    let pearl_store = state.pearl_store.clone();
    let last_activity = state.last_activity.clone();

    // Pearl bookkeeping — mirrors the sandboxed path so runs show
    // up in `th pearls` the same way. Caller-supplied pearl wins
    // (the chat-agent's teammate_spawn passes the pearls_create id);
    // otherwise Diver, then direct PearlStore.
    let diver = state.diver.clone();
    let pearl_id: Option<String> = if let Some(supplied) = pearl_id_in {
        tracing::info!(pearl_id = %supplied, "direct dispatch: reusing caller-supplied pearl");
        let _ = pearl_store.update(
            &supplied,
            &smooth_pearls::PearlUpdate {
                status: Some(smooth_pearls::PearlStatus::InProgress),
                ..Default::default()
            },
        );
        Some(supplied)
    } else if let Some(ref diver_client) = diver {
        match diver_client.dispatch(&format!("Task: {}", truncate_str(&message, 60)), &message, None).await {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(error = %e, "direct dispatch: Diver dispatch failed, falling back to direct PearlStore");
                crate::pearls::create_pearl(&pearl_store, &format!("Task: {}", truncate_str(&message, 60)), &message, "task", 2)
                    .ok()
                    .map(|i| i.id)
            }
        }
    } else {
        let id = crate::pearls::create_pearl(&pearl_store, &format!("Task: {}", truncate_str(&message, 60)), &message, "task", 2)
            .ok()
            .map(|i| i.id);
        if let Some(ref id) = id {
            let _ = pearl_store.update(
                id,
                &smooth_pearls::PearlUpdate {
                    status: Some(smooth_pearls::PearlStatus::InProgress),
                    ..Default::default()
                },
            );
        }
        id
    };

    let pearl_store_for_abort = pearl_store.clone();
    let pearl_id_for_abort = pearl_id.clone();
    let close_pearl_on_abort = |reason: &str| {
        if let Some(ref id) = pearl_id_for_abort {
            tracing::warn!(pearl_id = %id, reason, "closing task pearl (direct dispatch early return)");
            let _ = pearl_store_for_abort.close(&[id]);
        }
    };

    // Resolve runner binary + workspace as host paths.
    let runner_bin = match find_native_operator_runner_binary() {
        Some(p) => p,
        None => {
            let err = "native smooth-operator-runner not found. Run `cargo build -p smooai-smooth-operator-runner` (debug) or `--release`, or set SMOOTH_OPERATOR_RUNNER_NATIVE=/absolute/path.";
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: err.into(),
            });
            tracing::error!("direct dispatch: {err}");
            close_pearl_on_abort(err);
            return;
        }
    };

    let host_workspace: std::path::PathBuf = working_dir
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    if !host_workspace.exists() {
        if let Err(e) = std::fs::create_dir_all(&host_workspace) {
            let msg = format!("failed to create workspace {}: {e}", host_workspace.display());
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: msg.clone(),
            });
            close_pearl_on_abort(&msg);
            return;
        }
    }
    let workspace_canon = host_workspace.canonicalize().unwrap_or_else(|_| host_workspace.clone());

    let (api_url, api_key, final_model) = match load_llm_config_for_runner(&model) {
        Ok(x) => x,
        Err(e) => {
            let msg = format!("no LLM provider configured: {e}");
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: msg.clone(),
            });
            close_pearl_on_abort(&msg);
            return;
        }
    };

    // Build a host tempdir that carries the task message + routing
    // config. Tempdir's Drop removes it when this function returns,
    // which is AFTER we've waited for the subprocess, so the files
    // are safe for the runner's lifetime.
    let control_dir = match tempfile::Builder::new().prefix("smooth-direct-").tempdir() {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("failed to create control tempdir: {e}");
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: msg.clone(),
            });
            close_pearl_on_abort(&msg);
            return;
        }
    };

    let resumption_context = build_resumption_context(&state.session_store, pearl_id.as_deref(), 20);
    let full_task_message = if resumption_context.is_empty() {
        message.clone()
    } else {
        format!("{message}\n\n{resumption_context}")
    };

    let task_path = control_dir.path().join("task.txt");
    if let Err(e) = std::fs::write(&task_path, full_task_message.as_bytes()) {
        let msg = format!("failed to write task file: {e}");
        let _ = event_tx.send(ServerEvent::TaskError {
            task_id: task_id.clone(),
            message: msg.clone(),
        });
        close_pearl_on_abort(&msg);
        return;
    }

    // Write routing.json so the runner's workflow path picks up
    // the provider registry (needed for the coding slot).
    //
    // When the caller passes `opts.model`, override the `coding` slot's
    // model before serializing. The runner's `coding_workflow` resolves
    // the LLM via `routing.coding`, so without this override the model
    // override is ignored and the runner uses the default coding model
    // (smooth-coding) regardless of what `teammate_spawn(model=...)`
    // requested. That made simple lookups dispatch on a slow reasoning
    // model and look like a hang.
    let routing_path = control_dir.path().join("routing.json");
    if let Some(home) = dirs_next::home_dir() {
        let providers_path = home.join(".smooth/providers.json");
        match smooth_operator::providers::ProviderRegistry::load_from_file(&providers_path) {
            Ok(mut registry) => {
                if let Some(ref m) = model {
                    registry.routing.coding.model = m.clone();
                    tracing::info!(model = %m, "direct dispatch: overrode routing.coding.model from opts.model");
                }
                match registry.to_json() {
                    Ok(json) => {
                        if let Err(e) = std::fs::write(&routing_path, json) {
                            tracing::warn!(error = %e, "direct dispatch: failed to write routing.json; workflow will fall back to classic agent");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "direct dispatch: failed to serialize routing config"),
                }
            }
            Err(e) => tracing::warn!(error = %e, "direct dispatch: failed to load providers.json"),
        }
    }

    let tid = task_id.clone();

    tokio::spawn(async move {
        let _control_dir = control_dir; // keep alive

        // Build the runner's environment. Deliberately minimal —
        // the direct path doesn't need Narc/Wonk/Goalie URLs.
        let mut cmd = tokio::process::Command::new(&runner_bin);
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()))
            .env("HOME", std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
            .env("SMOOTH_API_URL", &api_url)
            .env("SMOOTH_API_KEY", &api_key)
            .env("SMOOTH_MODEL", &final_model)
            .env("SMOOTH_OPERATOR_ID", &tid)
            .env("SMOOTH_WORKSPACE", workspace_canon.to_string_lossy().as_ref())
            .env("SMOOTH_TASK_FILE", task_path.to_string_lossy().as_ref())
            .env("SMOOTH_WORKFLOW", "1")
            .env("SMOOTH_ROUTING_JSON_FILE", routing_path.to_string_lossy().as_ref());
        if let Some(b) = budget {
            cmd.env("SMOOTH_BUDGET_USD", b.to_string());
        }
        if let Some(ref agent_name) = agent {
            if !agent_name.trim().is_empty() {
                cmd.env("SMOOTH_AGENT", agent_name);
            }
        }
        if let Ok(skip) = std::env::var("SMOOTH_WORKFLOW_SKIP_TEST") {
            cmd.env("SMOOTH_WORKFLOW_SKIP_TEST", skip);
        }
        if let Ok(iters) = std::env::var("SMOOTH_WORKFLOW_MAX_ITERATIONS") {
            cmd.env("SMOOTH_WORKFLOW_MAX_ITERATIONS", iters);
        }
        if let Ok(iters) = std::env::var("SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS") {
            cmd.env("SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS", iters);
        }
        if let Some(ref pid) = pearl_id {
            cmd.env("SMOOTH_PEARL_ID", pid);
        }
        if let Ok(host_token) = std::env::var("SMOOTH_HOST_TOKEN") {
            cmd.env("SMOOTH_HOST_TOKEN", host_token);
        }
        if let Some(home) = dirs_next::home_dir() {
            let smooth_home = home.join(".smooth");
            if smooth_home.exists() {
                cmd.env("SMOOTH_HOME", smooth_home.to_string_lossy().as_ref());
            }
        }
        cmd.current_dir(&workspace_canon)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: tid.clone(),
                    message: format!("failed to spawn runner: {e}"),
                });
                return;
            }
        };

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let mut out_reader = tokio::io::BufReader::new(stdout).lines();
        let mut err_reader = tokio::io::BufReader::new(stderr).lines();

        let event_tx_out = event_tx.clone();
        let tid_out = tid.clone();
        let last_activity_for_stdout = last_activity.clone();
        let stdout_task = tokio::spawn(async move {
            let mut agent_iterations: u32 = 0;
            let mut final_cost_usd: f64 = 0.0;
            while let Ok(Some(line)) = out_reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(mut la) = last_activity_for_stdout.lock() {
                    *la = std::time::Instant::now();
                }
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(event) => {
                        let Some(ty) = event.get("type").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        match ty {
                            "TokenDelta" => {
                                if let Some(content) = event.get("content").and_then(|v| v.as_str()) {
                                    let _ = event_tx_out.send(ServerEvent::TokenDelta {
                                        task_id: tid_out.clone(),
                                        content: content.to_string(),
                                    });
                                }
                            }
                            "ToolCallStart" => {
                                let tool_name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let _ = event_tx_out.send(ServerEvent::ToolCallStart {
                                    task_id: tid_out.clone(),
                                    tool_name,
                                    arguments: String::new(),
                                });
                            }
                            "ToolCallComplete" => {
                                let tool_name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let is_error = event.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false);
                                let _ = event_tx_out.send(ServerEvent::ToolCallComplete {
                                    task_id: tid_out.clone(),
                                    tool_name,
                                    result: String::new(),
                                    is_error,
                                    duration_ms: 0,
                                });
                            }
                            "Completed" => {
                                if let Some(iters) = event.get("iterations").and_then(serde_json::Value::as_u64) {
                                    agent_iterations = u32::try_from(iters).unwrap_or(u32::MAX);
                                }
                                if let Some(c) = event.get("cost_usd").and_then(serde_json::Value::as_f64) {
                                    if c > final_cost_usd {
                                        final_cost_usd = c;
                                    }
                                }
                            }
                            "Error" => {
                                if let Some(msg) = event.get("message").and_then(|v| v.as_str()) {
                                    let _ = event_tx_out.send(ServerEvent::TaskError {
                                        task_id: tid_out.clone(),
                                        message: msg.to_string(),
                                    });
                                }
                            }
                            _ => {} // informational; not forwarded
                        }
                    }
                    Err(_) => {
                        // Non-JSON — forward as TokenDelta for visibility.
                        let _ = event_tx_out.send(ServerEvent::TokenDelta {
                            task_id: tid_out.clone(),
                            content: format!("{trimmed}\n"),
                        });
                    }
                }
            }
            (agent_iterations, final_cost_usd)
        });

        let event_tx_err = event_tx.clone();
        let tid_err = tid.clone();
        let stderr_task = tokio::spawn(async move {
            while let Ok(Some(line)) = err_reader.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let _ = event_tx_err.send(ServerEvent::TokenDelta {
                    task_id: tid_err.clone(),
                    content: format!("[runner] {line}\n"),
                });
            }
        });

        let exit = child.wait().await;
        let (iters, cost) = stdout_task.await.unwrap_or((0, 0.0));
        let _ = stderr_task.await;

        match exit {
            Ok(status) if status.success() => {
                let _ = event_tx.send(ServerEvent::TaskComplete {
                    task_id: tid.clone(),
                    iterations: iters,
                    cost_usd: cost,
                });
            }
            Ok(status) => {
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: tid.clone(),
                    message: format!("runner exited non-zero: {status}"),
                });
            }
            Err(e) => {
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: tid.clone(),
                    message: format!("runner wait failed: {e}"),
                });
            }
        }

        // Pearl bookkeeping on exit — mark the task pearl done.
        if let Some(ref id) = pearl_id {
            let _ = pearl_store.close(&[id.as_str()]);
        }
    });
}

/// Load LLM config for the in-VM runner. Big Smooth reads its own
/// providers.json (which it already does for the in-process path) and
/// projects the relevant fields into env vars the runner can consume.
/// Pull the bare hostname out of an HTTP(S) URL for the secrets
/// allowed-hosts list. Intentionally minimal — we don't need a
/// full URL crate for the shape providers.json produces
/// (`https://llm.smoo.ai/v1`, `http://127.0.0.1:11434/v1`, etc.).
/// Strips scheme, port, userinfo, and path. Returns an empty
/// string on any parse failure; callers treat empty as "no
/// substitution" (the placeholder never gets expanded on the wire,
/// which is safer than expanding on the wrong host).
fn extract_host_from_url(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    // Strip userinfo ("user:pass@").
    let after_userinfo = after_scheme.rsplit_once('@').map_or(after_scheme, |(_, host)| host);
    // Cut at first '/', '?', or '#' → authority.
    let authority = after_userinfo.split(['/', '?', '#']).next().unwrap_or("");
    // Strip port.
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    host.to_string()
}

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
        service: "big-smooth".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        uptime: state.start_time.elapsed().as_secs_f64(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    })
}

async fn system_health_handler(State(state): State<AppState>) -> Json<ApiResponse<SystemHealth>> {
    state.touch();
    // Round-trip a query against Dolt to confirm the store is responsive.
    let db_ok = state.pearl_store.get_config("__health_check").is_ok();
    let ts = crate::tailscale::get_status();

    let orch = state.orchestrator.lock().await;
    let orch_health = OrchestratorHealth {
        state: orch.state_name().to_string(),
        active_workers: orch.active_workers.len() as u32,
        completed: orch.completed_beads.len() as u32,
    };
    let sandbox_active = u32::try_from(orch.pool.active_count()).unwrap_or(u32::MAX);
    let sandbox_max = u32::try_from(orch.pool.max_concurrency()).unwrap_or(u32::MAX);
    drop(orch);

    Json(ApiResponse {
        data: SystemHealth {
            leader: LeaderHealth {
                status: "healthy".into(),
                uptime: state.start_time.elapsed().as_secs_f64(),
            },
            database: DatabaseHealth {
                status: if db_ok { "healthy" } else { "down" }.into(),
                path: state.pearl_store.dolt_path().display().to_string(),
            },
            sandbox: SandboxHealth {
                status: "healthy".into(),
                backend: "local-microsandbox".into(),
                active_sandboxes: sandbox_active,
                max_concurrency: sandbox_max,
            },
            tailscale: TailscaleHealth {
                status: if ts.connected { "connected" } else { "disconnected" }.into(),
                hostname: ts.hostname,
            },
            pearls: PearlsHealth {
                status: "healthy".into(),
                open_pearls: state.pearl_store.stats().map_or(0, |s| (s.open + s.in_progress) as u32),
            },
            orchestrator: orch_health,
        },
        ok: true,
    })
}

// ── Orchestrator ──────────────────────────────────────────

async fn orchestrator_status_handler(State(state): State<AppState>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let orch = state.orchestrator.lock().await;
    let status = serde_json::json!({
        "state": orch.state_name(),
        "active_workers": orch.active_workers.len(),
        "completed": orch.completed_beads.len(),
        "pool_max_concurrency": orch.pool.max_concurrency(),
        "pool_active": orch.pool.active_count(),
    });
    Json(ApiResponse { data: status, ok: true })
}

// ── Config ─────────────────────────────────────────────────

async fn get_config_handler(State(state): State<AppState>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let pairs = state.pearl_store.list_config().unwrap_or_default();
    let mut obj = serde_json::Map::new();
    for (k, v) in pairs {
        // Values were set as JSON-stringified; parse back if possible,
        // otherwise return the raw string.
        let parsed: serde_json::Value = serde_json::from_str(&v).unwrap_or(serde_json::Value::String(v));
        obj.insert(k, parsed);
    }
    Json(ApiResponse {
        data: serde_json::Value::Object(obj),
        ok: true,
    })
}

async fn set_config_handler(State(state): State<AppState>, Json(body): Json<ConfigBody>) -> Json<ApiResponse<()>> {
    state.touch();
    let value_str = serde_json::to_string(&body.value).unwrap_or_default();
    let ok = state.pearl_store.set_config(&body.key, &value_str).is_ok();
    Json(ApiResponse { data: (), ok })
}

// ── Tasks (headless agent execution via SSE) ──────────────

async fn run_task_handler(State(state): State<AppState>, Json(req): Json<TaskRequest>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    state.touch();

    // Subscribe to the broadcast channel BEFORE dispatching so we don't miss
    // events. The dispatched task broadcasts ServerEvents which we forward as
    // AgentEvent SSE chunks for clients.
    let mut event_rx = state.event_tx.subscribe();
    let (sse_tx, sse_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

    // Dispatch via the unified ws task path — sandboxed if SMOOTH_SANDBOXED is
    // set, in-process otherwise. Sandboxed is the security architecture path:
    // operator runs inside a microVM with Wonk/Goalie/Narc enforcement.
    let state_clone = state.clone();
    let opts = DispatchOptions {
        message: req.message.clone(),
        model: req.model.clone(),
        budget: req.budget,
        working_dir: req.working_dir.clone(),
        image: req.image.clone(),
        keep_alive: req.keep_alive,
        memory_mb: req.memory_mb,
        agent: req.agent.clone(),
        pearl_id: None,
    };

    tokio::spawn(async move {
        dispatch_ws_task(&state_clone, opts).await;
    });

    // Bridge ServerEvent broadcast → AgentEvent SSE stream
    tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    let agent_event = match event {
                        ServerEvent::TokenDelta { content, .. } => Some(AgentEvent::TokenDelta { content }),
                        ServerEvent::ToolCallStart { tool_name, .. } => Some(AgentEvent::ToolCallStart { iteration: 0, tool_name }),
                        ServerEvent::ToolCallComplete { tool_name, is_error, .. } => Some(AgentEvent::ToolCallComplete {
                            iteration: 0,
                            tool_name,
                            is_error,
                        }),
                        ServerEvent::TaskComplete { iterations, cost_usd, .. } => {
                            let _ = sse_tx.send(AgentEvent::Completed {
                                agent_id: "task".into(),
                                iterations,
                                cost_usd,
                            });
                            break;
                        }
                        ServerEvent::TaskError { message, .. } => {
                            let _ = sse_tx.send(AgentEvent::Error { message });
                            break;
                        }
                        _ => None,
                    };
                    if let Some(e) = agent_event {
                        if sse_tx.send(e).is_err() {
                            break;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(sse_rx);
    let sse_stream = futures_util::StreamExt::map(stream, |event| {
        let data = serde_json::to_string(&event).unwrap_or_else(|_| r#"{"type":"Error","message":"serialization failed"}"#.into());
        Ok(Event::default().data(data))
    });

    Sse::new(sse_stream)
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

/// Derive a stable cache key from a workspace path. Produces
/// `<basename>-<6hex>` where the hex is the first 6 nibbles of an FNV-1a
/// hash of the canonicalized path — stable across runs, distinguishes
/// siblings sharing a basename. Returns `None` for empty inputs.
///
/// Why FNV rather than SHA: we only need bucket-level collision
/// resistance across the user's own projects, and avoiding the
/// `sha2` dep keeps this hot path free of cost.
pub fn project_cache_key(workspace: &str) -> Option<String> {
    let ws = workspace.trim();
    if ws.is_empty() {
        return None;
    }

    // FNV-1a 64-bit.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in ws.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }

    let basename = std::path::Path::new(ws)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "workspace".to_string());

    // Keep keys filesystem-safe: alphanum + dashes. Collapse anything
    // else to dashes so weird paths ("my project (copy)/") don't
    // produce pathological directory names.
    let safe: String = basename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();

    Some(format!("{safe}-{:06x}", hash & 0x00ff_ffff))
}

// ── Projects (multi-project pearl support) ─────────────────

#[derive(Serialize, Default)]
struct ProjectPearlCounts {
    open: usize,
    in_progress: usize,
    closed: usize,
}

#[derive(Serialize)]
struct ProjectInfo {
    path: String,
    name: String,
    pearl_counts: ProjectPearlCounts,
}

/// Returns `true` if a registry entry should be filtered out (temp dirs, invalid roots,
/// or missing `.smooth/dolt/` directory).
fn is_invalid_project(path: &str) -> bool {
    let p = std::path::Path::new(path);
    path.starts_with("/var/folders")
        || path == "/"
        || path == "/root"
        || p.components().count() <= 3 // filter bare home dirs like /Users/username
        || !p.join(".smooth/dolt").exists()
}

async fn list_projects_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<ProjectInfo>>> {
    state.touch();

    // Use the per-project `PearlStore`s pre-spawned at startup, each
    // routed through its own long-running `smooth-dolt serve`. See
    // `AppState::project_pearl_stores` for why we don't open fresh.
    let stores = state.project_pearl_stores.clone();

    let projects = tokio::task::spawn_blocking(move || -> Vec<ProjectInfo> {
        let registry = match smooth_pearls::Registry::load() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load project registry");
                return Vec::new();
            }
        };

        let mut projects = Vec::new();
        for entry in registry.list() {
            let path_str = entry.path.to_string_lossy().to_string();
            if is_invalid_project(&path_str) {
                continue;
            }

            let counts = match stores.get(&entry.path) {
                Some(store) => store
                    .stats()
                    .map(|stats| ProjectPearlCounts {
                        open: stats.open,
                        in_progress: stats.in_progress,
                        closed: stats.closed,
                    })
                    .unwrap_or_default(),
                None => {
                    // Project registered after startup, or its serve
                    // child failed to spawn. Surface the entry with
                    // zero counts so it still appears in the picker.
                    tracing::debug!(path = %path_str, "project not in pre-spawned cache; restart service to populate");
                    ProjectPearlCounts::default()
                }
            };

            projects.push(ProjectInfo {
                path: path_str,
                name: entry.name.clone(),
                pearl_counts: counts,
            });
        }
        projects
    })
    .await
    .unwrap_or_default();

    Json(ApiResponse { data: projects, ok: true })
}

#[derive(Deserialize)]
pub struct ProjectPearlsParams {
    path: String,
    status: Option<String>,
    /// Optional cap on returned pearls. Defaults to `0` = "no limit" so
    /// the dashboard / pearls page get the full set for client-side
    /// counting and bucketing. Pass an explicit value to paginate.
    #[serde(default)]
    limit: usize,
}

async fn project_pearls_handler(State(state): State<AppState>, Query(params): Query<ProjectPearlsParams>) -> Json<ApiResponse<Vec<smooth_pearls::Pearl>>> {
    state.touch();

    // Use the pre-spawned (server-mode) store for this project. See
    // `AppState::project_pearl_stores` for why we don't open fresh.
    let project_path = std::path::PathBuf::from(&params.path);
    let stores = state.project_pearl_stores.clone();
    let limit = params.limit;
    let status = params.status.clone();
    let path_for_log = params.path.clone();
    let result: Result<Vec<smooth_pearls::Pearl>, anyhow::Error> = tokio::task::spawn_blocking(move || {
        let store = stores
            .get(&project_path)
            .ok_or_else(|| anyhow::anyhow!("project not in pre-spawned cache (restart service to populate): {}", project_path.display()))?;
        let mut query = smooth_pearls::PearlQuery::new().with_limit(limit);
        if let Some(ref s) = status {
            query = query.with_status(smooth_pearls::PearlStatus::from_str_loose(s).unwrap_or(smooth_pearls::PearlStatus::Open));
        }
        Ok(store.list(&query).unwrap_or_default())
    })
    .await
    .unwrap_or_else(|join_err| Err(anyhow::anyhow!("blocking task join failed: {join_err}")));

    match result {
        Ok(pearls) => Json(ApiResponse { data: pearls, ok: true }),
        Err(e) => {
            tracing::warn!(error = %e, path = %path_for_log, "failed to load pearls for project");
            Json(ApiResponse { data: vec![], ok: false })
        }
    }
}

// ── Issues ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ListPearlsParams {
    status: Option<String>,
    /// Optional cap. Defaults to `0` = "no limit" so the web UI gets
    /// the full set; pass a value to paginate.
    #[serde(default)]
    limit: usize,
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
    let issues = crate::pearls::list_pearls_with_limit(&state.pearl_store, params.status.as_deref(), params.limit).unwrap_or_default();
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
    let orch = state.orchestrator.lock().await;
    let data: Vec<serde_json::Value> = orch
        .active_workers
        .iter()
        .map(|(id, handle)| {
            serde_json::json!({
                "operator_id": id,
                "msb_name": handle.msb_name,
                "bead_id": handle.bead_id,
                "host_port": handle.host_port,
                "port_mappings": handle.port_mappings,
                "created_at": handle.created_at,
            })
        })
        .collect();
    Json(ApiResponse { data, ok: true })
}

async fn get_worker_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let orch = state.orchestrator.lock().await;
    let data = orch
        .active_workers
        .get(&id)
        .map(|h| {
            serde_json::json!({
                "operator_id": id,
                "msb_name": h.msb_name,
                "bead_id": h.bead_id,
                "host_port": h.host_port,
                "port_mappings": h.port_mappings,
                "created_at": h.created_at,
                "status": "running",
            })
        })
        .unwrap_or_else(|| serde_json::json!({"id": id, "status": "unknown"}));
    Json(ApiResponse { data, ok: true })
}

async fn kill_worker_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<()>> {
    state.touch();
    // Look up the sandbox handle so we can destroy the VM by its
    // msb_name (the sandbox registry key), then drop it from the
    // orchestrator's active_workers map.
    let msb_name = {
        let orch = state.orchestrator.lock().await;
        orch.active_workers.get(&id).map(|h| h.msb_name.clone())
    };
    match msb_name {
        Some(name) => {
            if let Err(e) = crate::sandbox::destroy_sandbox(&name).await {
                tracing::warn!(operator = %id, error = %e, "kill_worker: destroy_sandbox failed");
            }
            let mut orch = state.orchestrator.lock().await;
            orch.active_workers.remove(&id);
            tracing::info!(operator = %id, "kill_worker: VM destroyed and removed from active set");
            Json(ApiResponse { data: (), ok: true })
        }
        None => {
            tracing::warn!(operator = %id, "kill_worker: no active operator with that id");
            Json(ApiResponse { data: (), ok: false })
        }
    }
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

    // The chat agent IS the team lead. It searches pearls, creates them
    // with smooth-summarize-generated titles, dispatches teammates
    // (operators), nudges them with steering messages, and reads back
    // their progress. Default model is `smooth-reasoning-kimi` —
    // capability for orchestration beats raw speed; per-request model
    // override is a Phase 6 polish (see plan).
    let system_prompt = include_str!("chat_tools_system_prompt.txt");

    async fn chat_inner(
        state: AppState,
        system_prompt: &str,
        user_content: &str,
        model_override: Option<&str>,
        budget_usd: Option<f64>,
    ) -> anyhow::Result<String> {
        use smooth_operator::agent::{Agent, AgentConfig, AgentEvent};
        use smooth_operator::cost::CostBudget;

        let providers_path = dirs_next::home_dir().unwrap_or_default().join(".smooth/providers.json");
        let registry = ProviderRegistry::load_from_file(&providers_path).map_err(|e| anyhow::anyhow!("no LLM providers configured: {e}"))?;

        // Resolve the chat agent's LLM. Default is the reasoning slot
        // (smooth-reasoning-kimi via providers.json routing). A
        // per-request `model` field on the chat body lets callers flip
        // to smooth-fast-gemini for one-liner queries.
        let llm_config = if let Some(m) = model_override.filter(|s| !s.trim().is_empty()) {
            // Use the default provider's key/url with the requested model alias.
            let mut cfg = registry.default_llm_config().map_err(|e| anyhow::anyhow!("no default provider: {e}"))?;
            cfg.model = m.to_string();
            cfg
        } else {
            // Reasoning slot — see plan + providers.json `routing.thinking`.
            registry
                .llm_config_for(smooth_operator::providers::Activity::Reasoning)
                .map_err(|e| anyhow::anyhow!("resolving reasoning slot for chat: {e}"))?
        };

        let registry_arc = std::sync::Arc::new(registry);
        let tools = crate::chat_tools::build_chat_tools(state.clone(), registry_arc.clone());

        // 20-iteration cap: enough for the chat-agent to drive a task
        // to completion via teammate_wait (poll + retry + format). Each
        // teammate_wait burns just one iteration even if the underlying
        // wait is a minute. Five was too few — the agent ran out of
        // turns mid-poll and returned an empty assistant message.
        let mut agent_cfg = AgentConfig::new("big-smooth-chat", system_prompt, llm_config).with_max_iterations(20);
        if let Some(cap) = budget_usd {
            agent_cfg = agent_cfg.with_budget(CostBudget {
                max_cost_usd: Some(cap),
                max_tokens: None,
            });
        }
        let agent = Agent::new(agent_cfg, tools);

        let thoughts = crate::thoughts::ThoughtStreamer::new(&registry_arc, state.event_tx.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let drain = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    AgentEvent::ToolCallStart { tool_name, .. } => {
                        thoughts.emit(crate::thoughts::ThoughtContext::ToolCall { tool_name });
                    }
                    AgentEvent::LlmResponse { content_preview, .. } if !content_preview.trim().is_empty() => {
                        thoughts.emit(crate::thoughts::ThoughtContext::AssistantPreview { snippet: content_preview });
                    }
                    _ => {}
                }
            }
        });

        let conversation = agent
            .run_with_channel(user_content.to_string(), tx)
            .await
            .map_err(|e| anyhow::anyhow!("chat agent: {e}"))?;
        drain.abort();

        // Final assistant message is the user-facing reply.
        let last_assistant = conversation.last_assistant_content().unwrap_or("(no response)").to_string();
        Ok(last_assistant)
    }

    let result: anyhow::Result<String> = chat_inner(state, system_prompt, &body.content, body.model.as_deref(), body.budget_usd).await;

    match result {
        Ok(response) => Json(ApiResponse { data: response, ok: true }),
        Err(e) => Json(ApiResponse {
            data: format!("Error: {e}"),
            ok: true,
        }),
    }
}

// ── Chat sessions ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateChatSessionBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
pub struct PostChatMessageBody {
    content: String,
}

#[derive(Serialize)]
pub struct ChatMessageView {
    id: String,
    role: String, // "user" | "assistant"
    content: String,
    created_at: String,
}

async fn create_chat_session_handler(State(state): State<AppState>, Json(body): Json<CreateChatSessionBody>) -> Json<ApiResponse<crate::session::ChatSession>> {
    state.touch();
    let title = body.title.unwrap_or_else(|| "New chat".to_string());
    let model = body.model.unwrap_or_else(chat_default_model);
    match state.session_store.create_chat_session(&title, &model) {
        Ok(session) => Json(ApiResponse { data: session, ok: true }),
        Err(e) => {
            tracing::warn!(error = %e, "failed to create chat session");
            Json(ApiResponse {
                data: crate::session::ChatSession {
                    id: String::new(),
                    title: String::new(),
                    model: String::new(),
                    started_at: chrono::Utc::now(),
                    message_count: 0,
                    token_count: 0,
                },
                ok: false,
            })
        }
    }
}

async fn list_chat_sessions_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<crate::session::ChatSession>>> {
    state.touch();
    let sessions = state.session_store.list_chat_sessions().unwrap_or_default();
    Json(ApiResponse { data: sessions, ok: true })
}

async fn get_chat_session_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<Option<crate::session::ChatSession>>> {
    state.touch();
    let session = state.session_store.get_chat_session(&id).ok().flatten();
    Json(ApiResponse { data: session, ok: true })
}

async fn delete_chat_session_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<()>> {
    state.touch();
    let ok = state.session_store.delete_chat_session(&id).is_ok();
    Json(ApiResponse { data: (), ok })
}

async fn get_chat_messages_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<Vec<ChatMessageView>>> {
    use crate::session::SessionStore;
    state.touch();
    let msgs = state.session_store.get_messages(&id, 1000).unwrap_or_default();
    let views: Vec<ChatMessageView> = msgs
        .into_iter()
        .map(|m| ChatMessageView {
            id: m.id,
            role: if m.from == "user" { "user".to_string() } else { "assistant".to_string() },
            content: m.content,
            created_at: m.timestamp.to_rfc3339(),
        })
        .collect();
    Json(ApiResponse { data: views, ok: true })
}

async fn post_chat_message_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PostChatMessageBody>,
) -> Json<ApiResponse<ChatMessageView>> {
    use crate::session::SessionStore;
    state.touch();

    let user_content = body.content;
    let user_msg_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let user_msg = crate::session::SessionMessage {
        id: user_msg_id.clone(),
        session_id: id.clone(),
        from: "user".into(),
        to: "bigsmooth".into(),
        content: user_content.clone(),
        timestamp: chrono::Utc::now(),
        message_type: crate::session::MessageType::Command,
    };
    if let Err(e) = state.session_store.save_message(user_msg) {
        tracing::warn!(error = %e, "failed to save user chat message");
    }

    // If this is the first message, kick off an async auto-name via
    // `smooth-fast` — the Haiku-class utility slot. We spawn the LLM
    // call into a detached tokio task so the chat response isn't gated
    // on it; the title lands on the session row via `rename_chat_session`
    // whenever the completion comes back. If the fast slot isn't
    // configured or the call fails, we fall back to the legacy
    // truncate-first-60-chars behaviour so a session is never left
    // literally named "New chat".
    if let Ok(Some(session)) = state.session_store.get_chat_session(&id) {
        if session.title == "New chat" {
            let session_store = state.session_store.clone();
            let id_for_spawn = id.clone();
            let prompt_for_spawn = user_content.clone();
            tokio::spawn(async move {
                let title = auto_name_session(&prompt_for_spawn).await.unwrap_or_else(|| {
                    // Fallback to the legacy behaviour.
                    let short: String = prompt_for_spawn.chars().take(60).collect();
                    short.trim().to_string()
                });
                if !title.is_empty() {
                    let _ = session_store.rename_chat_session(&id_for_spawn, &title);
                }
            });
        }
    }

    // Pull recent history to feed the LLM (oldest first).
    let history = state.session_store.get_messages(&id, 50).unwrap_or_default();

    // Goal-first system prompt shared with /api/chat — the session-bound
    // path is the one the web UI hits, so this is what the user actually
    // sees as Big Smooth's persona.
    let system_prompt = include_str!("chat_tools_system_prompt.txt");
    let assistant_text = match run_chat_with_history(&state, system_prompt, &history, &user_content).await {
        Ok(s) => s,
        Err(e) => format!("Error: {e}"),
    };

    let assistant_msg_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let assistant_msg = crate::session::SessionMessage {
        id: assistant_msg_id.clone(),
        session_id: id.clone(),
        from: "bigsmooth".into(),
        to: "user".into(),
        content: assistant_text.clone(),
        timestamp: chrono::Utc::now(),
        message_type: crate::session::MessageType::Response,
    };
    if let Err(e) = state.session_store.save_message(assistant_msg) {
        tracing::warn!(error = %e, "failed to save assistant chat message");
    }

    let _ = state.session_store.bump_message_count(&id, 2);

    Json(ApiResponse {
        data: ChatMessageView {
            id: assistant_msg_id,
            role: "assistant".into(),
            content: assistant_text,
            created_at: chrono::Utc::now().to_rfc3339(),
        },
        ok: true,
    })
}

fn chat_system_prompt() -> &'static str {
    "You are Big Smooth, an AI agent orchestration leader. You help users manage projects, assign work to Smooth Operators (AI agents in sandboxes), review work, and coordinate tasks.\n\nAvailable commands: th run <pearl-id>, th operators, th pause/steer/cancel <pearl-id>, th auth status, th status"
}

/// Generate a short (3–6 word) title summarizing the user's first
/// message, using the `smooth-fast` routing slot (Haiku-class). Runs
/// in a detached task so chat latency isn't gated on it. Returns
/// `None` if the slot isn't configured or the call fails — caller
/// falls back to a literal truncation of the prompt.
///
/// Trims quotes, trailing punctuation, and clamps to 60 chars so a
/// chatty model can't silently fill the UI with a paragraph.
async fn auto_name_session(user_prompt: &str) -> Option<String> {
    let providers_path = dirs_next::home_dir()?.join(".smooth/providers.json");
    let registry = ProviderRegistry::load_from_file(&providers_path).ok()?;
    let cast = smooth_operator::cast::Cast::builtin();
    let agent = cast.get("tagger")?;
    let config = registry.llm_config_for(agent.slot).ok()?;
    let llm = smooth_operator::llm::LlmClient::new(config);

    let system = smooth_operator::conversation::Message::system(&agent.prompt);
    let user = smooth_operator::conversation::Message::user(user_prompt);
    let resp = llm.chat(&[&system, &user], &[]).await.ok()?;

    let cleaned = resp
        .content
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '.' || c == '\n')
        .chars()
        .take(60)
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn chat_default_model() -> String {
    let providers_path = dirs_next::home_dir().unwrap_or_default().join(".smooth/providers.json");
    ProviderRegistry::load_from_file(&providers_path)
        .ok()
        .and_then(|r| r.default_llm_config().ok())
        .map(|c| c.model)
        .unwrap_or_else(|| "default".to_string())
}

/// Run an agentic chat turn for the session-bound endpoint
/// (`POST /api/chat/sessions/{id}/messages`).
///
/// Same Agent + tools as the bare `/api/chat` handler — the session
/// endpoint just adds prior-conversation context. The web UI uses this
/// endpoint, so making this agentic is what makes the chat actually
/// orchestrate (search/create pearls, spawn teammates, message them)
/// instead of returning Haiku-class one-shots.
async fn run_chat_with_history(
    state: &AppState,
    system_prompt: &str,
    history: &[crate::session::SessionMessage],
    user_content: &str,
) -> anyhow::Result<String> {
    use smooth_operator::agent::{Agent, AgentConfig, AgentEvent};

    let providers_path = dirs_next::home_dir().unwrap_or_default().join(".smooth/providers.json");
    let registry = ProviderRegistry::load_from_file(&providers_path).map_err(|e| anyhow::anyhow!("no LLM providers configured: {e}"))?;

    // Reasoning slot — same default as `/api/chat`. Capability over speed
    // for orchestration; users can flip the slot via providers.json.
    let llm_config = registry
        .llm_config_for(smooth_operator::providers::Activity::Reasoning)
        .map_err(|e| anyhow::anyhow!("resolving reasoning slot for chat: {e}"))?;

    let registry_arc = std::sync::Arc::new(registry);
    let tools = crate::chat_tools::build_chat_tools(state.clone(), registry_arc.clone());

    // Fold prior history into a single read-only context block prepended to
    // the new user-turn. Avoids the user/assistant alternation requirement
    // some OpenAI-compat providers enforce on the live message array, and
    // keeps the agent's compaction strategy from churning on old turns.
    let mut history_block = String::new();
    for m in history {
        let speaker = if m.from == "user" { "User" } else { "Big Smooth" };
        history_block.push_str(&format!("{speaker}: {}\n\n", m.content));
    }
    let user_payload = if history_block.is_empty() {
        user_content.to_string()
    } else {
        format!("Recent conversation history (read-only context):\n\n{history_block}---\n\nNew user message:\n\n{user_content}")
    };

    // 20 iterations so the agent can spawn → wait → format without
    // running out of turns. teammate_wait absorbs the long wait into
    // one iteration so this stays responsive.
    let agent_cfg = AgentConfig::new("big-smooth-chat-session", system_prompt, llm_config).with_max_iterations(20);
    let agent = Agent::new(agent_cfg, tools);

    // Thought stream — Gemini Flash Lite summarizes each tool call /
    // assistant snippet into a one-line first-person thought and
    // broadcasts it to the chat WS so the UI can float it next to the
    // BS face. Non-blocking (Semaphore-capped) so the agent never
    // waits on the summarizer.
    let thoughts = crate::thoughts::ThoughtStreamer::new(&registry_arc, state.event_tx.clone());

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let drain = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::ToolCallStart { tool_name, .. } => {
                    thoughts.emit(crate::thoughts::ThoughtContext::ToolCall { tool_name });
                }
                AgentEvent::LlmResponse { content_preview, .. } if !content_preview.trim().is_empty() => {
                    thoughts.emit(crate::thoughts::ThoughtContext::AssistantPreview { snippet: content_preview });
                }
                _ => {}
            }
        }
    });

    let conversation = agent.run_with_channel(user_payload, tx).await.map_err(|e| anyhow::anyhow!("chat agent: {e}"))?;
    drain.abort();

    Ok(conversation.last_assistant_content().unwrap_or("(no response)").to_string())
}

// ── Boardroom Narc — POST /api/narc/judge ─────────────────

/// Arbitrate a runtime access request escalated from a per-VM Wonk.
///
/// Wonk calls this when its local policy can't auto-approve a `/check/*`
/// request. Narc applies its rule engine, cache, and (when nothing else
/// resolves the request) LLM judge, then returns an approve / deny /
/// escalate_to_human verdict. Returns the decision directly as JSON — no
/// `ApiResponse` envelope, because Wonk speaks the raw `JudgeDecision`
/// wire format shared with `smooth-narc::judge`.
async fn narc_judge_handler(State(state): State<AppState>, Json(request): Json<smooth_narc::judge::JudgeRequest>) -> Json<smooth_narc::judge::JudgeDecision> {
    state.touch();
    let decision = state.boardroom_narc.judge(request).await;
    Json(decision)
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

// ── Teammates (Phase 4) ────────────────────────────────────

#[derive(Deserialize)]
pub struct PostTeammateMessageBody {
    content: String,
}

async fn list_teammates_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<crate::teammates::TeammateView>>> {
    state.touch();
    let mut list = state.teammates.list().await;
    list.sort_by_key(|t| std::cmp::Reverse(t.last_event_at));
    Json(ApiResponse { data: list, ok: true })
}

async fn get_teammate_messages_handler(State(state): State<AppState>, Path(name): Path<String>) -> Json<ApiResponse<Vec<crate::session::SessionMessage>>> {
    state.touch();
    let Some(view) = state.teammates.get(&name).await else {
        return Json(ApiResponse { data: Vec::new(), ok: false });
    };
    // Return the pearl's recent comments cast as session-message-shaped
    // records so the chat panel can render them uniformly.
    let comments = state.pearl_store.get_comments(&view.pearl_id).unwrap_or_default();
    let msgs: Vec<crate::session::SessionMessage> = comments
        .into_iter()
        .map(|c| crate::session::SessionMessage {
            id: c.id,
            session_id: view.pearl_id.clone(),
            from: actor_for_comment(&c.content).to_string(),
            to: "user".to_string(),
            content: c.content,
            message_type: crate::session::MessageType::Response,
            timestamp: c.created_at,
        })
        .collect();
    Json(ApiResponse { data: msgs, ok: true })
}

async fn post_teammate_message_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PostTeammateMessageBody>,
) -> Json<ApiResponse<String>> {
    state.touch();
    let Some(view) = state.teammates.get(&name).await else {
        return Json(ApiResponse {
            data: format!("teammate {name} not found"),
            ok: false,
        });
    };
    let comment = format!("[CHAT:USER] {}", body.content);
    if let Err(e) = state.pearl_store.add_comment(&view.pearl_id, &comment) {
        return Json(ApiResponse {
            data: format!("posting message failed: {e}"),
            ok: false,
        });
    }
    Json(ApiResponse {
        data: "Message queued for the teammate.".into(),
        ok: true,
    })
}

async fn shutdown_teammate_handler(State(state): State<AppState>, Path(name): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    let Some(view) = state.teammates.get(&name).await else {
        return Json(ApiResponse {
            data: format!("teammate {name} not found"),
            ok: false,
        });
    };
    let _ = state
        .pearl_store
        .add_comment(&view.pearl_id, "[STEERING:SHUTDOWN] graceful shutdown requested by user");
    state.teammates.mark_status(&name, "ended").await;
    Json(ApiResponse {
        data: "shutdown requested".into(),
        ok: true,
    })
}

fn actor_for_comment(body: &str) -> &'static str {
    let t = body.trim_start();
    if t.starts_with("[CHAT:USER]") {
        "user"
    } else if t.starts_with("[CHAT:TEAMMATE]") || t.starts_with("[PROGRESS]") || t.starts_with("[QUESTION:TEAMMATE") || t.starts_with("[IDLE]") {
        "teammate"
    } else if t.starts_with("[STEERING:") || t.starts_with("[ANSWER:") {
        "lead"
    } else {
        "system"
    }
}

// ── Jira ───────────────────────────────────────────────────

async fn jira_status_handler(State(state): State<AppState>) -> Json<ApiResponse<crate::jira::SyncStatus>> {
    state.touch();
    let config = crate::jira::JiraConfig::from_pearl_store(&state.pearl_store);
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

// ── Delegation ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DelegateRequest {
    /// The operator requesting delegation.
    pub parent_operator_id: String,
    /// The task to delegate.
    pub task: String,
    /// Optional model override; if absent the orchestrator picks one.
    pub model: Option<String>,
}

#[derive(Serialize)]
pub struct DelegateResponse {
    pub delegation_id: String,
    pub status: String,
}

#[derive(Serialize)]
pub struct DelegateStatusResponse {
    pub delegation_id: String,
    pub status: String,
    /// Last comment on the pearl, if completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

async fn delegate_handler(State(state): State<AppState>, Json(body): Json<DelegateRequest>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();

    // 1. Create a sub-pearl (subtask type) linked to the parent operator.
    let title = format!("[delegated] {}", truncate_str(&body.task, 80));
    let pearl = match crate::pearls::create_pearl(&state.pearl_store, &title, &body.task, "subtask", 1) {
        Ok(p) => p,
        Err(e) => {
            return Json(ApiResponse {
                data: serde_json::json!({"error": e.to_string()}),
                ok: false,
            });
        }
    };
    let pearl_id = pearl.id.clone();

    // 2. Leave as Open so the orchestrator's `ready()` picks it up on the
    //    next scheduling cycle. The orchestrator will transition it to
    //    InProgress when it dispatches an operator.

    // 3. Add a comment noting delegation origin.
    let comment = format!(
        "[DELEGATION] Delegated by operator {} | model: {}",
        body.parent_operator_id,
        body.model.as_deref().unwrap_or("inherit")
    );
    let _ = state.pearl_store.add_comment(&pearl_id, &comment);

    // 4. Notify the orchestrator so it can schedule dispatch.
    {
        let mut orch = state.orchestrator.lock().await;
        orch.nudge();
    }

    let resp = DelegateResponse {
        delegation_id: pearl_id,
        status: "dispatched".into(),
    };
    Json(ApiResponse {
        data: serde_json::to_value(resp).unwrap_or(serde_json::json!(null)),
        ok: true,
    })
}

async fn delegate_status_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();

    // Look up the pearl.
    let pearl = match crate::pearls::get_pearl(&state.pearl_store, &id) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Json(ApiResponse {
                data: serde_json::json!({"error": "delegation not found"}),
                ok: false,
            });
        }
        Err(e) => {
            return Json(ApiResponse {
                data: serde_json::json!({"error": e.to_string()}),
                ok: false,
            });
        }
    };

    let (status_str, result) = match pearl.status {
        smooth_pearls::PearlStatus::Closed => {
            // Grab the last comment as the result.
            let last_comment = state
                .pearl_store
                .get_comments(&id)
                .ok()
                .and_then(|comments| comments.last().map(|c| c.content.clone()));
            ("completed".to_string(), last_comment)
        }
        smooth_pearls::PearlStatus::InProgress => ("in_progress".to_string(), None),
        smooth_pearls::PearlStatus::Open => ("in_progress".to_string(), None),
        smooth_pearls::PearlStatus::Deferred => ("failed".to_string(), None),
    };

    let resp = DelegateStatusResponse {
        delegation_id: id,
        status: status_str,
        result,
    };
    Json(ApiResponse {
        data: serde_json::to_value(resp).unwrap_or(serde_json::json!(null)),
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
    use tower::ServiceExt;

    #[test]
    fn extract_host_handles_common_shapes() {
        assert_eq!(extract_host_from_url("https://llm.smoo.ai/v1"), "llm.smoo.ai");
        assert_eq!(extract_host_from_url("http://127.0.0.1:11434/v1"), "127.0.0.1");
        assert_eq!(extract_host_from_url("https://api.openai.com"), "api.openai.com");
        assert_eq!(extract_host_from_url("https://user:pass@example.com/path"), "example.com");
        assert_eq!(extract_host_from_url("https://example.com:443/v1?q=1"), "example.com");
        assert_eq!(extract_host_from_url("example.com"), "example.com", "no scheme still works");
        // Malformed → empty; callers MUST treat empty as "don't substitute"
        // because substituting on the wrong host would leak the secret.
        assert_eq!(extract_host_from_url(""), "");
    }

    #[test]
    fn find_native_operator_runner_finds_debug_or_release_build() {
        // The direct-dispatch path needs a runner binary built for
        // the host triple. We don't assert which profile — CI or
        // dev boxes could have either — but at least one must
        // exist after a normal `cargo build -p smooai-smooth-operator-runner`.
        //
        // This test runs inside cargo, which means the workspace
        // has been built; skip gracefully if the binary happens to
        // be missing (e.g. running on an alternate target dir).
        if let Some(p) = find_native_operator_runner_binary() {
            assert!(p.is_file(), "returned path must point at a real file: {}", p.display());
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            assert_eq!(name, "smooth-operator-runner", "must be the runner binary, got: {name}");
            // Must be under target/<profile>/, not the
            // aarch64-unknown-linux-musl cross-compile output
            // (that one is for sandboxed dispatch).
            let path_str = p.to_string_lossy();
            assert!(
                path_str.contains("/target/release/") || path_str.contains("/target/debug/"),
                "native path should be target/release or target/debug, got: {path_str}"
            );
            assert!(
                !path_str.contains("aarch64-unknown-linux-musl"),
                "native path must NOT be the cross-compiled runner, got: {path_str}"
            );
        }
    }

    #[test]
    fn project_cache_key_is_stable_and_distinguishes_paths() {
        let a = project_cache_key("/Users/me/dev/budgeting").unwrap();
        let b = project_cache_key("/Users/me/dev/budgeting").unwrap();
        assert_eq!(a, b, "same input → same key");
        assert!(a.starts_with("budgeting-"), "key leads with basename: {a}");

        // Sibling paths with the same basename get different suffixes.
        let a = project_cache_key("/home/alice/apps/web").unwrap();
        let b = project_cache_key("/home/bob/apps/web").unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with("web-"));
        assert!(b.starts_with("web-"));

        // Weird chars collapsed so the key is filesystem-safe.
        let k = project_cache_key("/tmp/my project (copy)").unwrap();
        assert!(
            k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'),
            "unsafe char in {k}"
        );

        // Empty / whitespace → None.
        assert!(project_cache_key("").is_none());
        assert!(project_cache_key("   ").is_none());
    }

    #[test]
    fn max_sandbox_concurrency_env_override() {
        // Each sub-case uses a unique env var name via std::env isolation.
        // Set a valid numeric value.
        std::env::set_var("SMOOTH_SANDBOX_MAX_CONCURRENCY", "7");
        assert_eq!(max_sandbox_concurrency(), 7);

        // Zero is treated as unset → default.
        std::env::set_var("SMOOTH_SANDBOX_MAX_CONCURRENCY", "0");
        assert_eq!(max_sandbox_concurrency(), DEFAULT_SANDBOX_MAX_CONCURRENCY);

        // Garbage falls back to default.
        std::env::set_var("SMOOTH_SANDBOX_MAX_CONCURRENCY", "not-a-number");
        assert_eq!(max_sandbox_concurrency(), DEFAULT_SANDBOX_MAX_CONCURRENCY);

        // Unset falls back to default.
        std::env::remove_var("SMOOTH_SANDBOX_MAX_CONCURRENCY");
        assert_eq!(max_sandbox_concurrency(), DEFAULT_SANDBOX_MAX_CONCURRENCY);
    }

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
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return;
        };
        let state = AppState::new(pearl_store);
        let _router = build_router(state);
        // If we get here without panic, the router is valid
    }

    #[test]
    fn test_app_state_touch_updates_activity() {
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return;
        };
        let state = AppState::new(pearl_store);

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

    // pearl th-461ab9 (diag): tests for the runner-logs diagnostic helpers.
    #[test]
    fn diag_flag_is_truthy_accepts_documented_truthy_values() {
        for v in ["1", "true", "TRUE", "yes", "on", " 1 ", "  on  "] {
            assert!(diag_flag_is_truthy(v), "expected truthy for {v:?}");
        }
    }

    #[test]
    fn diag_flag_is_truthy_rejects_falsy_and_unknown_values() {
        for v in ["", "0", "false", "FALSE", "no", "off", "True", "False", "anything-else", "  "] {
            assert!(!diag_flag_is_truthy(v), "expected falsy for {v:?}");
        }
    }

    #[test]
    fn build_runner_diag_wrapper_argv_returns_three_elem_sh_invocation() {
        let argv = build_runner_diag_wrapper_argv("/opt/smooth/bin/smooth-operator-runner");
        assert_eq!(argv.len(), 3, "expected /bin/sh -c <script>, got: {argv:?}");
        assert_eq!(argv[0], "/bin/sh");
        assert_eq!(argv[1], "-c");
        let script = &argv[2];
        // The script must include the runner path verbatim (not template
        // syntax) and reference the host-readable log dir.
        assert!(
            script.contains("/opt/smooth/bin/smooth-operator-runner"),
            "runner path not interpolated: {script}"
        );
        assert!(script.contains("/var/log/smooth-runner"), "log dir missing: {script}");
        // The 8 expected diagnostic files must each be addressed.
        for name in [
            "00-started.txt",
            "01-mounts.txt",
            "02-listing.txt",
            "03-env.txt",
            "04-runner-check.txt",
            "05-stdout.log",
            "06-stderr.log",
            "07-exit-code.txt",
        ] {
            assert!(script.contains(name), "expected {name} in script: {script}");
        }
        // FIFO + background tee pattern is what preserves streaming back to
        // sandbox.exec — if it ever gets refactored away, this test fires.
        // The streaming-preserving pipeline: tee duplicates runner output to
        // both the parent's stdout/stderr (so sandbox.exec keeps seeing it
        // live) and to disk. fd 3 is used to swap streams so a single
        // pipeline can split into two tee branches without FIFOs.
        assert!(script.contains("tee -a "), "tee invocation missing: {script}");
        assert!(script.contains("3>&1"), "fd-3 stream-swap missing (needed for stderr branch)");
        // exit $rc preserves the runner's actual exit code (round-tripped
        // through 07-exit-code.txt because POSIX pipelines drop leftmost
        // stage's exit status).
        assert!(script.contains("exit $rc"), "exit code not preserved: {script}");
        assert!(script.contains("07-exit-code.txt"), "rcfile-via-disk pattern missing: {script}");
    }

    #[test]
    fn build_runner_diag_wrapper_argv_handles_runner_with_special_chars() {
        // Path may contain hyphens/underscores; just make sure the
        // template doesn't do anything unsafe with them.
        let argv = build_runner_diag_wrapper_argv("/opt/smooth/bin/smooth-operator-runner_v2");
        assert!(argv[2].contains("smooth-operator-runner_v2"));
        // Note: we deliberately do NOT shell-quote runner_in_vm because
        // it's controlled by Big Smooth (always a fixed
        // /opt/smooth/bin/<binary> path resolved from a host-side build
        // artifact). This test pins that assumption — if the call site
        // ever passes user-controlled input here, this test should be
        // updated alongside a shell-escape change.
    }

    #[test]
    fn build_runner_diag_wrapper_executes_locally_and_captures_streams() {
        // Run the actual generated script against a tiny shell-script
        // stand-in for the runner. Proves the FIFO + background tee
        // pattern doesn't deadlock and does land all 8 files.
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("var-log-smooth-runner");
        std::fs::create_dir_all(&log_dir).unwrap();
        let fake_runner = tmp.path().join("fake-runner.sh");
        std::fs::write(
            &fake_runner,
            "#!/bin/sh\n\
             echo '{\"type\":\"TokenDelta\",\"content\":\"hello\"}'\n\
             echo final-stderr-line >&2\n\
             exit 7\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_runner, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Patch the script to use our tmp log dir instead of /var/log/smooth-runner.
        let argv = build_runner_diag_wrapper_argv(fake_runner.to_str().unwrap());
        let patched = argv[2].replace("/var/log/smooth-runner", log_dir.to_str().unwrap());
        let output = std::process::Command::new("/bin/sh").arg("-c").arg(&patched).output().expect("spawn sh");
        assert_eq!(output.status.code(), Some(7), "runner exit code must be preserved through wrapper");
        // Streaming preservation: the runner's stdout line must appear on
        // the wrapper's stdout (this is what sandbox.exec captures).
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("\"TokenDelta\""),
            "wrapper stdout must echo runner stdout (sandbox.exec captures this); got: {stdout}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("final-stderr-line"), "wrapper stderr must echo runner stderr; got: {stderr}");
        // All 8 diagnostic files must exist with non-zero size for the ones we expect populated.
        for name in [
            "00-started.txt",
            "01-mounts.txt",
            "03-env.txt",
            "04-runner-check.txt",
            "05-stdout.log",
            "06-stderr.log",
            "07-exit-code.txt",
        ] {
            let p = log_dir.join(name);
            assert!(p.exists(), "{name} should exist after wrapper run");
            let size = std::fs::metadata(&p).unwrap().len();
            assert!(size > 0, "{name} is empty");
        }
        // Exit code file must contain "7\n" (the runner's exit code).
        let exit_str = std::fs::read_to_string(log_dir.join("07-exit-code.txt")).unwrap();
        assert_eq!(exit_str.trim(), "7", "07-exit-code.txt should hold runner's exit code");
        // 04-runner-check.txt must report runner-exists-and-executable.
        let check = std::fs::read_to_string(log_dir.join("04-runner-check.txt")).unwrap();
        assert!(check.contains("runner-exists-and-executable"), "got: {check}");
    }

    #[test]
    fn build_runner_diag_wrapper_handles_missing_runner() {
        // Same as above but with a runner path that doesn't exist —
        // wrapper should still produce all the diagnostic files and
        // surface a non-zero exit code, NOT hang.
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("var-log-smooth-runner");
        std::fs::create_dir_all(&log_dir).unwrap();
        let argv = build_runner_diag_wrapper_argv("/this/path/does/not/exist/fake-runner");
        let patched = argv[2].replace("/var/log/smooth-runner", log_dir.to_str().unwrap());
        let output = std::process::Command::new("/bin/sh").arg("-c").arg(&patched).output().expect("spawn sh");
        // Non-zero exit (likely 127) indicating runner-not-found.
        assert_ne!(output.status.code(), Some(0), "missing runner must not produce success exit");
        // 04-runner-check.txt must report MISSING-RUNNER.
        let check = std::fs::read_to_string(log_dir.join("04-runner-check.txt")).unwrap();
        assert!(check.contains("MISSING-RUNNER"), "got: {check}");
        // Stderr file should capture the shell's "not found" diagnostic.
        let stderr_log = std::fs::read_to_string(log_dir.join("06-stderr.log")).unwrap_or_default();
        assert!(
            stderr_log.contains("not found") || stderr_log.contains("No such file"),
            "stderr should capture missing-runner error; got: {stderr_log}"
        );
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

    // ── Delegation tests ──────────────────────────────────────

    #[test]
    fn test_delegate_request_deserializes() {
        let json = r#"{"parent_operator_id":"op-123","task":"Write unit tests","model":"kimi-k2.5"}"#;
        let req: DelegateRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.parent_operator_id, "op-123");
        assert_eq!(req.task, "Write unit tests");
        assert_eq!(req.model.as_deref(), Some("kimi-k2.5"));
    }

    #[test]
    fn test_delegate_request_minimal() {
        let json = r#"{"parent_operator_id":"op-1","task":"Do something"}"#;
        let req: DelegateRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.parent_operator_id, "op-1");
        assert_eq!(req.task, "Do something");
        assert!(req.model.is_none());
    }

    #[test]
    fn test_delegate_response_serializes() {
        let resp = DelegateResponse {
            delegation_id: "th-abc123".into(),
            status: "dispatched".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"delegation_id\":\"th-abc123\""));
        assert!(json.contains("\"status\":\"dispatched\""));
    }

    #[test]
    fn test_delegate_status_response_completed() {
        let resp = DelegateStatusResponse {
            delegation_id: "th-abc123".into(),
            status: "completed".into(),
            result: Some("All tests pass.".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"completed\""));
        assert!(json.contains("All tests pass."));
    }

    #[test]
    fn test_delegate_status_response_in_progress_no_result() {
        let resp = DelegateStatusResponse {
            delegation_id: "th-xyz789".into(),
            status: "in_progress".into(),
            result: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"in_progress\""));
        // result field should be absent (skip_serializing_if = None)
        assert!(!json.contains("\"result\""));
    }

    #[tokio::test]
    async fn test_delegate_endpoint_creates_pearl() {
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return; // Dolt binary not available, skip
        };
        let state = AppState::new(pearl_store);
        let app = build_router(state.clone());

        let body = serde_json::json!({
            "parent_operator_id": "op-test",
            "task": "Write unit tests for the auth module"
        });

        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/delegate")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["data"]["status"], "dispatched");
        let delegation_id = resp["data"]["delegation_id"].as_str().unwrap();
        assert!(delegation_id.starts_with("th-"), "pearl ID should start with th-");

        // Verify the pearl was created in the store
        let pearl = crate::pearls::get_pearl(&state.pearl_store, delegation_id)
            .unwrap()
            .expect("pearl should exist");
        assert!(pearl.title.contains("[delegated]"));
        assert_eq!(pearl.status, smooth_pearls::PearlStatus::Open);
    }

    #[tokio::test]
    async fn test_delegate_status_endpoint_returns_status() {
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return;
        };

        // Create a pearl directly to check status.
        let pearl = crate::pearls::create_pearl(&pearl_store, "test delegation", "test", "subtask", 1).unwrap();
        let pearl_id = pearl.id.clone();

        let state = AppState::new(pearl_store);
        let app = build_router(state.clone());

        // Check status — should be in_progress (Open maps to in_progress).
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(format!("/api/delegate/{pearl_id}/status"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["data"]["status"], "in_progress");
        assert_eq!(resp["data"]["delegation_id"], pearl_id);

        // Now close the pearl and check again.
        let _ = state.pearl_store.close(&[&pearl_id]);
        let app2 = build_router(state);
        let response2 = app2
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(format!("/api/delegate/{pearl_id}/status"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes2 = axum::body::to_bytes(response2.into_body(), usize::MAX).await.unwrap();
        let resp2: serde_json::Value = serde_json::from_slice(&body_bytes2).unwrap();
        assert_eq!(resp2["data"]["status"], "completed");
    }

    #[tokio::test]
    async fn test_delegate_status_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return;
        };
        let state = AppState::new(pearl_store);
        let app = build_router(state);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/delegate/th-nonexistent/status")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(resp["ok"], false);
        assert!(resp["data"]["error"].as_str().unwrap().contains("not found"));
    }
}

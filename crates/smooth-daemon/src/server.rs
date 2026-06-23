//! The always-on daemon's HTTP/WebSocket surface.
//!
//! Phase 1 implements the endpoints the frontends need to connect, run a task,
//! and resume state after a reconnect:
//! - `GET /health` — liveness + version (the TUI probes this before auto-start).
//! - `GET /ws` — the WebSocket the TUI and SPA already speak ([`crate::wire`]).
//! - `GET /api/event` — the durable Server-Sent-Events stream, replayed from a
//!   `?cursor=` seq so a frontend (or the daemon, post-restart) catches up with
//!   zero loss. This closes the resume gap opencode left stubbed.
//!
//! - `GET /api/session` — list/create sessions; `GET /api/session/{id}` fetch.
//!
//! On connect the WS sends [`ServerEvent::Connected`] immediately, then a
//! [`ServerEvent::Pong`] heartbeat every 30s (legacy behaviour). Connect with
//! `/ws?session=<id>` to resume an existing session (its durable conversation
//! history replays on the next `TaskStart`). Each `TaskStart` runs through the
//! [`SessionRunCoordinator`] so a session has at most one in-flight turn, and
//! events stream back over the same socket.
//!
//! Not yet here: loopback+tailnet bind hardening, and bearer-token auth.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::approval::ApprovalCoordinator;
use crate::coordinator::{SessionRunCoordinator, StartError};
use crate::event::{DaemonEvent, EventStore, InMemoryEventLog, Seq};
use crate::messages::MessageStore;
use crate::permission::PermissionMode;
use crate::runner::{self, TaskSpec};
use crate::session::{InMemorySessionStore, Session, SessionStatus, SessionStore};
use crate::wire::{ClientEvent, PriorMessage, ServerEvent};

const HEARTBEAT: Duration = Duration::from_secs(30);

/// How long the SSE refill loop waits before re-polling the event log when it's
/// caught up. Keeps live latency low without busy-spinning; the durable log +
/// cursor means nothing is missed between polls.
const SSE_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Max events pulled from the log per refill.
const SSE_BATCH: usize = 256;

/// Shared daemon state handed to every request. Cheap to clone (all `Arc`s).
#[derive(Clone)]
pub struct AppState {
    /// Per-session run serialization.
    pub coordinator: Arc<SessionRunCoordinator>,
    /// Durable event log backing the SSE resume endpoint.
    pub events: Arc<dyn EventStore>,
    /// Session registry backing the `/api/session` surface.
    pub sessions: Arc<dyn SessionStore>,
    /// Durable conversation history (for cross-restart resume).
    pub messages: Arc<dyn MessageStore>,
    /// Routes operator approval replies to waiting permission hooks.
    pub approvals: Arc<ApprovalCoordinator>,
    /// Gate-1 permission posture for this daemon.
    pub permission_mode: PermissionMode,
}

impl AppState {
    /// Build daemon state with the in-memory backends (dev/test).
    #[must_use]
    pub fn new() -> Self {
        Self {
            coordinator: SessionRunCoordinator::new(),
            events: Arc::new(InMemoryEventLog::new()),
            sessions: Arc::new(InMemorySessionStore::new()),
            messages: Arc::new(crate::messages::InMemoryMessageStore::new()),
            approvals: ApprovalCoordinator::new(),
            permission_mode: PermissionMode::default(),
        }
    }

    /// Build daemon state with **durable** SQLite-backed events + sessions at
    /// `db_path`, so the SSE event stream and session list survive a restart
    /// (Phase 2, th-bd0e22).
    ///
    /// # Errors
    /// Returns an error if the database cannot be opened/initialized.
    pub fn persistent(db_path: &std::path::Path) -> anyhow::Result<Self> {
        let stores = crate::sqlite::open_stores(db_path)?;
        Ok(Self {
            coordinator: SessionRunCoordinator::new(),
            events: stores.events,
            sessions: stores.sessions,
            messages: stores.messages,
            approvals: ApprovalCoordinator::new(),
            permission_mode: crate::config::resolve_permission_mode(),
        })
    }

    /// The default daemon database path: `SMOOTH_DAEMON_DB` if set, else
    /// `~/.smooth/daemon.db`.
    #[must_use]
    pub fn default_db_path() -> std::path::PathBuf {
        if let Ok(p) = std::env::var("SMOOTH_DAEMON_DB") {
            return std::path::PathBuf::from(p);
        }
        dirs_next::home_dir().map_or_else(|| std::path::PathBuf::from("daemon.db"), |h| h.join(".smooth").join("daemon.db"))
    }

    /// Build durable daemon state at the [`default_db_path`](Self::default_db_path).
    ///
    /// # Errors
    /// Returns an error if the database cannot be opened/initialized.
    pub fn persistent_default() -> anyhow::Result<Self> {
        Self::persistent(&Self::default_db_path())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the axum router for the daemon.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws_handler))
        .route("/api/event", get(event_stream_handler))
        .route("/api/session", get(list_sessions).post(create_session))
        .route("/api/session/{id}", get(get_session))
        .route("/api/session/{id}/messages", get(list_session_messages))
        .with_state(state)
        // The embedded control-surface SPA (fallback for non-API routes).
        .fallback_service(smooth_web::web_router())
}

/// Bind `addr` and serve until a shutdown signal (Ctrl-C / SIGTERM).
///
/// # Errors
/// Returns an error if the address cannot be bound or the server exits abnormally.
pub async fn serve(state: AppState, addr: SocketAddr) -> anyhow::Result<()> {
    serve_with_shutdown(state, addr, shutdown_signal()).await
}

/// Bind `addr` and serve until `shutdown` resolves.
///
/// # Errors
/// Returns an error if the address cannot be bound or the server exits abnormally.
pub async fn serve_with_shutdown<F>(state: AppState, addr: SocketAddr, shutdown: F) -> anyhow::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_on(listener, state, shutdown).await
}

/// Serve on an already-bound `listener` until `shutdown` resolves. Useful for
/// tests (bind to an ephemeral port, then assert clean shutdown).
///
/// # Errors
/// Returns an error if the server exits abnormally.
pub async fn serve_on<F>(listener: tokio::net::TcpListener, state: AppState, shutdown: F) -> anyhow::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    if let Ok(addr) = listener.local_addr() {
        tracing::info!(%addr, version = crate::version(), "smooth-daemon listening");
    }
    axum::serve(listener, build_router(state)).with_graceful_shutdown(shutdown).await?;
    tracing::info!("smooth-daemon stopped");
    Ok(())
}

/// Resolve when the process receives Ctrl-C or (on Unix) SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => tracing::warn!(error = %e, "could not install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "smooth-daemon",
        "version": crate::version(),
    }))
}

/// Body for `POST /api/session`.
#[derive(Debug, Default, Deserialize)]
struct CreateSessionBody {
    /// Optional human label for the new session.
    #[serde(default)]
    title: Option<String>,
}

/// `GET /api/session` — list sessions, newest activity first.
async fn list_sessions(State(state): State<AppState>) -> Result<Json<Vec<Session>>, StatusCode> {
    state.sessions.list().await.map(Json).map_err(internal_error)
}

/// `POST /api/session` — create a session.
async fn create_session(State(state): State<AppState>, Json(body): Json<CreateSessionBody>) -> Result<Json<Session>, StatusCode> {
    state.sessions.create(None, body.title).await.map(Json).map_err(internal_error)
}

/// `GET /api/session/{id}` — fetch one session (404 if unknown).
async fn get_session(Path(id): Path<String>, State(state): State<AppState>) -> Result<Json<Session>, StatusCode> {
    let session = state.sessions.get(&id).await.map_err(internal_error)?;
    session.ok_or(StatusCode::NOT_FOUND).map(Json)
}

/// `GET /api/session/{id}/messages` — the session's durable conversation
/// history (oldest first), for resuming a conversation in the UI.
async fn list_session_messages(Path(id): Path<String>, State(state): State<AppState>) -> Result<Json<Vec<crate::messages::StoredMessage>>, StatusCode> {
    state.messages.load(&id, PRIOR_HISTORY_LIMIT).await.map(Json).map_err(internal_error)
}

#[allow(clippy::needless_pass_by_value, reason = "used as a map_err fn-pointer, which passes the error by value")]
fn internal_error(e: anyhow::Error) -> StatusCode {
    tracing::error!(error = %e, "session store error");
    StatusCode::INTERNAL_SERVER_ERROR
}

/// Resolve the workspace root for a task: the `TaskStart.working_dir` if given,
/// else the daemon's current directory. Canonicalized best-effort so the tools'
/// path-confinement prefix check is reliable.
fn resolve_workspace(working_dir: Option<String>) -> PathBuf {
    let raw = working_dir
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

/// Max prior turns replayed into a resumed session.
const PRIOR_HISTORY_LIMIT: usize = 1000;

/// Load a session's durable conversation history as replayable prior messages.
async fn load_prior(state: &AppState, session_id: &str) -> Vec<PriorMessage> {
    state
        .messages
        .load(session_id, PRIOR_HISTORY_LIMIT)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|m| PriorMessage {
            role: m.role,
            content: m.content,
        })
        .collect()
}

/// Query parameters for [`event_stream_handler`].
#[derive(Debug, Deserialize)]
struct EventQuery {
    /// Resume from events with seq strictly greater than this (default 0 = from
    /// the beginning). A frontend persists the last seq it saw and passes it
    /// here on reconnect.
    #[serde(default)]
    cursor: Seq,
    /// Optional: restrict to one session's events (default = the global stream).
    #[serde(default)]
    session: Option<String>,
}

/// `GET /api/event` — durable, cursor-resumable SSE stream of [`DaemonEvent`]s.
///
/// Each SSE message carries the event seq as its `id`, so a client reconnecting
/// with `Last-Event-ID` / `?cursor=` resumes exactly where it left off with no
/// gaps and no duplicates.
async fn event_stream_handler(Query(q): Query<EventQuery>, State(state): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(event_stream(state.events, q.cursor, q.session)).keep_alive(KeepAlive::default())
}

/// Build the SSE event stream: replay everything after `cursor`, then live-tail.
///
/// Implemented as a poll loop over the durable [`EventStore`] (not a broadcast
/// subscription) so resume is trivially correct — the cursor is the only state,
/// and a restarted daemon with a durable log resumes identically. Phase 2's
/// Dolt-backed store slots in behind the same trait with no change here.
fn event_stream(events: Arc<dyn EventStore>, cursor: Seq, session: Option<String>) -> impl Stream<Item = Result<Event, Infallible>> {
    struct State {
        events: Arc<dyn EventStore>,
        cursor: Seq,
        session: Option<String>,
        buf: VecDeque<DaemonEvent>,
    }

    let init = State {
        events,
        cursor,
        session,
        buf: VecDeque::new(),
    };

    futures_util::stream::unfold(init, |mut st| async move {
        loop {
            if let Some(ev) = st.buf.pop_front() {
                let data = serde_json::to_string(&ev).unwrap_or_default();
                let sse = Event::default().id(ev.seq.to_string()).event("daemon").data(data);
                return Some((Ok(sse), st));
            }
            match st.events.since(st.cursor, st.session.as_deref(), SSE_BATCH).await {
                Ok(batch) if !batch.is_empty() => {
                    if let Some(last) = batch.last() {
                        st.cursor = last.seq;
                    }
                    st.buf.extend(batch);
                }
                // Caught up (or a transient read error): wait and re-poll. The
                // KeepAlive layer emits SSE comments meanwhile so the
                // connection stays warm.
                _ => tokio::time::sleep(SSE_POLL_INTERVAL).await,
            }
        }
    })
}

/// WebSocket connect query: `?session=<id>` resumes an existing session (so its
/// durable conversation history replays); omit it for a fresh session.
#[derive(Debug, Deserialize)]
struct WsConnectQuery {
    #[serde(default)]
    session: Option<String>,
}

async fn ws_handler(ws: WebSocketUpgrade, Query(q): Query<WsConnectQuery>, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state, q.session))
}

async fn handle_socket(socket: WebSocket, state: AppState, requested_session: Option<String>) {
    // Resume the requested session (durable history replays on TaskStart) or
    // mint a fresh one.
    let session_id = requested_session.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (mut sink, mut stream) = socket.split();

    // All outbound events funnel through one channel so the agent runner, the
    // heartbeat, and the read loop never touch the socket sink directly.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();

    let writer = tokio::spawn(async move {
        while let Some(ev) = out_rx.recv().await {
            let Ok(json) = serde_json::to_string(&ev) else { continue };
            if sink.send(Message::Text(json.into())).await.is_err() {
                break; // client gone
            }
        }
    });

    let heartbeat = {
        let hb_tx = out_tx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(HEARTBEAT);
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                if hb_tx.send(ServerEvent::Pong).is_err() {
                    break;
                }
            }
        })
    };

    // Register the session so it shows up in /api/session.
    let _ = state.sessions.create(Some(session_id.clone()), None).await;

    // Greet the client first — the TUI waits for this before considering the
    // connection live.
    let _ = out_tx.send(ServerEvent::Connected {
        session_id: session_id.clone(),
    });

    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => match serde_json::from_str::<ClientEvent>(text.as_str()) {
                Ok(ev) => handle_client_event(ev, &session_id, &state, &out_tx).await,
                Err(e) => {
                    let _ = out_tx.send(ServerEvent::Error {
                        message: format!("unparseable client message: {e}"),
                    });
                }
            },
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
        }
    }

    // Socket closed: stop any work tied to it and mark the session idle.
    state.coordinator.cancel_session(&session_id);
    let _ = state.sessions.set_status(&session_id, SessionStatus::Idle).await;
    heartbeat.abort();
    writer.abort();
}

/// Handle one decoded client message. Non-blocking on the agent run: a
/// `TaskStart` hands the run to the coordinator (which spawns it) so the read
/// loop stays responsive to `Steer`/`TaskCancel` mid-task. Only the quick
/// session-status writes are awaited.
async fn handle_client_event(ev: ClientEvent, session_id: &str, state: &AppState, out_tx: &UnboundedSender<ServerEvent>) {
    match ev {
        ClientEvent::TaskStart {
            message,
            model,
            budget,
            working_dir,
            ..
        } => {
            let task_id = uuid::Uuid::new_v4().to_string();
            // The daemon owns the conversation: load this session's durable
            // history as prior_messages (ignoring any client-sent copy), so a
            // turn continues the conversation even across a daemon restart.
            let prior_messages = load_prior(state, session_id).await;
            let spec = TaskSpec {
                task_id: task_id.clone(),
                session_id: session_id.to_owned(),
                message,
                model,
                budget,
                prior_messages,
                workspace: resolve_workspace(working_dir),
            };
            let out = out_tx.clone();
            let events = Arc::clone(&state.events);
            let messages = Arc::clone(&state.messages);
            let approvals = Arc::clone(&state.approvals);
            let mode = state.permission_mode;
            let run = async move { runner::run_task(spec, out, events, messages, approvals, mode).await };

            match state.coordinator.try_start(session_id.to_owned(), task_id.clone(), run) {
                Ok(()) => {
                    let _ = state.sessions.set_status(session_id, SessionStatus::Active).await;
                }
                Err(StartError::Busy { task_id: running, .. }) => {
                    let _ = out_tx.send(ServerEvent::TaskError {
                        task_id,
                        message: format!("session busy: task {running} is still running — cancel it or wait"),
                    });
                }
            }
        }
        ClientEvent::TaskCancel { task_id } => {
            state.coordinator.cancel_task(&task_id);
        }
        ClientEvent::Ping => {
            let _ = out_tx.send(ServerEvent::Pong);
        }
        ClientEvent::PermissionReply { request_id, allow } => {
            state.approvals.resolve(&request_id, allow);
        }
        // Acknowledged but not yet acted on in the daemon (later phases).
        ClientEvent::Steer { .. } | ClientEvent::PearlCreate { .. } | ClientEvent::PearlUpdate { .. } | ClientEvent::PearlClose { .. } => {}
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    async fn spawn_test_server() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = build_router(AppState::new());
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        addr
    }

    #[tokio::test]
    async fn health_reports_ok_and_version() {
        let Json(body) = health().await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "smooth-daemon");
        assert!(body["version"].as_str().is_some_and(|v| !v.is_empty()));
    }

    #[tokio::test]
    async fn ws_greets_with_connected_then_answers_ping_with_pong() {
        let addr = spawn_test_server().await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await.unwrap();

        // First frame must be Connected (TUI depends on this).
        let first = ws.next().await.unwrap().unwrap();
        let ev: ServerEvent = serde_json::from_str(first.to_text().unwrap()).unwrap();
        assert!(matches!(ev, ServerEvent::Connected { .. }), "first frame is Connected, got {ev:?}");

        // Ping → Pong.
        let ping = serde_json::to_string(&ClientEvent::Ping).unwrap();
        ws.send(TMessage::Text(ping.into())).await.unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        let ev: ServerEvent = serde_json::from_str(reply.to_text().unwrap()).unwrap();
        assert_eq!(ev, ServerEvent::Pong);
    }

    #[tokio::test]
    async fn task_start_without_llm_streams_a_task_error_over_the_socket() {
        // No LLM env configured → the run fails fast with a TaskError, proving
        // the full TaskStart → coordinator → runner → socket path end-to-end
        // without needing a real model.
        std::env::remove_var("SMOOTH_API_URL");
        std::env::remove_var("SMOOTH_API_KEY");
        std::env::set_var("SMOOTH_PROVIDERS_FILE", "/nonexistent/smooth-daemon/providers.json");

        let addr = spawn_test_server().await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await.unwrap();

        // Drain Connected.
        let _ = ws.next().await.unwrap().unwrap();

        let start = serde_json::to_string(&ClientEvent::TaskStart {
            message: "hi".into(),
            model: Some("m".into()),
            budget: None,
            working_dir: None,
            agent: None,
            prior_messages: vec![],
        })
        .unwrap();
        ws.send(TMessage::Text(start.into())).await.unwrap();

        // Expect a TaskError terminal (skip any heartbeat noise, though none is
        // due within the test window).
        let reply = ws.next().await.unwrap().unwrap();
        let ev: ServerEvent = serde_json::from_str(reply.to_text().unwrap()).unwrap();
        match ev {
            ServerEvent::TaskError { message, .. } => assert!(message.contains("config"), "got: {message}"),
            other => panic!("expected TaskError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn serve_on_returns_when_shutdown_fires() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        // Shutdown future already resolved → graceful shutdown triggers at once;
        // with no live connections, serve returns promptly.
        let res = tokio::time::timeout(Duration::from_secs(5), serve_on(listener, AppState::new(), async {})).await;
        assert!(res.is_ok(), "serve_on should return on shutdown, not hang");
        assert!(res.unwrap().is_ok());
    }

    #[tokio::test]
    async fn session_api_create_list_get_and_404() {
        let state = AppState::new();

        let Json(created) = create_session(State(state.clone()), Json(CreateSessionBody { title: Some("hack".into()) }))
            .await
            .expect("create ok");
        assert_eq!(created.title.as_deref(), Some("hack"));
        assert_eq!(created.status, SessionStatus::Idle);

        let Json(list) = list_sessions(State(state.clone())).await.expect("list ok");
        assert_eq!(list.len(), 1);

        let Json(got) = get_session(Path(created.id.clone()), State(state.clone())).await.expect("get ok");
        assert_eq!(got.id, created.id);

        let missing = get_session(Path("nope".into()), State(state.clone())).await.unwrap_err();
        assert_eq!(missing, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn sse_stream_resumes_from_cursor_then_live_tails() {
        use crate::event::{EventKind, InMemoryEventLog};

        let events: Arc<dyn EventStore> = Arc::new(InMemoryEventLog::new());
        for i in 0..3u8 {
            events.append("s1", EventKind::TokenDelta { text: format!("c{i}") }).await.unwrap();
            // seqs 1,2,3
        }

        // Resume from seq 1 → only seqs 2 and 3 should replay, then the stream
        // live-tails (blocks) because it has caught up.
        let stream = event_stream(Arc::clone(&events), 1, None);
        tokio::pin!(stream);

        for _ in 0..2 {
            let item = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("replay should not block")
                .expect("stream yields an item");
            assert!(item.is_ok());
        }

        // No more historical events after the cursor → the next pull blocks.
        let caught_up = tokio::time::timeout(Duration::from_millis(400), stream.next()).await;
        assert!(caught_up.is_err(), "stream live-tails once caught up (no replay of seq 1)");

        // A newly appended event is delivered live.
        events.append("s1", EventKind::TokenDelta { text: "live".into() }).await.unwrap();
        let live = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("live event should arrive");
        assert!(live.is_some());
    }
}

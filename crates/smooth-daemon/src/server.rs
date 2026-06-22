//! The always-on daemon's HTTP/WebSocket surface.
//!
//! Phase 1 implements the two endpoints the frontends need to connect and run
//! a task:
//! - `GET /health` — liveness + version (the TUI probes this before auto-start).
//! - `GET /ws` — the WebSocket the TUI and SPA already speak ([`crate::wire`]).
//!
//! On connect the daemon sends [`ServerEvent::Connected`] immediately, then a
//! [`ServerEvent::Pong`] heartbeat every 30s (legacy behaviour). Each
//! `TaskStart` runs through the [`SessionRunCoordinator`] so a session has at
//! most one in-flight turn, and events stream back over the same socket.
//!
//! Not yet here (later Phase 1 work): the durable `/api/event` SSE endpoint
//! with cursor resume, the `/api/session` REST surface, loopback+tailnet bind
//! hardening, and bearer-token auth.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::UnboundedSender;

use crate::coordinator::{SessionRunCoordinator, StartError};
use crate::event::{EventStore, InMemoryEventLog};
use crate::runner::{self, TaskSpec};
use crate::wire::{ClientEvent, ServerEvent};

const HEARTBEAT: Duration = Duration::from_secs(30);

/// Shared daemon state handed to every request. Cheap to clone (all `Arc`s).
#[derive(Clone)]
pub struct AppState {
    /// Per-session run serialization.
    pub coordinator: Arc<SessionRunCoordinator>,
    /// Durable event log backing the (future) SSE resume endpoint.
    pub events: Arc<dyn EventStore>,
}

impl AppState {
    /// Build daemon state with the in-memory event log (Phase 1 default).
    /// Phase 2 swaps in the Dolt-backed [`EventStore`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            coordinator: SessionRunCoordinator::new(),
            events: Arc::new(InMemoryEventLog::new()),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the axum router for the daemon.
pub fn build_router(state: AppState) -> Router {
    Router::new().route("/health", get(health)).route("/ws", get(ws_handler)).with_state(state)
}

/// Bind `addr` and serve until the process is stopped.
///
/// # Errors
/// Returns an error if the address cannot be bound or the server exits abnormally.
pub async fn serve(state: AppState, addr: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, version = crate::version(), "smooth-daemon listening");
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "smooth-daemon",
        "version": crate::version(),
    }))
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let session_id = uuid::Uuid::new_v4().to_string();
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

    // Greet the client first — the TUI waits for this before considering the
    // connection live.
    let _ = out_tx.send(ServerEvent::Connected {
        session_id: session_id.clone(),
    });

    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => match serde_json::from_str::<ClientEvent>(text.as_str()) {
                Ok(ev) => handle_client_event(ev, &session_id, &state, &out_tx),
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

    // Socket closed: stop any work tied to it.
    state.coordinator.cancel_session(&session_id);
    heartbeat.abort();
    writer.abort();
}

/// Handle one decoded client message. Synchronous and non-blocking: a
/// `TaskStart` hands the agent run to the coordinator (which spawns it) so the
/// read loop stays responsive to `Steer`/`TaskCancel` mid-task.
fn handle_client_event(ev: ClientEvent, session_id: &str, state: &AppState, out_tx: &UnboundedSender<ServerEvent>) {
    match ev {
        ClientEvent::TaskStart {
            message,
            model,
            budget,
            prior_messages,
            ..
        } => {
            let task_id = uuid::Uuid::new_v4().to_string();
            let spec = TaskSpec {
                task_id: task_id.clone(),
                session_id: session_id.to_owned(),
                message,
                model,
                budget,
                prior_messages,
            };
            let out = out_tx.clone();
            let events = Arc::clone(&state.events);
            let run = async move { runner::run_task(spec, out, events).await };

            if let Err(StartError::Busy { task_id: running, .. }) = state.coordinator.try_start(session_id.to_owned(), task_id.clone(), run) {
                let _ = out_tx.send(ServerEvent::TaskError {
                    task_id,
                    message: format!("session busy: task {running} is still running — cancel it or wait"),
                });
            }
        }
        ClientEvent::TaskCancel { task_id } => {
            state.coordinator.cancel_task(&task_id);
        }
        ClientEvent::Ping => {
            let _ = out_tx.send(ServerEvent::Pong);
        }
        // Acknowledged but not yet acted on in the daemon (later phases).
        ClientEvent::Steer { .. } | ClientEvent::PearlCreate { .. } | ClientEvent::PearlUpdate { .. } | ClientEvent::PearlClose { .. } => {}
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
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
}

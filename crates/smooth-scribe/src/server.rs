use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::forwarder::ForwarderHandle;
use crate::log_entry::LogEntry;
use crate::store::{LogStore, MemoryLogStore, Query};

/// Shared application state for the Scribe server.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<MemoryLogStore>,
    /// Optional Archivist forwarder. When `Some`, every `POST /log` entry
    /// is cloned into the forwarder channel for cross-VM aggregation.
    /// `None` keeps Scribe standalone (matching pre-Boardroom behavior).
    pub forwarder: Option<ForwarderHandle>,
}

impl AppState {
    /// Build a state that stores locally and does not forward anywhere.
    #[must_use]
    pub fn local_only() -> Self {
        Self {
            store: Arc::new(MemoryLogStore::new()),
            forwarder: None,
        }
    }

    /// Build a state that mirrors every entry to an Archivist forwarder.
    #[must_use]
    pub fn with_forwarder(forwarder: ForwarderHandle) -> Self {
        Self {
            store: Arc::new(MemoryLogStore::new()),
            forwarder: Some(forwarder),
        }
    }
}

/// Build the axum router for the Scribe HTTP server.
pub fn build_router() -> Router {
    build_router_with_state(AppState::local_only())
}

/// Build the axum router with a provided state (useful for testing).
pub fn build_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/log", post(post_log))
        .route("/logs", get(get_logs))
        .route("/health", get(health))
        .with_state(state)
}

async fn post_log(State(state): State<AppState>, Json(entry): Json<LogEntry>) -> StatusCode {
    if let Some(ref fwd) = state.forwarder {
        fwd.try_forward(entry.clone());
    }
    state.store.append(entry);
    StatusCode::CREATED
}

async fn get_logs(State(state): State<AppState>, axum::extract::Query(query): axum::extract::Query<Query>) -> Json<Vec<LogEntry>> {
    Json(state.store.query(&query))
}

async fn health() -> &'static str {
    "ok"
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::log_entry::LogLevel;

    fn app() -> Router {
        build_router()
    }

    #[tokio::test]
    async fn test_health() {
        let resp = app()
            .oneshot(Request::builder().uri("/health").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.expect("body").to_bytes();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn test_post_log() {
        let entry = LogEntry::new("test-svc", LogLevel::Info, "hello");
        let json = serde_json::to_string(&entry).expect("serialize");
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/log")
                    .header("content-type", "application/json")
                    .body(Body::from(json))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_get_logs() {
        let state = AppState::local_only();
        state.store.append(LogEntry::new("svc-a", LogLevel::Info, "one"));
        state.store.append(LogEntry::new("svc-b", LogLevel::Warn, "two"));

        let router = build_router_with_state(state);
        let resp = router
            .oneshot(Request::builder().uri("/logs").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.expect("body").to_bytes();
        let entries: Vec<LogEntry> = serde_json::from_slice(&body).expect("deserialize");
        assert_eq!(entries.len(), 2);
    }
}

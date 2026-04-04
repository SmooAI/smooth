use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::ingest::{IngestBatch, IngestResult};
use crate::store::{ArchiveQuery, ArchiveStats, ArchiveStore, MemoryArchiveStore};

/// Shared application state for the Archivist server.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<MemoryArchiveStore>,
}

/// Build the axum router for the Archivist HTTP server.
pub fn build_router() -> Router {
    let state = AppState {
        store: Arc::new(MemoryArchiveStore::new()),
    };
    build_router_with_state(state)
}

/// Build the axum router with a provided state (useful for testing).
pub fn build_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/ingest", post(post_ingest))
        .route("/query", get(get_query))
        .route("/stats", get(get_stats))
        .route("/health", get(health))
        .with_state(state)
}

async fn post_ingest(State(state): State<AppState>, Json(batch): Json<IngestBatch>) -> (StatusCode, Json<IngestResult>) {
    let result = state.store.ingest(batch);
    (StatusCode::OK, Json(result))
}

async fn get_query(State(state): State<AppState>, axum::extract::Query(query): axum::extract::Query<ArchiveQuery>) -> Json<Vec<smooth_scribe::LogEntry>> {
    Json(state.store.query(&query))
}

async fn get_stats(State(state): State<AppState>) -> Json<ArchiveStats> {
    Json(state.store.stats())
}

async fn health() -> &'static str {
    "ok"
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use smooth_scribe::{LogEntry, LogLevel};
    use tower::ServiceExt;

    use super::*;

    fn app() -> Router {
        build_router()
    }

    fn app_with_state() -> (Router, AppState) {
        let state = AppState {
            store: Arc::new(MemoryArchiveStore::new()),
        };
        (build_router_with_state(state.clone()), state)
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
    async fn test_post_ingest() {
        let batch = IngestBatch {
            entries: vec![LogEntry::new("svc", LogLevel::Info, "hello")],
            source_vm: "vm-1".to_string(),
        };
        let json = serde_json::to_string(&batch).expect("serialize");
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(json))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.expect("body").to_bytes();
        let result: IngestResult = serde_json::from_slice(&body).expect("deserialize");
        assert_eq!(result.accepted, 1);
        assert_eq!(result.rejected, 0);
    }

    #[tokio::test]
    async fn test_get_query() {
        let (router, state) = app_with_state();
        state.store.ingest(IngestBatch {
            entries: vec![LogEntry::new("svc", LogLevel::Info, "one"), LogEntry::new("svc", LogLevel::Warn, "two")],
            source_vm: "vm-1".to_string(),
        });

        let resp = router
            .oneshot(Request::builder().uri("/query").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.expect("body").to_bytes();
        let entries: Vec<LogEntry> = serde_json::from_slice(&body).expect("deserialize");
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn test_get_stats() {
        let (router, state) = app_with_state();
        state.store.ingest(IngestBatch {
            entries: vec![LogEntry::new("svc", LogLevel::Info, "a")],
            source_vm: "vm-1".to_string(),
        });
        state.store.ingest(IngestBatch {
            entries: vec![LogEntry::new("svc", LogLevel::Error, "b")],
            source_vm: "vm-2".to_string(),
        });

        let resp = router
            .oneshot(Request::builder().uri("/stats").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.expect("body").to_bytes();
        let stats: ArchiveStats = serde_json::from_slice(&body).expect("deserialize");
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.by_vm.len(), 2);
    }
}

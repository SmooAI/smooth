//! Diver HTTP server — axum router for pearl lifecycle management.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::jira::JiraClient;
use crate::store::{CompleteRequest, CostEntry, DispatchRequest, DispatchResult, DiverStore, SessionMessage};

/// Shared application state for the Diver server.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<DiverStore>,
    pub jira: Option<Arc<JiraClient>>,
}

/// Build the axum router with auto-detected Jira config.
pub fn build_router(store: DiverStore) -> Router {
    let jira = JiraClient::from_env().map(Arc::new);
    let state = AppState { store: Arc::new(store), jira };
    build_router_with_state(state)
}

/// Build the axum router with a provided state (useful for testing).
pub fn build_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/dispatch", post(post_dispatch))
        .route("/complete", post(post_complete))
        .route("/sub-pearl", post(post_sub_pearl))
        .route("/pearl/{id}", get(get_pearl))
        .route("/pearl/{id}/cost", post(post_cost))
        .route("/pearl/{id}/costs", get(get_costs))
        .route("/pearl/{id}/message", post(post_message))
        .route("/pearls", get(get_pearls))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn post_dispatch(State(state): State<AppState>, Json(req): Json<DispatchRequest>) -> Result<(StatusCode, Json<DispatchResult>), (StatusCode, String)> {
    let mut result = state.store.dispatch(&req).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Create Jira ticket if configured
    if let Some(ref jira) = state.jira {
        match jira.create_ticket(&req.title, &req.description).await {
            Ok(ticket) => {
                state.store.set_jira_key(&result.pearl.id, &ticket.key);
                result.jira_key = Some(ticket.key);
            }
            Err(e) => {
                tracing::warn!(error = %e, "diver: failed to create Jira ticket (non-fatal)");
            }
        }
    }

    Ok((StatusCode::CREATED, Json(result)))
}

async fn post_complete(State(state): State<AppState>, Json(req): Json<CompleteRequest>) -> Result<Json<smooth_pearls::Pearl>, (StatusCode, String)> {
    let pearl = state.store.complete(&req).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Transition Jira to Done and add completion comment
    if let Some(ref jira) = state.jira {
        if let Some(jira_key) = state.store.jira_key(&req.pearl_id) {
            if let Some(ref summary) = req.summary {
                let comment = format!("[Smooth] Task completed: {summary}");
                if let Err(e) = jira.add_comment(&jira_key, &comment).await {
                    tracing::warn!(error = %e, "diver: failed to add Jira completion comment");
                }
            }
            if let Err(e) = jira.transition_ticket(&jira_key, "done").await {
                tracing::warn!(error = %e, jira_key = %jira_key, "diver: failed to transition Jira (non-fatal)");
            }
        }
    }

    Ok(Json(pearl))
}

/// Request to create a sub-pearl.
#[derive(Debug, Deserialize)]
struct SubPearlRequest {
    parent_id: String,
    title: String,
    description: String,
}

async fn post_sub_pearl(
    State(state): State<AppState>,
    Json(req): Json<SubPearlRequest>,
) -> Result<(StatusCode, Json<smooth_pearls::Pearl>), (StatusCode, String)> {
    let pearl = state
        .store
        .create_sub_pearl(&req.parent_id, &req.title, &req.description)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok((StatusCode::CREATED, Json(pearl)))
}

async fn get_pearl(State(state): State<AppState>, Path(id): Path<String>) -> Result<Json<smooth_pearls::Pearl>, (StatusCode, String)> {
    let pearl = state
        .store
        .get(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("pearl {id} not found")))?;
    Ok(Json(pearl))
}

async fn post_cost(State(state): State<AppState>, Path(id): Path<String>, Json(mut entry): Json<CostEntry>) -> StatusCode {
    entry.pearl_id = id;
    state.store.record_cost(entry);
    StatusCode::CREATED
}

/// Post a session message and sync to Jira as a comment.
async fn post_message(State(state): State<AppState>, Path(id): Path<String>, Json(msg): Json<SessionMessage>) -> StatusCode {
    // Save as pearl comment for local tracking
    let comment_text = format!("[{} → {}] {}", msg.from, msg.to, msg.content);
    let _ = state.store.pearl_store().add_comment(&id, &comment_text);

    // Sync to Jira if configured
    if let Some(ref jira) = state.jira {
        if let Some(jira_key) = state.store.jira_key(&id) {
            let jira_comment = format!("[Smooth: {} → {}] {}", msg.from, msg.to, msg.content);
            if let Err(e) = jira.add_comment(&jira_key, &jira_comment).await {
                tracing::warn!(error = %e, pearl = %id, "diver: failed to sync message to Jira");
            }
        }
    }

    StatusCode::CREATED
}

async fn get_costs(State(state): State<AppState>, Path(id): Path<String>) -> Json<Vec<CostEntry>> {
    Json(state.store.costs(&id))
}

/// Query params for listing pearls.
#[derive(Debug, Deserialize)]
struct PearlListQuery {
    status: Option<String>,
}

async fn get_pearls(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<PearlListQuery>,
) -> Result<Json<Vec<smooth_pearls::Pearl>>, (StatusCode, String)> {
    let pearls = state
        .store
        .list(q.status.as_deref())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(pearls))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use smooth_pearls::PearlStore;
    use tower::ServiceExt;

    use super::*;

    fn test_app() -> Option<Router> {
        let tmp = tempfile::tempdir().ok()?;
        let dolt_dir = tmp.path().join("dolt");
        match PearlStore::init(&dolt_dir) {
            Ok(store) => {
                std::mem::forget(tmp);
                let diver_store = DiverStore::new(store);
                let state = AppState {
                    store: Arc::new(diver_store),
                    jira: None,
                };
                Some(build_router_with_state(state))
            }
            Err(_) => None,
        }
    }

    #[tokio::test]
    async fn test_health() {
        let Some(app) = test_app() else { return };
        let resp = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_dispatch_and_get() {
        let Some(app) = test_app() else { return };
        let body = serde_json::json!({
            "title": "Test dispatch",
            "description": "Testing the dispatch endpoint"
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/dispatch")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp_body = resp.into_body().collect().await.expect("body").to_bytes();
        let result: DispatchResult = serde_json::from_slice(&resp_body).expect("deserialize");
        assert_eq!(result.pearl.title, "Test dispatch");

        // GET the pearl
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/pearl/{}", result.pearl.id))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_dispatch_complete_cycle() {
        let Some(app) = test_app() else { return };
        // Dispatch
        let body = serde_json::json!({ "title": "Lifecycle test", "description": "" });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/dispatch")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");
        let resp_body = resp.into_body().collect().await.expect("body").to_bytes();
        let result: DispatchResult = serde_json::from_slice(&resp_body).expect("deserialize");

        // Complete
        let complete_body = serde_json::json!({
            "pearl_id": result.pearl.id,
            "summary": "All done",
            "cost_usd": 0.12
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&complete_body).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = resp.into_body().collect().await.expect("body").to_bytes();
        let pearl: smooth_pearls::Pearl = serde_json::from_slice(&resp_body).expect("deserialize");
        assert_eq!(pearl.status, smooth_pearls::PearlStatus::Closed);
    }

    #[tokio::test]
    async fn test_list_pearls() {
        let Some(app) = test_app() else { return };
        let resp = app
            .oneshot(Request::builder().uri("/pearls").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_get_nonexistent_pearl() {
        let Some(app) = test_app() else { return };
        let resp = app
            .oneshot(Request::builder().uri("/pearl/th-000000").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}

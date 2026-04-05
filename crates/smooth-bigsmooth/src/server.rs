//! Axum HTTP server — all REST routes, middleware, CORS.

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::db::Database;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub issue_store: smooth_issues::IssueStore,
    pub start_time: Instant,
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
        // Embedded web UI (SPA fallback — must be last)
        .fallback_service(smooth_web::web_router())
        // Middleware
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Start the leader HTTP server.
pub async fn start(state: AppState, addr: SocketAddr) -> anyhow::Result<()> {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Smooth leader running at http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

// ── Health ─────────────────────────────────────────────────

async fn health_handler(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        service: "smooth-leader".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        uptime: state.start_time.elapsed().as_secs_f64(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    })
}

async fn system_health_handler(State(state): State<AppState>) -> Json<ApiResponse<SystemHealth>> {
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

async fn get_config_handler(State(_state): State<AppState>) -> Json<ApiResponse<serde_json::Value>> {
    Json(ApiResponse {
        data: serde_json::json!({}),
        ok: true,
    })
}

async fn set_config_handler(State(state): State<AppState>, Json(body): Json<ConfigBody>) -> Json<ApiResponse<()>> {
    let value_str = serde_json::to_string(&body.value).unwrap_or_default();
    let _ = state.db.set_config(&body.key, &value_str);
    Json(ApiResponse { data: (), ok: true })
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
    let issues = crate::issues::list_issues(&state.issue_store, params.status.as_deref()).unwrap_or_default();
    Json(ApiResponse { data: issues, ok: true })
}

async fn get_issue_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    let issue = crate::issues::get_issue(&state.issue_store, &id).unwrap_or(None);
    let data = match issue {
        Some(i) => serde_json::to_value(i).unwrap_or(serde_json::json!(null)),
        None => serde_json::json!(null),
    };
    Json(ApiResponse { data, ok: true })
}

async fn ready_issues_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<smooth_issues::Issue>>> {
    let issues = crate::issues::get_ready(&state.issue_store).unwrap_or_default();
    Json(ApiResponse { data: issues, ok: true })
}

async fn create_issue_handler(State(state): State<AppState>, Json(body): Json<CreateIssueBody>) -> Json<ApiResponse<serde_json::Value>> {
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
    let stats = crate::issues::stats(&state.issue_store).unwrap_or_default();
    Json(ApiResponse { data: stats, ok: true })
}

// ── Workers ────────────────────────────────────────────────

async fn list_workers_handler() -> Json<ApiResponse<Vec<serde_json::Value>>> {
    // TODO: Query worker_runs from SQLite
    Json(ApiResponse { data: vec![], ok: true })
}

async fn get_worker_handler(Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    Json(ApiResponse {
        data: serde_json::json!({"id": id, "status": "unknown"}),
        ok: true,
    })
}

async fn kill_worker_handler(Path(id): Path<String>) -> Json<ApiResponse<()>> {
    tracing::info!("Kill worker {id}");
    Json(ApiResponse { data: (), ok: true })
}

// ── Messages ───────────────────────────────────────────────

async fn inbox_handler() -> Json<ApiResponse<Vec<serde_json::Value>>> {
    Json(ApiResponse { data: vec![], ok: true })
}

// ── Reviews ────────────────────────────────────────────────

async fn list_reviews_handler() -> Json<ApiResponse<Vec<serde_json::Value>>> {
    Json(ApiResponse { data: vec![], ok: true })
}

async fn approve_review_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<()>> {
    tracing::info!("Approve review for {bead_id}");
    let _ = state.issue_store.close(&[&bead_id]);
    Json(ApiResponse { data: (), ok: true })
}

async fn reject_review_handler(Path(bead_id): Path<String>) -> Json<ApiResponse<()>> {
    tracing::info!("Reject review for {bead_id}");
    Json(ApiResponse { data: (), ok: true })
}

// ── Chat ───────────────────────────────────────────────────

async fn chat_handler(Json(body): Json<ChatBody>) -> Json<ApiResponse<String>> {
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
    tracing::info!("Pause operator on {bead_id}");
    let _ = state.issue_store.add_comment(&bead_id, "[STEERING:PAUSE] Operator paused by human.");
    Json(ApiResponse {
        data: "paused".into(),
        ok: true,
    })
}

async fn resume_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    tracing::info!("Resume operator on {bead_id}");
    let _ = state.issue_store.add_comment(&bead_id, "[STEERING:RESUME] Operator resumed.");
    Json(ApiResponse {
        data: "resumed".into(),
        ok: true,
    })
}

async fn steer_handler(State(state): State<AppState>, Path(bead_id): Path<String>, Json(body): Json<SteerBody>) -> Json<ApiResponse<String>> {
    let msg = body.message.unwrap_or_default();
    tracing::info!("Steer operator on {bead_id}: {msg}");
    let _ = state.issue_store.add_comment(&bead_id, &format!("[STEERING:GUIDANCE] {msg}"));
    Json(ApiResponse {
        data: "steered".into(),
        ok: true,
    })
}

async fn cancel_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    tracing::info!("Cancel operator on {bead_id}");
    let _ = state.issue_store.add_comment(&bead_id, "[STEERING:CANCEL] Operator cancelled.");
    Json(ApiResponse {
        data: "cancelled".into(),
        ok: true,
    })
}

// ── Jira ───────────────────────────────────────────────────

async fn jira_status_handler(State(state): State<AppState>) -> Json<ApiResponse<crate::jira::SyncStatus>> {
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

async fn jira_sync_handler(State(_state): State<AppState>) -> Json<ApiResponse<crate::jira::SyncResult>> {
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
        let state = AppState {
            db,
            issue_store,
            start_time: Instant::now(),
        };
        let _router = build_router(state);
        // If we get here without panic, the router is valid
    }
}

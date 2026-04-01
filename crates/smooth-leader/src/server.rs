//! Axum HTTP server — routes, middleware, CORS.

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::get;
use axum::Router;
use serde::Serialize;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::db::Database;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub start_time: Instant,
}

/// Health check response.
#[derive(Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub service: String,
    pub version: String,
    pub uptime: f64,
    pub timestamp: String,
}

/// System health response.
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

/// Build the axum router with all routes.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/api/system/health", get(system_health_handler))
        .route("/api/system/config", get(get_config_handler))
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

// ── Route handlers ─────────────────────────────────────────

async fn health_handler(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        service: "smooth-leader".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        uptime: state.start_time.elapsed().as_secs_f64(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    })
}

async fn system_health_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let db_path = state.db.path().display().to_string();
    let db_ok = state.db.get_config("__health_check").is_ok();

    let health = SystemHealth {
        leader: LeaderHealth {
            status: "healthy".into(),
            uptime: state.start_time.elapsed().as_secs_f64(),
        },
        database: DatabaseHealth {
            status: if db_ok { "healthy" } else { "down" }.into(),
            path: db_path,
        },
        sandbox: SandboxHealth {
            status: "healthy".into(),
            backend: "local-microsandbox".into(),
            active_sandboxes: 0,
            max_concurrency: 3,
        },
        tailscale: TailscaleHealth {
            status: "disconnected".into(),
            hostname: None,
        },
        beads: BeadsHealth {
            status: "healthy".into(),
            open_issues: 0,
        },
    };

    Json(serde_json::json!({ "data": health, "ok": true }))
}

async fn get_config_handler(State(state): State<AppState>) -> Result<Json<serde_json::Value>, StatusCode> {
    // Return all config as JSON object
    let conn = state.db.get_config("__all");
    drop(conn);
    Ok(Json(serde_json::json!({ "data": {}, "ok": true })))
}

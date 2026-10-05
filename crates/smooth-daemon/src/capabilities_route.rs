//! `GET /api/capabilities` — this daemon's version and the capability names it
//! serves (ADR-012). Clients feature-detect against these instead of assuming
//! the daemon was released in lockstep with them: the Big Smooth app bundles
//! its own daemon on its own cadence. Public metadata, so it is ungated, like
//! `/health`.

use axum::routing::get;
use axum::{Json, Router};
use smooth_policy::daemon::DaemonCapabilities;

/// Build the `/api/capabilities` router.
pub fn capabilities_router() -> Router {
    Router::new().route("/api/capabilities", get(capabilities_handler))
}

async fn capabilities_handler() -> Json<DaemonCapabilities> {
    Json(DaemonCapabilities::current(env!("CARGO_PKG_VERSION")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn reports_this_build_version_and_every_capability() {
        let resp = capabilities_router()
            .oneshot(Request::get("/api/capabilities").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let caps: DaemonCapabilities = serde_json::from_slice(&bytes).expect("JSON capabilities body");
        assert_eq!(caps.version, env!("CARGO_PKG_VERSION"));
        for c in smooth_policy::daemon::CAPABILITIES {
            assert!(caps.has(c.name), "missing {}", c.name);
        }
        assert!(caps.has(smooth_policy::daemon::CAP_SESSION_WORKSPACES));
    }
}

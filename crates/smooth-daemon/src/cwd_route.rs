//! `GET`/`POST /api/session/cwd` — the web UI's `/cd` and `/pwd`.
//!
//! Slash commands typed in chat would otherwise flow straight to the LLM (the
//! operator's `LocalServer` owns the WS `send_message` path; the daemon can't
//! intercept it without forking the engine). So `/cd` is handled UI-side: the
//! composer detects a leading `/cd`, POSTs here to set the conversation's cwd,
//! and echoes a system line. The agent-driven path is the `cd` TOOL — both
//! write the SAME [`SessionCwd`] store, so a `/cd` and a `cd` tool call are
//! interchangeable within a conversation.
//!
//! `POST {session, path}` sets + returns the resolved dir (400 on out-of-root /
//! missing / not-a-directory). `GET ?session=…` reads the current dir (the
//! root when unset) for `/pwd`.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use smooth_tools::SessionCwd;

#[derive(Deserialize)]
struct SetCwdBody {
    /// The conversation id (the operator's per-turn key). Empty ⇒ the default
    /// session bucket, matching a turn with no resolved conversation.
    #[serde(default)]
    session: String,
    /// Target directory. Empty or `~` resets to the root.
    #[serde(default)]
    path: String,
}

#[derive(Deserialize)]
struct GetCwdQuery {
    #[serde(default)]
    session: String,
}

#[derive(Deserialize)]
struct AddWorkspaceBody {
    #[serde(default)]
    session: String,
    path: String,
    #[serde(default)]
    select: bool,
    #[serde(default)]
    user_path: String,
}

#[derive(Clone)]
struct WorkspaceState {
    cwd: SessionCwd,
    token: Option<String>,
}

#[derive(Serialize)]
struct CwdReply {
    cwd: String,
    root: String,
}

/// `POST /api/session/cwd` — set the conversation's cwd. 400 with the error
/// message when the path escapes the root / doesn't exist / isn't a directory.
async fn set_cwd(State(state): State<WorkspaceState>, Json(body): Json<SetCwdBody>) -> Result<Json<CwdReply>, (StatusCode, String)> {
    let resolved = state.cwd.set(&body.session, &body.path).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(CwdReply {
        cwd: resolved.display().to_string(),
        root: state.cwd.root().display().to_string(),
    }))
}

/// `GET /api/session/cwd?session=…` — the conversation's current cwd (root when
/// unset), for `/pwd`.
async fn get_cwd(State(state): State<WorkspaceState>, Query(q): Query<GetCwdQuery>) -> Json<CwdReply> {
    Json(CwdReply {
        cwd: state.cwd.get(&q.session).display().to_string(),
        root: state.cwd.root().display().to_string(),
    })
}

/// `POST /api/session/workspaces` explicitly opens a directory for one session
/// and selects it as that conversation's current working directory.
async fn add_workspace(
    State(state): State<WorkspaceState>,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
    Json(body): Json<AddWorkspaceBody>,
) -> Result<Json<CwdReply>, (StatusCode, String)> {
    if !crate::flow_route::authorized(state.token.as_deref(), &headers, &query) {
        return Err((StatusCode::UNAUTHORIZED, "missing or invalid local token".into()));
    }
    if body.session.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "a conversation session id is required".into()));
    }
    if body.user_path.len() > 16_384 {
        return Err((StatusCode::BAD_REQUEST, "user PATH exceeds the 16 KiB limit".into()));
    }
    if !body.user_path.is_empty() {
        state.cwd.set_user_path(&body.session, &body.user_path);
    }
    let root = state
        .cwd
        .add_session_root(&body.session, std::path::Path::new(&body.path))
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let resolved = if body.select {
        state
            .cwd
            .set(&body.session, root.to_str().unwrap_or_default())
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    } else {
        state.cwd.get(&body.session)
    };
    Ok(Json(CwdReply {
        cwd: resolved.display().to_string(),
        root: state.cwd.root().display().to_string(),
    }))
}

/// The `/api/session/cwd` router, backed by the shared [`SessionCwd`] store.
pub fn cwd_router(cwd: SessionCwd, token: Option<String>) -> Router {
    Router::new()
        .route("/api/session/cwd", post(set_cwd).get(get_cwd))
        .route("/api/session/workspaces", post(add_workspace))
        .with_state(WorkspaceState { cwd, token })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    fn fixture() -> (tempfile::TempDir, Router) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo")).unwrap();
        let router = cwd_router(SessionCwd::new(tmp.path().to_path_buf()), None);
        (tmp, router)
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn post_sets_and_returns_cwd() {
        let (tmp, router) = fixture();
        let req = Request::builder()
            .method("POST")
            .uri("/api/session/cwd")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"session":"c1","path":"repo"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["cwd"], tmp.path().join("repo").canonicalize().unwrap().display().to_string());
    }

    #[tokio::test]
    async fn post_out_of_root_is_400() {
        let (_tmp, router) = fixture();
        let req = Request::builder()
            .method("POST")
            .uri("/api/session/cwd")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"session":"c1","path":"../escape"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_returns_root_when_unset() {
        let (tmp, router) = fixture();
        let req = Request::builder()
            .method("GET")
            .uri("/api/session/cwd?session=fresh")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let root = tmp.path().canonicalize().unwrap().display().to_string();
        assert_eq!(json["cwd"], root);
        assert_eq!(json["root"], root);
    }

    #[tokio::test]
    async fn explicitly_opens_a_workspace_for_one_conversation() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        let secondary = tmp.path().join("secondary");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&secondary).unwrap();
        let router = cwd_router(SessionCwd::new(primary.clone()), None);
        let req = Request::builder()
            .method("POST")
            .uri("/api/session/workspaces")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({"session":"conv-1", "path":secondary, "select":true}).to_string()))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let req = Request::builder()
            .method("POST")
            .uri("/api/session/cwd")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({"session":"conv-2", "path":secondary}).to_string()))
            .unwrap();
        assert_eq!(router.oneshot(req).await.unwrap().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn authenticated_workspace_open_records_the_callers_path() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        let secondary = tmp.path().join("secondary");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&secondary).unwrap();
        let cwd = SessionCwd::new(primary);
        let router = cwd_router(cwd.clone(), Some("secret-token".into()));
        let req = Request::builder()
            .method("POST")
            .uri("/api/session/workspaces?token=secret-token")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "session":"conv-1",
                    "path":secondary,
                    "select":true,
                    "user_path":"/user/bin:/usr/bin"
                })
                .to_string(),
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(cwd.user_path("conv-1").unwrap(), std::ffi::OsString::from("/user/bin:/usr/bin"));
    }

    #[tokio::test]
    async fn adding_a_workspace_requires_the_local_daemon_token() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        let secondary = tmp.path().join("secondary");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&secondary).unwrap();
        let router = cwd_router(SessionCwd::new(primary), Some("secret-token".into()));
        let req = Request::builder()
            .method("POST")
            .uri("/api/session/workspaces")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({"session":"conv-1", "path":secondary}).to_string()))
            .unwrap();
        assert_eq!(router.oneshot(req).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    }
}

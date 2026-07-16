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
use axum::http::StatusCode;
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

#[derive(Serialize)]
struct CwdReply {
    cwd: String,
    root: String,
}

/// `POST /api/session/cwd` — set the conversation's cwd. 400 with the error
/// message when the path escapes the root / doesn't exist / isn't a directory.
async fn set_cwd(State(cwd): State<SessionCwd>, Json(body): Json<SetCwdBody>) -> Result<Json<CwdReply>, (StatusCode, String)> {
    let resolved = cwd.set(&body.session, &body.path).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(CwdReply {
        cwd: resolved.display().to_string(),
        root: cwd.root().display().to_string(),
    }))
}

/// `GET /api/session/cwd?session=…` — the conversation's current cwd (root when
/// unset), for `/pwd`.
async fn get_cwd(State(cwd): State<SessionCwd>, Query(q): Query<GetCwdQuery>) -> Json<CwdReply> {
    Json(CwdReply {
        cwd: cwd.get(&q.session).display().to_string(),
        root: cwd.root().display().to_string(),
    })
}

/// The `/api/session/cwd` router, backed by the shared [`SessionCwd`] store.
pub fn cwd_router(cwd: SessionCwd) -> Router {
    Router::new().route("/api/session/cwd", post(set_cwd).get(get_cwd)).with_state(cwd)
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
        let router = cwd_router(SessionCwd::new(tmp.path().to_path_buf()));
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
}

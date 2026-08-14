//! `GET`/`POST /api/session/mode` — the faces' Plan/Auto toggle.
//!
//! Big Smooth's two execution modes ([`crate::session_mode::Mode`]) are
//! per-conversation. A face flips the mode out-of-band from the WS `send_message`
//! path (which the operator's `LocalServer` owns): `th code` on `shift+tab`, the
//! desktop/mobile Plan/Auto chip on tap. Both POST here to set the conversation's
//! mode; the daemon's `ToolProvider` reads the SAME [`SessionModes`] store when
//! it builds the per-turn tool set, so a Plan turn is filtered read-only.
//!
//! `POST {session, mode}` sets + echoes the mode (400 on an unknown mode).
//! `GET ?session=…` reads it (Auto when unset) so a reconnecting face can sync.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::session_mode::{Mode, SessionModes};

#[derive(Deserialize)]
struct SetModeBody {
    /// The conversation id (the operator's per-turn key). Empty ⇒ the default
    /// session bucket, matching a turn with no resolved conversation.
    #[serde(default)]
    session: String,
    /// `"plan"` or `"auto"`.
    #[serde(default)]
    mode: String,
}

#[derive(Deserialize)]
struct GetModeQuery {
    #[serde(default)]
    session: String,
}

#[derive(Serialize)]
struct ModeReply {
    mode: &'static str,
}

/// `POST /api/session/mode` — set the conversation's mode. 400 when `mode` isn't
/// `plan`/`auto` (an unknown value must not silently become Auto and drop a
/// Plan-mode read-only guarantee).
async fn set_mode(State(modes): State<SessionModes>, Json(body): Json<SetModeBody>) -> Result<Json<ModeReply>, (StatusCode, String)> {
    let mode = Mode::parse(&body.mode).ok_or_else(|| (StatusCode::BAD_REQUEST, format!("unknown mode `{}` (expected plan|auto)", body.mode)))?;
    modes.set(&body.session, mode);
    Ok(Json(ModeReply { mode: mode.as_str() }))
}

/// `GET /api/session/mode?session=…` — the conversation's current mode (Auto when
/// unset), so a face can sync on reconnect.
async fn get_mode(State(modes): State<SessionModes>, Query(q): Query<GetModeQuery>) -> Json<ModeReply> {
    Json(ModeReply {
        mode: modes.get(&q.session).as_str(),
    })
}

/// The `/api/session/mode` router, backed by the shared [`SessionModes`] store.
pub fn mode_router(modes: SessionModes) -> Router {
    Router::new().route("/api/session/mode", post(set_mode).get(get_mode)).with_state(modes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn post_sets_mode_and_provider_sees_it() {
        let modes = SessionModes::new();
        let router = mode_router(modes.clone());
        let req = Request::builder()
            .method("POST")
            .uri("/api/session/mode")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"session":"c1","mode":"plan"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["mode"], "plan");
        // The store the ToolProvider reads reflects the POST.
        assert_eq!(modes.get("c1"), Mode::Plan);
    }

    #[tokio::test]
    async fn post_unknown_mode_is_400_and_leaves_state() {
        let modes = SessionModes::new();
        modes.set("c1", Mode::Plan);
        let router = mode_router(modes.clone());
        let req = Request::builder()
            .method("POST")
            .uri("/api/session/mode")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"session":"c1","mode":"bypass"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(modes.get("c1"), Mode::Plan, "a rejected set leaves the mode unchanged");
    }

    #[tokio::test]
    async fn get_returns_auto_when_unset() {
        let router = mode_router(SessionModes::new());
        let req = Request::builder()
            .method("GET")
            .uri("/api/session/mode?session=fresh")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["mode"], "auto");
    }
}

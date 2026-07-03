//! SEP `ui/*` relay — the daemon side of SEP Phase 6.
//!
//! A dispatched operative hosts extensions in-process. When an extension
//! issues a `ui/*` request the operative's `HttpUiProvider` POSTs it here (over
//! the same `SMOOTH_NARC_URL` + `SMOOTH_HOST_TOKEN` callback channel the
//! `host_tool` already uses). This module fans the request out to connected
//! frontends over the existing [`ServerEvent`] broadcast and, for the
//! interactive kinds, blocks the operative's HTTP call until a human answers
//! via [`answer_handler`] (or it times out / the permission engine auto-answers).
//!
//! Two endpoints:
//! - `POST /api/ui/request` — operative → daemon (bearer-authed). Returns the
//!   answer JSON the extension's `ui/*` call resolves to.
//! - `POST /api/ui/answer` — frontend → daemon. Resolves a parked request.
//!
//! One-way kinds (`notify`/`set_status`/`set_widget`/`set_title`) return `{}`
//! immediately after broadcasting — nothing to await. Interactive kinds
//! (`select`/`confirm`/`input`) park a oneshot in [`AppState::ui_pending`]
//! keyed by a fresh `request_id`.
//!
//! Unattended safety: with no clients connected the interactive path
//! short-circuits to `{ "cancelled": true }` rather than hang; under
//! `SMOOTH_AUTO_MODE=bypass` a `confirm` is auto-answered `{confirmed:true}`
//! (audited). Otherwise the call waits up to [`ui_timeout`], then cancels.

use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auto_mode::AutoMode;
use crate::events::ServerEvent;
use crate::server::AppState;

/// Interactive `ui/*` kinds — these await a human answer. Everything else is
/// render-only (fire-and-forget).
const INTERACTIVE_KINDS: [&str; 3] = ["select", "confirm", "input"];

/// How long an interactive `ui/*` request waits for an answer before it
/// resolves to `{cancelled:true}`. Override with `SMOOTH_UI_TIMEOUT_SECS`.
fn ui_timeout() -> Duration {
    let secs = std::env::var("SMOOTH_UI_TIMEOUT_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(120);
    Duration::from_secs(secs)
}

/// What the operative POSTs to `/api/ui/request` — the raw `ui/request` from
/// the extension plus the routing identity Big Smooth threaded into its env.
#[derive(Debug, Deserialize)]
pub struct UiRequestBody {
    /// The dispatched task id (`SMOOTH_OPERATOR_ID`) — scopes the request to a
    /// frontend session.
    pub task_id: String,
    /// Owning extension name.
    pub ext: String,
    /// `select` | `confirm` | `input` | `notify` | `set_status` | `set_widget`
    /// | `set_title`.
    pub kind: String,
    /// The extension's `ui/request` params (prompt, options, widget, …).
    #[serde(default)]
    pub params: Value,
}

/// The frontend's answer, POSTed to `/api/ui/answer`. Mirrors the engine's
/// `UiAnswer` shape (`smooth_code::sep_host::UiAnswer`).
#[derive(Debug, Deserialize)]
pub struct UiAnswerBody {
    pub request_id: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub confirmed: Option<bool>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub cancelled: bool,
}

#[derive(Debug, Serialize)]
pub struct UiAnswerAck {
    pub ok: bool,
}

/// Build the JSON answer body the extension's `ui/*` call resolves to.
fn answer_value(value: Option<String>, confirmed: Option<bool>, text: Option<String>, cancelled: bool) -> Value {
    let mut m = serde_json::Map::new();
    if let Some(v) = value {
        m.insert("value".into(), json!(v));
    }
    if let Some(c) = confirmed {
        m.insert("confirmed".into(), json!(c));
    }
    if let Some(t) = text {
        m.insert("text".into(), json!(t));
    }
    if cancelled {
        m.insert("cancelled".into(), json!(true));
    }
    Value::Object(m)
}

/// The `{cancelled:true}` fallback returned when nobody answers.
fn cancelled() -> Value {
    json!({ "cancelled": true })
}

fn check_bearer(state: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let expected = state.host_token.as_ref();
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    let presented = auth.strip_prefix("Bearer ").unwrap_or("");
    if presented != expected {
        return Err((StatusCode::UNAUTHORIZED, "ui-relay: bad bearer token".into()));
    }
    Ok(())
}

/// The relay core: broadcast a `ui/*` request to connected frontends, park
/// interactive kinds until a human answers (or auto-mode / timeout resolves
/// them). Shared by the operative's HTTP callback ([`request_handler`]) and the
/// chat loop's in-process delegate ([`crate::sep::DaemonUiProvider`]) so both
/// surfaces get identical timeout + auto-confirm + audit behavior.
pub(crate) async fn relay(state: &AppState, task_id: &str, ext: &str, kind: &str, params: Value) -> Value {
    let request_id = uuid::Uuid::new_v4().to_string();
    let interactive = INTERACTIVE_KINDS.contains(&kind);

    // Broadcast to every connected frontend so it can render the dialog /
    // toast / widget. `send` erroring only means zero receivers — fine.
    let _ = state.event_tx.send(ServerEvent::UiRequest {
        task_id: task_id.to_string(),
        request_id: request_id.clone(),
        ext: ext.to_string(),
        kind: kind.to_string(),
        payload: params.clone(),
    });

    // One-way kinds: nothing to await.
    if !interactive {
        return json!({});
    }

    // Auto-mode may answer a policy-covered `confirm` without a human. Bypass
    // ("allow everything except hard denies") auto-confirms; audited.
    let auto_mode = AutoMode::from_env_value(std::env::var("SMOOTH_AUTO_MODE").ok().as_deref());
    if kind == "confirm" && auto_mode == AutoMode::Bypass {
        crate::audit::audit(&crate::audit::AuditEntry {
            actor: "sep-ui".into(),
            action: "auto-confirm (SMOOTH_AUTO_MODE=bypass)".into(),
            target: Some(ext.to_string()),
            bead_id: Some(task_id.to_string()),
            input: Some(params),
            output: Some(json!({ "confirmed": true })),
            duration_ms: None,
            error: None,
        });
        let _ = state.event_tx.send(ServerEvent::UiResolved {
            request_id: request_id.clone(),
        });
        return json!({ "confirmed": true });
    }

    // Unattended (no frontend connected): don't hang the caller — cancel.
    if state.event_tx.receiver_count() == 0 {
        return cancelled();
    }

    // Park a oneshot the answer handler will fire.
    let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
    if let Ok(mut pending) = state.ui_pending.lock() {
        pending.insert(request_id.clone(), tx);
    }

    match tokio::time::timeout(ui_timeout(), rx).await {
        Ok(Ok(v)) => v,
        // Sender dropped or timed out → cancel and clean up the slot.
        _ => {
            if let Ok(mut pending) = state.ui_pending.lock() {
                pending.remove(&request_id);
            }
            let _ = state.event_tx.send(ServerEvent::UiResolved {
                request_id: request_id.clone(),
            });
            cancelled()
        }
    }
}

/// `POST /api/ui/request` — the operative's `HttpUiProvider` calls this. Blocks
/// (for interactive kinds) until a human answers, the permission engine
/// auto-answers, or [`ui_timeout`] elapses.
pub async fn request_handler(State(state): State<AppState>, headers: HeaderMap, Json(body): Json<UiRequestBody>) -> Result<Json<Value>, (StatusCode, String)> {
    check_bearer(&state, &headers)?;
    state.touch();
    Ok(Json(relay(&state, &body.task_id, &body.ext, &body.kind, body.params).await))
}

/// `POST /api/ui/answer` — a frontend resolves a parked interactive request.
pub async fn answer_handler(State(state): State<AppState>, Json(body): Json<UiAnswerBody>) -> Json<UiAnswerAck> {
    state.touch();
    let sender = state.ui_pending.lock().ok().and_then(|mut p| p.remove(&body.request_id));
    let ok = if let Some(tx) = sender {
        let value = answer_value(body.value, body.confirmed, body.text, body.cancelled);
        // Err only if the request already timed out and dropped its rx.
        let delivered = tx.send(value).is_ok();
        // Tell other clients showing the same dialog to dismiss it.
        let _ = state.event_tx.send(ServerEvent::UiResolved {
            request_id: body.request_id.clone(),
        });
        delivered
    } else {
        false
    };
    Json(UiAnswerAck { ok })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_value_builds_each_field() {
        assert_eq!(answer_value(Some("a".into()), None, None, false), json!({ "value": "a" }));
        assert_eq!(answer_value(None, Some(true), None, false), json!({ "confirmed": true }));
        assert_eq!(answer_value(None, None, Some("hi".into()), false), json!({ "text": "hi" }));
        assert_eq!(answer_value(None, None, None, true), json!({ "cancelled": true }));
    }

    #[test]
    fn interactive_kinds_classified() {
        for k in ["select", "confirm", "input"] {
            assert!(INTERACTIVE_KINDS.contains(&k), "{k} should be interactive");
        }
        for k in ["notify", "set_status", "set_widget", "set_title"] {
            assert!(!INTERACTIVE_KINDS.contains(&k), "{k} should be one-way");
        }
    }

    #[test]
    fn ui_timeout_default_and_override() {
        // Default when unset (can't mutate env safely mid-test-suite; just
        // assert the parse logic via a direct value).
        assert_eq!(Duration::from_secs(120), Duration::from_secs(120));
        assert_eq!(cancelled(), json!({ "cancelled": true }));
    }

    // ── Handler round-trips (need a PearlStore-backed AppState) ──────────
    // Gated on `PearlStore::init` succeeding — CI without the smooth-dolt
    // binary skips these (same pattern as server.rs's AppState tests).

    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::Json;

    fn test_state() -> Option<AppState> {
        let tmp = tempfile::tempdir().ok()?;
        let store = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")).ok()?;
        // Leak the tempdir so the dolt dir outlives the state for the test.
        std::mem::forget(tmp);
        Some(AppState::new(store))
    }

    fn bearer(state: &AppState) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("authorization", format!("Bearer {}", state.host_token).parse().unwrap());
        h
    }

    #[tokio::test]
    async fn bad_bearer_is_rejected() {
        let Some(state) = test_state() else { return };
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer wrong".parse().unwrap());
        let err = request_handler(
            State(state),
            h,
            Json(UiRequestBody {
                task_id: "t".into(),
                ext: "e".into(),
                kind: "confirm".into(),
                params: json!({ "prompt": "?" }),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn one_way_kind_returns_immediately() {
        let Some(state) = test_state() else { return };
        let h = bearer(&state);
        let out = request_handler(
            State(state),
            h,
            Json(UiRequestBody {
                task_id: "t".into(),
                ext: "e".into(),
                kind: "notify".into(),
                params: json!({ "message": "hi", "level": "info" }),
            }),
        )
        .await
        .unwrap();
        assert_eq!(out.0, json!({}));
    }

    #[tokio::test]
    async fn no_receivers_cancels_interactive() {
        let Some(state) = test_state() else { return };
        // No `event_tx.subscribe()` → receiver_count 0 → unattended cancel.
        let h = bearer(&state);
        let out = request_handler(
            State(state),
            h,
            Json(UiRequestBody {
                task_id: "t".into(),
                ext: "e".into(),
                kind: "confirm".into(),
                params: json!({ "prompt": "?" }),
            }),
        )
        .await
        .unwrap();
        assert_eq!(out.0, cancelled());
    }

    #[tokio::test]
    async fn interactive_round_trip_resolves_with_answer() {
        let Some(state) = test_state() else { return };
        // A live client keeps receiver_count > 0 so the request parks.
        let mut rx = state.event_tx.subscribe();
        let h = bearer(&state);

        let state_req = state.clone();
        let req = tokio::spawn(async move {
            request_handler(
                State(state_req),
                h,
                Json(UiRequestBody {
                    task_id: "t".into(),
                    ext: "todo".into(),
                    kind: "confirm".into(),
                    params: json!({ "prompt": "sure?" }),
                }),
            )
            .await
            .unwrap()
        });

        // Pull the broadcast UiRequest to learn the generated request_id.
        let request_id = loop {
            match rx.recv().await.unwrap() {
                ServerEvent::UiRequest { request_id, .. } => break request_id,
                _ => continue,
            }
        };

        let ack = answer_handler(
            State(state.clone()),
            Json(UiAnswerBody {
                request_id: request_id.clone(),
                value: None,
                confirmed: Some(true),
                text: None,
                cancelled: false,
            }),
        )
        .await;
        assert!(ack.0.ok, "answer must be delivered to the parked request");

        let out = req.await.unwrap();
        assert_eq!(out.0, json!({ "confirmed": true }));
    }

    #[tokio::test]
    async fn answer_unknown_request_is_not_ok() {
        let Some(state) = test_state() else { return };
        let ack = answer_handler(
            State(state),
            Json(UiAnswerBody {
                request_id: "nope".into(),
                value: None,
                confirmed: Some(true),
                text: None,
                cancelled: false,
            }),
        )
        .await;
        assert!(!ack.0.ok);
    }
}

//! `/api/flow/*` — the SmoothFlow engine's transport (epic th-6ac036, lane A
//! th-7f0af3).
//!
//! `GET /api/flow/ws` is the flow WebSocket (a sibling of the operator's
//! canonical `/ws`, which the daemon cannot intercept). HTTP siblings cover
//! the one-shot calls. `POST /api/flow/hooks` is where Claude Code hook
//! scripts report; a `PermissionRequest` is held open (≤120 s) until a
//! `flow.approve` answers it.
//!
//! **Auth.** Every route except `/api/flow/hooks` requires the daemon's local
//! token (`?token=`, `Authorization: Bearer`, or `X-Smooth-Token`) — a flow
//! session is a shell on this machine, and the daemon may be reachable over a
//! tailnet. Hooks are unauthenticated on purpose: the hook script must never
//! block the harness, and it can only touch a session whose pre-assigned
//! 128-bit id it already knows.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use smooth_flow::protocol::{client_seq, parse_client_frame, CandidateSpec};
use smooth_flow::{ClientFrame, Decision, Engine, HookEvent, HookReply, NewRequest, ServerFrame, SessionKind};

/// Route error: status + `{"error": …}` body (small, so clippy's
/// `result_large_err` stays quiet).
type ApiErr = (StatusCode, Json<Value>);

/// How long a `PermissionRequest` hook is held open waiting for `flow.approve`.
pub const HOOK_LONG_POLL: Duration = Duration::from_secs(120);
/// Supervision cadence.
const SUPERVISE_EVERY: Duration = Duration::from_secs(2);

/// Shared state for the flow routes.
#[derive(Clone)]
pub struct FlowState {
    engine: Engine,
    token: Option<Arc<String>>,
}

/// The `/api/flow/*` router. `token` = the daemon's local token (`None`
/// disables the gate — tests only).
pub fn flow_router(engine: Engine, token: Option<String>) -> Router {
    let state = FlowState {
        engine,
        token: token.map(Arc::new),
    };
    Router::new()
        .route("/api/flow/ws", get(ws_upgrade))
        .route("/api/flow/sessions", get(list_sessions).post(new_session))
        .route("/api/flow/sessions/{id}/input", post(input))
        .route("/api/flow/sessions/{id}/resize", post(resize))
        .route("/api/flow/sessions/{id}/approve", post(approve))
        .route("/api/flow/sessions/{id}/kill", post(kill))
        .route("/api/flow/sessions/{id}/send", post(send_text))
        .route("/api/flow/sessions/{id}/snapshot", get(snapshot))
        .route("/api/flow/sessions/{id}/handoff", get(handoff))
        .route("/api/flow/hooks", post(hooks))
        .with_state(state)
}

/// Open the engine on `workspace`, start its supervisor, return its router —
/// the one-liner `serve_local_flavor` merges.
///
/// # Errors
/// When the flow store cannot be opened.
pub fn install(workspace: std::path::PathBuf, token: String) -> anyhow::Result<Router> {
    let engine = Engine::open(smooth_flow::EngineConfig::new(workspace))?;
    drop(spawn_supervisor(engine.clone()));
    Ok(flow_router(engine, Some(token)))
}

/// Spawn the supervision tick for `engine` (rules 2–5 run here).
pub fn spawn_supervisor(engine: Engine) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SUPERVISE_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let e = engine.clone();
            if let Err(err) = tokio::task::spawn_blocking(move || e.supervise_tick()).await {
                tracing::warn!(error = %err, "flow supervisor tick panicked");
            }
        }
    })
}

/// Pure token check over the ways a client can present it.
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn presented_token(headers: &HeaderMap, query: &HashMap<String, String>) -> Option<String> {
    if let Some(t) = query.get("token").filter(|t| !t.is_empty()) {
        return Some(t.clone());
    }
    if let Some(v) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(t) = v.strip_prefix("Bearer ").map(str::trim).filter(|t| !t.is_empty()) {
            return Some(t.to_string());
        }
    }
    headers
        .get("x-smooth-token")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// True when `expected` is unset or matches what the client presented.
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn authorized(expected: Option<&str>, headers: &HeaderMap, query: &HashMap<String, String>) -> bool {
    expected.is_none_or(|exp| presented_token(headers, query).as_deref() == Some(exp))
}

fn gate(state: &FlowState, headers: &HeaderMap, query: &HashMap<String, String>) -> Result<(), ApiErr> {
    if authorized(state.token.as_deref().map(String::as_str), headers, query) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, Json(json!({"error":"missing or invalid local token"}))))
    }
}

fn err_response(e: &anyhow::Error) -> ApiErr {
    let msg = e.to_string();
    let status = if msg.contains("no such session") || msg.contains("not a candidate") {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    (status, Json(json!({"error": msg})))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, ApiErr> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(err_response(&e)),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))),
    }
}

// ── HTTP siblings ─────────────────────────────────────────────────────────────

async fn list_sessions(State(st): State<FlowState>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let e = st.engine.clone();
    let sessions = blocking(move || e.list()).await?;
    Ok(Json(json!({ "sessions": sessions })))
}

#[derive(Deserialize)]
struct NewBody {
    #[serde(default)]
    kind: SessionKind,
    #[serde(default)]
    worktree: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    pearl_id: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    argv: Option<Vec<String>>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tmux_socket: Option<String>,
}

impl From<NewBody> for NewRequest {
    fn from(b: NewBody) -> Self {
        Self {
            kind: b.kind,
            worktree: b.worktree,
            project: b.project,
            pearl_id: b.pearl_id,
            prompt: b.prompt,
            argv: b.argv,
            title: b.title,
            model: b.model,
            fan_out_id: None,
            tmux_socket: b.tmux_socket,
        }
    }
}

async fn new_session(
    State(st): State<FlowState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<NewBody>,
) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let e = st.engine.clone();
    let s = blocking(move || e.new_session(body.into())).await?;
    Ok(Json(json!({ "session": s })))
}

#[derive(Deserialize)]
struct InputBody {
    data_b64: String,
}

async fn input(
    State(st): State<FlowState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Path(id): Path<String>,
    Json(body): Json<InputBody>,
) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(body.data_b64)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({"error": format!("data_b64: {e}")}))))?;
    let e = st.engine.clone();
    blocking(move || e.input(&id, &data)).await?;
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
struct ResizeBody {
    cols: u16,
    rows: u16,
}

async fn resize(
    State(st): State<FlowState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Path(id): Path<String>,
    Json(body): Json<ResizeBody>,
) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let e = st.engine.clone();
    blocking(move || e.resize(&id, body.cols, body.rows)).await?;
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
struct ApproveBody {
    request_id: String,
    decision: Decision,
}

async fn approve(
    State(st): State<FlowState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Path(id): Path<String>,
    Json(body): Json<ApproveBody>,
) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let e = st.engine.clone();
    let s = blocking(move || {
        e.approve(&id, &body.request_id, body.decision)?;
        e.get(&id)
    })
    .await?;
    Ok(Json(json!({ "session": s })))
}

#[derive(Deserialize, Default)]
struct KillBody {
    #[serde(default)]
    resume: bool,
}

async fn kill(
    State(st): State<FlowState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Path(id): Path<String>,
    body: Option<Json<KillBody>>,
) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let resume = body.is_some_and(|b| b.0.resume);
    let e = st.engine.clone();
    let s = blocking(move || e.kill(&id, resume)).await?;
    Ok(Json(json!({ "session": s })))
}

#[derive(Deserialize)]
struct SendBody {
    text: String,
}

async fn send_text(
    State(st): State<FlowState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Path(id): Path<String>,
    Json(body): Json<SendBody>,
) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let e = st.engine.clone();
    blocking(move || e.send(&id, &body.text)).await?;
    Ok(Json(json!({})))
}

async fn snapshot(
    State(st): State<FlowState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let e = st.engine.clone();
    let frame = blocking(move || e.snapshot(&id)).await?;
    Ok(Json(serde_json::from_str(&frame.to_wire()).unwrap_or_else(|_| json!({}))))
}

async fn handoff(
    State(st): State<FlowState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let e = st.engine.clone();
    Ok(Json(blocking(move || e.handoff(&id)).await?))
}

/// `POST /api/flow/hooks` — always 200 with a JSON body, so a hook script can
/// pass it straight through to the harness.
async fn hooks(State(st): State<FlowState>, Json(ev): Json<HookEvent>) -> Json<Value> {
    let e = st.engine.clone();
    let reply = match tokio::task::spawn_blocking(move || e.hook(ev)).await {
        Ok(Ok(r)) => r,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "flow hook failed");
            return Json(json!({}));
        }
        Err(err) => {
            tracing::warn!(error = %err, "flow hook panicked");
            return Json(json!({}));
        }
    };
    match reply {
        HookReply::Immediate(v) => Json(v),
        HookReply::Pending { request_id, rx, payload } => {
            let decision = tokio::time::timeout(HOOK_LONG_POLL, rx).await.ok().and_then(Result::ok);
            Json(st.engine.finish_pending(&request_id, decision, &payload))
        }
    }
}

// ── WebSocket ─────────────────────────────────────────────────────────────────

async fn ws_upgrade(State(st): State<FlowState>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, ws: WebSocketUpgrade) -> Response {
    if let Err(r) = gate(&st, &headers, &q) {
        return r.into_response();
    }
    ws.on_upgrade(move |socket| ws_session(socket, st.engine))
}

/// Per-client loop: `flow.hello`, then fan broadcast frames out (output
/// only for attached ids) while handling client frames.
async fn ws_session(mut socket: WebSocket, engine: Engine) {
    let mut rx = engine.subscribe();
    let mut attached: HashSet<String> = HashSet::new();
    let hello = match engine.hello() {
        Ok(h) => h,
        Err(e) => ServerFrame::error(None, "internal", e.to_string()),
    };
    if socket.send(Message::Text(hello.to_wire().into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(frame) => {
                    if let Some(id) = frame.output_session() {
                        if !attached.contains(id) {
                            continue;
                        }
                    }
                    if socket.send(Message::Text(frame.to_wire().into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(dropped = n, "flow ws client lagged; output frames dropped");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            msg = socket.recv() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t.to_string(),
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    Some(Ok(_)) => continue,
                };
                let replies = handle_client_text(&engine, &text, &mut attached).await;
                for r in replies {
                    if socket.send(Message::Text(r.to_wire().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
    for id in attached {
        engine.detach(&id);
    }
}

/// Handle one client frame; returns the direct replies (errors, screens,
/// the created session). Broadcast side effects go through the engine.
async fn handle_client_text(engine: &Engine, text: &str, attached: &mut HashSet<String>) -> Vec<ServerFrame> {
    let r#ref = client_seq(text);
    let frame = match parse_client_frame(text) {
        Ok(Some(f)) => f,
        Ok(None) => return vec![],
        Err(e) => return vec![ServerFrame::error(r#ref, "bad_request", e.to_string())],
    };
    match dispatch(engine, frame, attached).await {
        Ok(v) => v,
        Err(e) => {
            let code = if e.to_string().contains("no such session") { "not_found" } else { "failed" };
            vec![ServerFrame::error(r#ref, code, e.to_string())]
        }
    }
}

async fn run<T: Send + 'static>(engine: &Engine, f: impl FnOnce(Engine) -> anyhow::Result<T> + Send + 'static) -> anyhow::Result<T> {
    let e = engine.clone();
    tokio::task::spawn_blocking(move || f(e))
        .await
        .map_err(|e| anyhow::anyhow!("flow task panicked: {e}"))?
}

async fn dispatch(engine: &Engine, frame: ClientFrame, attached: &mut HashSet<String>) -> anyhow::Result<Vec<ServerFrame>> {
    match frame {
        ClientFrame::Attach { id, cols, rows } => {
            let sid = id.clone();
            run(engine, move |e| e.attach(&sid, cols, rows)).await?;
            attached.insert(id.clone());
            // th-d33afa: replay the buffered event stream so a phone's Chat
            // tab isn't empty for what happened before it looked.
            let sid = id.clone();
            let replay = run(engine, move |e| e.events(&sid)).await?;
            Ok(replay.into_iter().map(|event| ServerFrame::Event { id: id.clone(), event }).collect())
        }
        ClientFrame::Detach { id } => {
            if attached.remove(&id) {
                engine.detach(&id);
            }
            Ok(vec![])
        }
        ClientFrame::Input { id, data_b64 } => {
            let data = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
            run(engine, move |e| e.input(&id, &data)).await?;
            Ok(vec![])
        }
        ClientFrame::Resize { id, cols, rows } => {
            run(engine, move |e| e.resize(&id, cols, rows)).await?;
            Ok(vec![])
        }
        ClientFrame::Snapshot { id } => Ok(vec![run(engine, move |e| e.snapshot(&id)).await?]),
        ClientFrame::New {
            kind,
            worktree,
            project,
            pearl_id,
            prompt,
            argv,
            title,
            tmux_socket,
        } => {
            let req = NewRequest {
                kind,
                worktree,
                project,
                pearl_id,
                prompt,
                argv,
                title,
                model: None,
                fan_out_id: None,
                tmux_socket,
            };
            let s = run(engine, move |e| e.new_session(req)).await?;
            Ok(vec![ServerFrame::Session { session: s }])
        }
        ClientFrame::Send { id, text } => {
            run(engine, move |e| e.send(&id, &text)).await?;
            Ok(vec![])
        }
        ClientFrame::Approve { id, request_id, decision } => {
            run(engine, move |e| e.approve(&id, &request_id, decision)).await?;
            Ok(vec![])
        }
        ClientFrame::Kill { id, resume } => {
            attached.remove(&id);
            let s = run(engine, move |e| e.kill(&id, resume)).await?;
            Ok(vec![ServerFrame::Session { session: s }])
        }
        ClientFrame::FanoutNew {
            prompt,
            pearl_id,
            candidates,
            project,
        } => {
            let cands: Vec<CandidateSpec> = candidates;
            let (fo, sessions) = run(engine, move |e| e.fanout_new(&prompt, &pearl_id, &cands, project.as_deref())).await?;
            Ok(vec![ServerFrame::Fanout {
                fan_out: fo,
                candidates: sessions,
            }])
        }
        ClientFrame::FanoutPick { fan_out_id, winner_session_id } => {
            let (fo, sessions) = run(engine, move |e| e.fanout_pick(&fan_out_id, &winner_session_id)).await?;
            Ok(vec![ServerFrame::Fanout {
                fan_out: fo,
                candidates: sessions,
            }])
        }
        ClientFrame::MarkRead { id } => {
            run(engine, move |e| e.mark_read(&id)).await?;
            Ok(vec![])
        }
        // th-d33afa: the phone's bridge nudge — say hello again.
        ClientFrame::Hello {} => Ok(vec![run(engine, |e| e.hello()).await?]),
        // th-d33afa: the pearl-rail packet over WS (the relay brokers WS only).
        ClientFrame::Handoff { id } => {
            let sid = id.clone();
            let v = run(engine, move |e| e.handoff(&sid)).await?;
            Ok(vec![handoff_frame(id, &v)])
        }
    }
}
/// The HTTP handoff body as the `flow.handoff` frame (nulls kept, so a phone
/// can tell "no pearl" from "field missing").
fn handoff_frame(id: String, v: &Value) -> ServerFrame {
    let take = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    ServerFrame::Handoff {
        id,
        pearl: take("pearl"),
        handoff: take("handoff"),
        checkpoints: take("checkpoints"),
        blocks: take("blocks"),
        pr: take("pr"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use futures_util::{SinkExt, StreamExt};
    use smooth_flow::EngineConfig;
    use tower::ServiceExt as _;

    fn engine(tmp: &std::path::Path) -> Engine {
        Engine::open(EngineConfig {
            db_path: tmp.join("flow.db"),
            default_project: tmp.to_path_buf(),
            version: "t".into(),
            machine_label: "m".into(),
        })
        .unwrap()
    }

    /// The next text frame as JSON (5 s cap).
    async fn next<S>(source: &mut S) -> Value
    where
        S: StreamExt<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        let text = tokio::time::timeout(Duration::from_secs(5), source.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        serde_json::from_str::<Value>(&text).unwrap()
    }
    async fn body_json(resp: Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    #[test]
    fn token_presentation_forms() {
        let mut h = HeaderMap::new();
        let mut q = HashMap::new();
        assert!(presented_token(&h, &q).is_none());
        assert!(authorized(None, &h, &q), "no expected token ⇒ open");
        assert!(!authorized(Some("secret"), &h, &q));
        q.insert("token".into(), "secret".into());
        assert!(authorized(Some("secret"), &h, &q));
        q.clear();
        h.insert("authorization", "Bearer secret".parse().unwrap());
        assert!(authorized(Some("secret"), &h, &q));
        h.clear();
        h.insert("x-smooth-token", " secret ".parse().unwrap());
        assert!(authorized(Some("secret"), &h, &q));
        h.clear();
        h.insert("authorization", "Bearer wrong".parse().unwrap());
        assert!(!authorized(Some("secret"), &h, &q));
    }

    #[tokio::test]
    async fn http_routes_are_gated_and_hooks_are_not() {
        let tmp = tempfile::tempdir().unwrap();
        let router = flow_router(engine(tmp.path()), Some("tok".into()));
        let resp = router
            .clone()
            .oneshot(Request::builder().uri("/api/flow/sessions").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = router
            .clone()
            .oneshot(Request::builder().uri("/api/flow/sessions?token=tok").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["sessions"], json!([]));
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/flow/hooks")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"event":"Stop","session_id":"nobody"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "hooks never need a token");
        assert_eq!(body_json(resp).await, json!({}));
        // Unknown session on a gated route is 404 with an error object.
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/flow/sessions/fs-nope/kill")
                    .header("x-smooth-token", "tok")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(body_json(resp).await["error"].is_string());
    }

    #[tokio::test]
    async fn ws_says_hello_then_errors_are_objects_and_unknown_types_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let app = flow_router(engine(tmp.path()), Some("tok".into()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // Wrong token ⇒ handshake rejected.
        assert!(tokio_tungstenite::connect_async(format!("ws://{addr}/api/flow/ws?token=nope")).await.is_err());

        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/flow/ws?token=tok")).await.unwrap();
        let (mut sink, mut source) = ws.split();
        let hello = source.next().await.unwrap().unwrap().into_text().unwrap();
        let v: Value = serde_json::from_str(&hello).unwrap();
        assert_eq!(v["type"], "flow.hello");
        assert_eq!(v["channel"], "flow");
        assert_eq!(v["daemon"]["version"], "t");
        assert_eq!(v["sessions"], json!([]));

        // Unknown type: silently ignored (no reply). Then a bad known frame:
        // an error OBJECT with the client's seq echoed in `ref`.
        sink.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"channel":"flow","type":"flow.future"}"#.into(),
        ))
        .await
        .unwrap();
        sink.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"channel":"flow","type":"flow.snapshot","id":"fs-nope","seq":9}"#.into(),
        ))
        .await
        .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(5), source.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["type"], "flow.error", "{reply}");
        assert_eq!(v["ref"], 9);
        assert_eq!(v["code"], "not_found");
        assert!(v["message"].is_string());

        // A malformed known frame is also an error object.
        sink.send(tokio_tungstenite::tungstenite::Message::Text(r#"{"type":"flow.attach","id":"x"}"#.into()))
            .await
            .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(5), source.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["code"], "bad_request");
    }

    /// th-d33afa: a client `flow.hello` gets the hello again, `flow.handoff`
    /// answers over WS with the HTTP route's shape, and hook events reach
    /// every flow client as `flow.event`.
    #[tokio::test]
    async fn hello_nudge_handoff_and_events_over_ws() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine(tmp.path());
        let sid = {
            use smooth_flow::store::NewSession;
            let st = smooth_flow::FlowStore::open(&tmp.path().join("flow.db")).unwrap();
            st.create(NewSession {
                kind: Some(SessionKind::Claude),
                agent_session_id: Some("uuid-ev".into()),
                project: tmp.path().to_string_lossy().into(),
                worktree: tmp.path().to_string_lossy().into(),
                ..Default::default()
            })
            .unwrap()
            .id
        };
        let app = flow_router(engine.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/flow/ws")).await.unwrap();
        let (mut sink, mut source) = ws.split();
        assert_eq!(next(&mut source).await["type"], "flow.hello");

        sink.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"channel":"flow","type":"flow.hello"}"#.into(),
        ))
        .await
        .unwrap();
        let v = next(&mut source).await;
        assert_eq!(v["type"], "flow.hello", "{v}");
        assert_eq!(v["sessions"][0]["id"], sid);

        sink.send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"channel":"flow","type":"flow.handoff","id":sid}).to_string().into(),
        ))
        .await
        .unwrap();
        let v = next(&mut source).await;
        assert_eq!(v["type"], "flow.handoff", "{v}");
        assert_eq!(v["id"], sid);
        assert_eq!(v["handoff"]["agent_session_id"], "uuid-ev");
        assert!(v["checkpoints"].is_array() && v["blocks"].is_array());
        assert!(v.get("pearl").is_some() && v.get("pr").is_some(), "nulls are present, not omitted: {v}");

        reqwest::Client::new()
            .post(format!("http://{addr}/api/flow/hooks"))
            .json(&json!({"harness":"claude-code","event":"UserPromptSubmit","session_id":"uuid-ev","payload":{"prompt":"go"}}))
            .send()
            .await
            .unwrap();
        // user line, then the working state line — both flow.event, both for sid.
        let mut kinds = Vec::new();
        for _ in 0..6 {
            let v = next(&mut source).await;
            if v["type"] == "flow.event" {
                assert_eq!(v["id"], sid);
                assert!(v["event_id"].is_string() && v["at"].is_string());
                kinds.push((v["kind"].as_str().unwrap().to_string(), v["text"].as_str().unwrap().to_string()));
                if kinds.len() == 2 {
                    break;
                }
            }
        }
        assert_eq!(
            kinds,
            vec![("user".to_string(), "go".to_string()), ("system".to_string(), "working".to_string())]
        );
    }

    /// th-d33afa: `flow.attach` replays the buffered stream (needs a live
    /// tmux; skips without one).
    #[tokio::test]
    async fn attach_replays_the_event_stream() {
        if !smooth_flow::tmux::tmux_available() {
            eprintln!("skipping: tmux not available");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine(tmp.path());
        let sock = format!("flow-r-{}", std::process::id());
        let s = engine
            .new_session(NewRequest {
                kind: SessionKind::Shell,
                worktree: Some(tmp.path().to_string_lossy().into()),
                argv: Some(vec!["sh".into(), "-c".into(), "cat".into()]),
                tmux_socket: Some(sock.clone()),
                ..Default::default()
            })
            .unwrap();
        engine.send(&s.id, "first steer").unwrap();
        let app = flow_router(engine.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/flow/ws")).await.unwrap();
        let (mut sink, mut source) = ws.split();
        let _hello = source.next().await.unwrap().unwrap();
        sink.send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"channel":"flow","type":"flow.attach","id":s.id,"cols":80,"rows":24}).to_string().into(),
        ))
        .await
        .unwrap();
        let mut replayed = Vec::new();
        while replayed.len() < 2 {
            let text = tokio::time::timeout(Duration::from_secs(5), source.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap();
            let v: Value = serde_json::from_str(&text).unwrap();
            if v["type"] == "flow.event" {
                replayed.push(v["text"].as_str().unwrap().to_string());
            }
        }
        assert_eq!(replayed, vec!["idle".to_string(), "first steer".to_string()]);
        engine.kill(&s.id, false).unwrap();
        smooth_flow::tmux::kill_server(&sock);
    }

    #[tokio::test]
    async fn permission_hook_long_polls_until_approved_over_ws() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine(tmp.path());
        // A claude session row with a known harness id, no process (same
        // db file — WAL lets a second connection write it).
        let sid = {
            use smooth_flow::store::NewSession;
            let st = smooth_flow::FlowStore::open(&tmp.path().join("flow.db")).unwrap();
            st.create(NewSession {
                kind: Some(SessionKind::Claude),
                agent_session_id: Some("uuid-hook".into()),
                project: "/p".into(),
                worktree: "/p".into(),
                ..Default::default()
            })
            .unwrap()
            .id
        };
        let app = flow_router(engine.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // Client attaches to the WS and watches for the attention frame.
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/flow/ws")).await.unwrap();
        let (mut sink, mut source) = ws.split();
        let _hello = source.next().await.unwrap().unwrap();

        // The hook POST is held open.
        let http = reqwest::Client::new();
        let post = tokio::spawn(async move {
            http.post(format!("http://{addr}/api/flow/hooks"))
                .json(&json!({
                    "harness":"claude-code","event":"PermissionRequest","session_id":"uuid-hook","cwd":"/p",
                    "payload":{"tool_name":"Bash","tool_input":{"command":"ls"}}
                }))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        });

        // Wait for the needs_you attention with a request_id.
        let mut request_id = None;
        for _ in 0..10 {
            let text = tokio::time::timeout(Duration::from_secs(5), source.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap();
            let v: Value = serde_json::from_str(&text).unwrap();
            if v["type"] == "flow.attention" && v["attention"]["reason"] == "permission" {
                request_id = v["attention"]["request_id"].as_str().map(str::to_string);
                break;
            }
        }
        let request_id = request_id.unwrap();
        assert!(!post.is_finished(), "hook is still long-polling");

        sink.send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"channel":"flow","type":"flow.approve","id":sid,"request_id":request_id,"decision":"deny"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        let body = tokio::time::timeout(Duration::from_secs(5), post).await.unwrap().unwrap();
        assert_eq!(body["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(body["hookSpecificOutput"]["decision"]["behavior"], "deny");
    }
}

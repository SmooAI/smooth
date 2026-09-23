//! `/api/relay/peers*` — the Big Smooth window's computer switcher
//! (pearl th-a49e21).
//!
//! - `GET /api/relay/peers` → `{self: {device, label}, relay: {state, detail,
//!   since}, peers: [{device, label, kind}], error}` — this computer, the relay
//!   link's state, and every OTHER Big Smooth daemon on the signed-in user's
//!   Smoo Relay. Always 200 once the token checks out, so the switcher can say
//!   WHY a computer is unreachable (`relay.state` / `error`) instead of just
//!   showing nothing.
//! - `GET /api/relay/peers/{device}/ws` → the canonical operator WebSocket of
//!   that computer's Big Smooth, tunnelled over the relay
//!   ([`crate::relay_tunnel::Dialer`]). The window points its `/ws` here.
//! - `GET|POST /api/relay/peers/{device}/{*path}` → that computer's REST route
//!   `/{path}` over the relay ([`crate::relay_http`]'s allowlist applies).
//!
//! So a window drives a remote computer by using
//! `/api/relay/peers/<device>` as its API base — nothing else in the SPA
//! changes. Every route is gated by THIS daemon's local token, like the rest of
//! the daemon's API; the token is stripped before anything is forwarded.
//!
//! **Who can be driven.** Only a device the relay lists right now as one of
//! the signed-in user's `daemon` peers — never this computer itself, never a
//! phone or a tunnel, never a malformed id. The relay itself only ever routes
//! between devices of the SAME Smoo user; this check means a window can't even
//! aim at anything else.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::flow_route::authorized;
use crate::relay::{valid_device, DirectoryError, RelayDirectory, RelayPeer};
use crate::relay_tunnel::{Dialer, HttpLinks, Tunnel, TunnelEvent};

type ApiErr = (StatusCode, Json<Value>);

/// How fresh a cached peer list must be to authorise a tunnel. A window
/// reconnects every ~1.5s while a computer is down; this keeps that from
/// becoming a `list_peers` per retry.
const PEER_CACHE: Duration = Duration::from_secs(10);
/// WebSocket close code for "that computer is not reachable right now".
const CLOSE_UNREACHABLE: u16 = 4404;

/// Shared state for the peers routes.
#[derive(Clone)]
pub struct PeersRouteState {
    directory: RelayDirectory,
    self_device: Arc<String>,
    self_label: Arc<String>,
    /// `None` when the relay is disabled on this daemon.
    dialer: Option<Dialer>,
    http: Option<HttpLinks>,
    token: Option<Arc<String>>,
}

impl PeersRouteState {
    /// `dialer = None` ⇔ the relay is off here. `token = None` disables the
    /// gate (tests only).
    #[must_use]
    pub fn new(directory: RelayDirectory, self_device: &str, self_label: &str, dialer: Option<Dialer>, token: Option<String>) -> Self {
        Self {
            directory,
            self_device: Arc::new(self_device.to_string()),
            self_label: Arc::new(self_label.to_string()),
            http: dialer.clone().map(HttpLinks::new),
            dialer,
            token: token.map(Arc::new),
        }
    }
}

/// The router.
pub fn peers_router(state: PeersRouteState) -> Router {
    Router::new()
        .route("/api/relay/peers", get(list))
        .route("/api/relay/peers/{device}/ws", get(ws_upgrade))
        .route("/api/relay/peers/{device}/{*path}", get(proxy).post(proxy))
        .with_state(state)
}

fn api_err(status: StatusCode, message: impl Into<String>) -> ApiErr {
    (status, Json(json!({ "error": message.into() })))
}

fn gate(state: &PeersRouteState, headers: &HeaderMap, query: &HashMap<String, String>) -> Result<(), ApiErr> {
    if authorized(state.token.as_deref().map(String::as_str), headers, query) {
        Ok(())
    } else {
        Err(api_err(StatusCode::UNAUTHORIZED, "missing or invalid local token"))
    }
}

/// Only this user's online Big Smooth daemons, minus this computer, sorted by
/// name — what the switcher offers.
fn drivable(peers: Vec<RelayPeer>, self_device: &str) -> Vec<RelayPeer> {
    let mut out: Vec<RelayPeer> = peers.into_iter().filter(|p| p.is_daemon() && p.device != self_device).collect();
    out.sort_by(|a, b| (a.label.to_lowercase(), &a.device).cmp(&(b.label.to_lowercase(), &b.device)));
    out
}

/// The human reason no peer list could be had.
fn directory_error(st: &PeersRouteState, e: &DirectoryError) -> String {
    match e {
        DirectoryError::NotOnline => st.directory.status().get().detail,
        DirectoryError::Timeout => "The Smoo Relay did not answer in time; try again.".to_string(),
    }
}

async fn list(State(st): State<PeersRouteState>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let (peers, error) = if st.dialer.is_none() {
        (Vec::new(), Some(st.directory.status().get().detail))
    } else {
        match st.directory.peers().await {
            Ok(p) => (drivable(p, &st.self_device), None),
            Err(e) => (Vec::new(), Some(directory_error(&st, &e))),
        }
    };
    Ok(Json(json!({
        "self": { "device": st.self_device.as_str(), "label": st.self_label.as_str() },
        "relay": st.directory.status().get(),
        "peers": peers,
        "error": error,
    })))
}

/// Check `device` is something this window may drive, returning its peer row.
async fn resolve_target(st: &PeersRouteState, device: &str) -> Result<RelayPeer, ApiErr> {
    if !valid_device(device) {
        return Err(api_err(StatusCode::BAD_REQUEST, "not a relay device id"));
    }
    if device == st.self_device.as_str() {
        return Err(api_err(StatusCode::BAD_REQUEST, "that is this computer — use the local API"));
    }
    if st.dialer.is_none() {
        return Err(api_err(StatusCode::SERVICE_UNAVAILABLE, st.directory.status().get().detail));
    }
    let peers = st.directory.peers_cached(PEER_CACHE).await.map_err(|e| {
        let status = if e == DirectoryError::Timeout {
            StatusCode::GATEWAY_TIMEOUT
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };
        api_err(status, directory_error(st, &e))
    })?;
    drivable(peers, &st.self_device)
        .into_iter()
        .find(|p| p.device == device)
        .ok_or_else(|| api_err(StatusCode::NOT_FOUND, "that computer is offline, or is not one of your Big Smooth computers"))
}

async fn ws_upgrade(
    State(st): State<PeersRouteState>,
    Path(device): Path<String>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    // The token first: an unauthenticated caller learns nothing, not even
    // whether it spoke the WebSocket handshake right.
    if let Err(e) = gate(&st, &headers, &q) {
        return e.into_response();
    }
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };
    let peer = match resolve_target(&st, &device).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let Some(dialer) = st.dialer.as_ref() else {
        return api_err(StatusCode::SERVICE_UNAVAILABLE, "the relay is off on this computer").into_response();
    };
    // Dial BEFORE upgrading, so a relay failure is an HTTP error the window
    // sees as a failed connect (and retries), not an open socket that says nothing.
    let tunnel = match dialer.open(&peer.device).await {
        Ok(t) => t,
        Err(e) => return api_err(StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    tracing::info!(target = %peer.device, label = %peer.label, via = %tunnel.device, "relay: a window is driving another computer's Big Smooth");
    ws.on_upgrade(move |socket| pump(socket, tunnel))
}

/// Window ⇄ tunnel until either side ends. Frames pass through unparsed
/// (the canonical protocol is the operator's business), except that non-JSON
/// from the window is dropped.
async fn pump(mut socket: WebSocket, mut tunnel: Tunnel) {
    let reason = loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if serde_json::from_str::<Value>(&text).is_ok() && !tunnel.send(text.to_string()) {
                        break Some("the relay connection dropped".to_string());
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break None,
                Some(Ok(_)) => {}
            },
            ev = tunnel.events.recv() => match ev {
                Some(TunnelEvent::Frame(text)) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break None;
                    }
                }
                Some(TunnelEvent::Ended(why)) => break Some(why),
                None => break Some("the relay connection dropped".to_string()),
            },
        }
    };
    if let Some(why) = reason {
        // Close reasons are capped at 123 bytes by the protocol.
        let why: String = why.chars().take(100).collect();
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: CLOSE_UNREACHABLE,
                reason: why.into(),
            })))
            .await;
    }
}

async fn proxy(
    State(st): State<PeersRouteState>,
    Path((device, path)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, ApiErr> {
    gate(&st, &headers, &q)?;
    let method = method.as_str().to_string();
    let path_and_query = match crate::relay_http::strip_token(uri.query()) {
        Some(query) => format!("/{path}?{query}"),
        None => format!("/{path}"),
    };
    // Checked here too, so a refused route never costs a relay round trip.
    if !crate::relay_http::allowed(&method, &path_and_query) {
        return Err(api_err(StatusCode::FORBIDDEN, "that route is not available over the relay"));
    }
    if body.len() > crate::relay_http::MAX_BODY_BYTES {
        return Err(api_err(StatusCode::PAYLOAD_TOO_LARGE, "request body too large for the relay"));
    }
    let body = if body.is_empty() {
        None
    } else {
        Some(String::from_utf8(body.to_vec()).map_err(|_| api_err(StatusCode::BAD_REQUEST, "request body must be UTF-8"))?)
    };
    let peer = resolve_target(&st, &device).await?;
    let Some(links) = st.http.as_ref() else {
        return Err(api_err(StatusCode::SERVICE_UNAVAILABLE, "the relay is off on this computer"));
    };
    let resp = links.request(&peer.device, &method, &path_and_query, body.as_deref()).await.map_err(|e| {
        let status = if e == crate::relay_tunnel::LinkError::Timeout {
            StatusCode::GATEWAY_TIMEOUT
        } else {
            StatusCode::BAD_GATEWAY
        };
        api_err(status, e.to_string())
    })?;
    let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::BAD_GATEWAY);
    Ok((status, [(axum::http::header::CONTENT_TYPE, resp.content_type)], resp.body).into_response())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use futures_util::{SinkExt, StreamExt};
    use http_body_util::BodyExt;
    use tokio_tungstenite::tungstenite::Message as TMsg;
    use tower::ServiceExt;

    use super::*;
    use crate::relay_status::{RelayPhase, RelayStatusHandle};
    use crate::relay_tunnel::tests::{token, FakeRelay};

    /// A directory whose "relay socket" answers every question with `peers`.
    fn answering_directory(state: RelayPhase, peers: Vec<RelayPeer>) -> RelayDirectory {
        let (dir, mut rx) = RelayDirectory::new(RelayStatusHandle::new(state, "The relay is down for a test."));
        tokio::spawn(async move {
            while let Some(w) = rx.recv().await {
                let _ = w.send(peers.clone());
            }
        });
        dir
    }

    fn peer(device: &str, label: &str, kind: &str) -> RelayPeer {
        RelayPeer {
            device: device.into(),
            label: label.into(),
            kind: kind.into(),
        }
    }

    fn fleet() -> Vec<RelayPeer> {
        vec![
            peer("daemon-hub", "smoo-hub", "daemon"),
            peer("daemon-hub-flow", "smoo-hub · SmoothFlow", "flow"),
            peer("phone-1", "Brent's iPhone", "phone"),
            peer("daemon-local-w0", "marvin (window)", "phone"),
            peer("daemon-attic", "Attic", "daemon"),
        ]
    }

    fn state(dir: RelayDirectory, dialer: Option<Dialer>) -> PeersRouteState {
        PeersRouteState::new(dir, "daemon-local", "marvin", dialer, Some("local-secret".into()))
    }

    async fn get_json(app: Router, uri: &str) -> (StatusCode, Value) {
        let resp = app
            .oneshot(Request::get(uri).header("authorization", "Bearer local-secret").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn every_route_needs_the_local_token() {
        let app = peers_router(state(answering_directory(RelayPhase::Online, fleet()), None));
        for uri in ["/api/relay/peers", "/api/relay/peers/daemon-hub/api/stats", "/api/relay/peers/daemon-hub/ws"] {
            let resp = app.clone().oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{uri}");
            let wrong = app
                .clone()
                .oneshot(Request::get(uri).header("authorization", "Bearer nope").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
    }

    #[tokio::test]
    async fn the_list_is_this_computer_plus_other_big_smooth_daemons_only() {
        let dialer = Dialer::new("ws://127.0.0.1:1/ws", "daemon-local", "marvin", token("u"));
        let app = peers_router(state(answering_directory(RelayPhase::Online, fleet()), Some(dialer)));
        let (status, v) = get_json(app, "/api/relay/peers").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["self"], json!({"device":"daemon-local","label":"marvin"}));
        assert_eq!(v["relay"]["state"], "online");
        assert_eq!(v["error"], Value::Null);
        let labels: Vec<&str> = v["peers"].as_array().unwrap().iter().map(|p| p["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["Attic", "smoo-hub"], "daemons only — no flow child, phone, or window tunnel");
    }

    #[tokio::test]
    async fn an_offline_relay_still_answers_with_the_reason() {
        let dialer = Dialer::new("ws://127.0.0.1:1/ws", "daemon-local", "marvin", token("u"));
        let app = peers_router(state(answering_directory(RelayPhase::SignedOut, fleet()), Some(dialer)));
        let (status, v) = get_json(app, "/api/relay/peers").await;
        assert_eq!(status, StatusCode::OK, "the switcher must still render this computer");
        assert_eq!(v["relay"]["state"], "signed_out");
        assert_eq!(v["peers"], json!([]));
        assert!(v["error"].as_str().unwrap().contains("down for a test"));
        assert_eq!(v["self"]["label"], "marvin");
    }

    #[tokio::test]
    async fn a_disabled_relay_lists_nobody_and_refuses_to_proxy() {
        let dir = answering_directory(RelayPhase::Disabled, fleet());
        let app = peers_router(state(dir, None));
        let (status, v) = get_json(app.clone(), "/api/relay/peers").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["peers"], json!([]));
        let (status, _) = get_json(app, "/api/relay/peers/daemon-hub/api/stats").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn only_a_listed_daemon_of_this_user_can_be_targeted() {
        let dialer = Dialer::new("ws://127.0.0.1:1/ws", "daemon-local", "marvin", token("u"));
        let app = peers_router(state(answering_directory(RelayPhase::Online, fleet()), Some(dialer)));
        for (device, want) in [
            ("daemon-local", StatusCode::BAD_REQUEST),  // this computer
            ("phone-1", StatusCode::NOT_FOUND),         // a phone is not a computer
            ("daemon-hub-flow", StatusCode::NOT_FOUND), // SmoothFlow's child
            ("daemon-local-w0", StatusCode::NOT_FOUND), // one of our own tunnels
            ("daemon-stranger", StatusCode::NOT_FOUND), // not on this account's relay
            ("bad:device", StatusCode::BAD_REQUEST),    // channel syntax
            ("a%20b", StatusCode::BAD_REQUEST),         // whitespace
            (&"x".repeat(65), StatusCode::BAD_REQUEST), // too long
            ("daemon-hub%0A", StatusCode::BAD_REQUEST), // control char
        ] {
            let (status, v) = get_json(app.clone(), &format!("/api/relay/peers/{device}/api/stats")).await;
            assert_eq!(status, want, "{device}: {v}");
            assert!(v["error"].is_string());
        }
    }

    #[tokio::test]
    async fn routes_off_the_allowlist_are_refused_before_any_relay_traffic() {
        let relay = FakeRelay::default();
        let connects = relay.connects.clone();
        let addr = relay.serve().await;
        let dialer = Dialer::new(format!("ws://{addr}/ws"), "daemon-local", "marvin", token("u1"));
        let app = peers_router(state(answering_directory(RelayPhase::Online, fleet()), Some(dialer)));
        for path in [
            "api/flow/sessions",
            "auth/login",
            "push/subscribe",
            "api/relay/peers",
            "api/stats/../flow/sessions",
            "ws2",
        ] {
            let (status, _) = get_json(app.clone(), &format!("/api/relay/peers/daemon-hub/{path}")).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        }
        assert!(connects.lock().unwrap().is_empty(), "nothing was dialled");
    }

    /// The whole road: a window's REST call and operator WebSocket reach a
    /// remote daemon's real relay code (`run_connection` → operator bridge /
    /// `relay_http::Server`) through the fake relay — and the window's local
    /// token never leaves this computer.
    #[tokio::test]
    async fn a_window_drives_a_remote_daemon_end_to_end() {
        use axum::extract::ws::WebSocketUpgrade as AxUpgrade;

        // The REMOTE computer: an operator WS that echoes, and a REST route
        // that reports what it was asked and with which token.
        let remote_app = Router::new()
            .route(
                "/ws",
                get(|u: AxUpgrade| async move {
                    u.on_upgrade(|mut ws: WebSocket| async move {
                        while let Some(Ok(Message::Text(t))) = ws.recv().await {
                            let v: Value = serde_json::from_str(&t).unwrap();
                            let reply = json!({"type":"echo","action": v["action"]});
                            if ws.send(Message::Text(reply.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    })
                }),
            )
            .route(
                "/api/session/cwd",
                get(|headers: HeaderMap, uri: Uri| async move {
                    Json(json!({
                        "auth": headers.get("authorization").and_then(|v| v.to_str().ok()),
                        "uri": uri.to_string(),
                        "cwd": "/Users/hub/project",
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote_addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, remote_app).await.unwrap() });

        let relay = FakeRelay::default();
        let relay_addr = relay.serve().await;

        // The remote daemon's real relay supervisor loop, on the fake relay.
        let remote_status = RelayStatusHandle::new(RelayPhase::Authenticating, "");
        let (_remote_dir, remote_peer_rx) = RelayDirectory::new(remote_status.clone());
        let remote_pairing = Arc::new(crate::flow_e2e::tests::state());
        let remote_http = crate::relay_http::Server::new(format!("http://{remote_addr}"), "hub-token");
        let remote_status_task = remote_status.clone();
        tokio::spawn(async move {
            let remote_status = remote_status_task;
            let mut peer_rx = remote_peer_rx;
            let (stream, _) = tokio_tungstenite::connect_async(format!("ws://{relay_addr}/ws?token=u1&device=daemon-hub&label=smoo-hub&kind=daemon"))
                .await
                .unwrap();
            let dialled = crate::relay_status::CredView::dialled(Some("u1".into()), "u1");
            let (_creds_tx, mut creds_rx) = tokio::sync::watch::channel(dialled.clone());
            let operator_url = format!("ws://{remote_addr}/ws?token=hub-token");
            let ctx = crate::relay::tests_support::ctx(&operator_url, &remote_pairing, &dialled, &remote_status, "daemon-hub", &remote_http);
            crate::relay::tests_support::run(stream, &ctx, &mut creds_rx, &mut peer_rx).await;
        });
        // Wait until the remote is registered.
        let mut watch = remote_status.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            while watch.borrow_and_update().state != RelayPhase::Online {
                watch.changed().await.unwrap();
            }
        })
        .await
        .unwrap();

        // THIS computer: the peers router, with a directory that lists the hub.
        let dialer = Dialer::new(format!("ws://{relay_addr}/ws"), "daemon-local", "marvin", token("u1"));
        let app = peers_router(state(
            answering_directory(RelayPhase::Online, vec![peer("daemon-hub", "smoo-hub", "daemon")]),
            Some(dialer),
        ));

        // REST: the remote answers with ITS token; ours is nowhere in the call.
        let (status, v) = get_json(app.clone(), "/api/relay/peers/daemon-hub/api/session/cwd?session=abc&token=local-secret").await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["cwd"], "/Users/hub/project");
        assert_eq!(v["auth"], "Bearer hub-token");
        assert_eq!(v["uri"], "/api/session/cwd?session=abc", "the local token is stripped before forwarding");

        // WebSocket: serve the router for real and talk through it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{local_addr}/api/relay/peers/daemon-hub/ws?token=local-secret"))
            .await
            .unwrap();
        ws.send(TMsg::Text(r#"{"action":"list_conversations","requestId":"lc-1"}"#.into()))
            .await
            .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap().unwrap().unwrap();
        let reply: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
        assert_eq!(reply, json!({"type":"echo","action":"list_conversations"}));

        // A computer that is not listed can't be dialled at all.
        let refused = tokio_tungstenite::connect_async(format!("ws://{local_addr}/api/relay/peers/daemon-stranger/ws?token=local-secret")).await;
        assert!(refused.is_err(), "the upgrade is refused");
    }
}

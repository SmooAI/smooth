//! Smoo Relay client (pearl th-2f626d, EPIC th-5561c5) — remote control
//! WITHOUT tailscale.
//!
//! The daemon dials OUT to the Smoo Relay (`rust/relay-ws` in the smooai repo,
//! SMOODEV-2828, `wss://relay.smoo.ai/ws`) and registers as the signed-in
//! user's device **`daemon`**. Phones connect to the same relay as their own
//! device ids and exchange `{to, frame}` envelopes; the relay forwards frames
//! between a user's devices — opaquely, same-user-only — so a phone anywhere
//! on the internet can chat with THIS daemon with no tailnet membership.
//!
//! Topology per phone device: one **loopback bridge** — a plain WS client onto
//! the daemon's own operator (`ws://127.0.0.1:<port>/ws?token=…`, the exact
//! seam the scheduler's `OperatorTurnDriver` uses) — so the operator sees each
//! phone as just another canonical-protocol client. Frames pass through
//! unparsed in both directions; the relay client only reads the envelope.
//!
//! Auth: the daemon's stored Smoo session (`th auth login`, kept fresh by the
//! credential heartbeat in [`crate::auth_login`]). The access token is re-read
//! from [`CredentialsStore`] on EVERY (re)connect — the heartbeat rotates it —
//! and a signed-out daemon simply waits and retries: the relay is a
//! reachability layer, never a reason the daemon can't boot.
//!
//! Config: `SMOOTH_RELAY=0` disables; `SMOOTH_RELAY_URL` overrides the default
//! relay endpoint (the env-knob precedent of `config.rs`).

use std::collections::HashMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use smooai_client_shared::auth::storage::CredentialsStore;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// The production relay endpoint (SMOODEV-2828).
const DEFAULT_RELAY_URL: &str = "wss://relay.smoo.ai/ws";
/// The device id THIS daemon registers under (v1: one daemon per user).
const DAEMON_DEVICE: &str = "daemon";
/// Reconnect backoff bounds. Exponential from `BACKOFF_MIN`, capped at
/// `BACKOFF_MAX`; reset after a connection that survived `BACKOFF_RESET_AFTER`.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(30);
/// How long a signed-out daemon waits before checking for credentials again.
const SIGNED_OUT_RECHECK: Duration = Duration::from_secs(60);

/// Resolve the relay endpoint from env. Pure (args, not env reads) so it's
/// hermetically testable: `enabled` = `SMOOTH_RELAY`, `url` = `SMOOTH_RELAY_URL`.
/// `None` ⇒ relay disabled.
fn resolve_relay_url_from(enabled: Option<&str>, url: Option<&str>) -> Option<String> {
    if matches!(enabled.map(str::trim), Some("0" | "false" | "off" | "no")) {
        return None;
    }
    Some(url.map_or(DEFAULT_RELAY_URL, str::trim).to_string()).filter(|u| !u.is_empty())
}

/// [`resolve_relay_url_from`] over the real environment.
pub fn resolve_relay_url() -> Option<String> {
    let enabled = std::env::var("SMOOTH_RELAY").ok();
    let url = std::env::var("SMOOTH_RELAY_URL").ok();
    resolve_relay_url_from(enabled.as_deref(), url.as_deref())
}

/// One inbound relay message, classified. Pure parse so the protocol rules are
/// unit-testable without sockets.
#[derive(Debug, PartialEq)]
enum RelayMsg {
    /// Relay heartbeat — answer with `{"type":"pong"}`.
    Ping,
    /// Registration ack / pong / other control noise — nothing to do.
    Ignore,
    /// The addressed peer (a phone we sent to) is connected nowhere — drop its bridge.
    PeerOffline(String),
    /// A relayed envelope: (sender device, the opaque frame as a wire string).
    Frame(String, String),
}

/// Classify one relay text frame.
fn classify_relay_msg(text: &str) -> RelayMsg {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return RelayMsg::Ignore;
    };
    match v.get("type").and_then(Value::as_str) {
        Some("ping") => return RelayMsg::Ping,
        Some("peer_offline") => {
            return v
                .get("to")
                .and_then(Value::as_str)
                .map_or(RelayMsg::Ignore, |d| RelayMsg::PeerOffline(d.to_string()));
        }
        Some("error") => {
            tracing::warn!(frame = %text, "relay: server error frame");
            return RelayMsg::Ignore;
        }
        Some(_) => return RelayMsg::Ignore, // connected / pong / future control frames
        None => {}
    }
    match (v.get("from").and_then(Value::as_str), v.get("frame")) {
        (Some(from), Some(frame)) => RelayMsg::Frame(from.to_string(), frame.to_string()),
        _ => RelayMsg::Ignore,
    }
}

/// Wrap an operator frame (raw text from the loopback WS) into a relay envelope
/// addressed to `to`. Non-JSON operator output is dropped (`None`) — the
/// canonical protocol is JSON-only, so anything else is line noise.
fn wrap_out(to: &str, operator_text: &str) -> Option<String> {
    let frame: Value = serde_json::from_str(operator_text).ok()?;
    Some(json!({ "to": to, "frame": frame }).to_string())
}

/// Percent-encode for a `?token=` query param (RFC 3986 unreserved passes).
/// Same as the scheduler's — tiny enough to duplicate over exporting.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A live loopback bridge for one phone device: frames from the phone go into
/// `to_operator`; a spawned task owns the loopback WS and pushes the operator's
/// replies back to the relay through the shared out-channel.
struct Bridge {
    to_operator: mpsc::UnboundedSender<String>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Spawn the loopback bridge task for `device`: connect to the daemon's own
/// operator WS, pump `rx` → operator and operator → `out` (wrapped in a `{to}`
/// envelope). Ends when either side closes; the caller reaps the entry lazily
/// (a dead `to_operator` receiver surfaces as a failed `send`).
fn spawn_bridge(
    device: String,
    local_ws_url: String,
    mut rx: mpsc::UnboundedReceiver<String>,
    out: mpsc::UnboundedSender<String>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (stream, _) = match tokio_tungstenite::connect_async(&local_ws_url).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, device, "relay: loopback operator connect failed; dropping session");
                return;
            }
        };
        let (mut sink, mut source) = stream.split();
        loop {
            tokio::select! {
                frame = rx.recv() => match frame {
                    Some(f) => {
                        if sink.send(Message::Text(f.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break, // bridge dropped by the supervisor
                },
                msg = source.next() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(envelope) = wrap_out(&device, &text) {
                            if out.send(envelope).is_err() {
                                break; // relay connection gone; supervisor rebuilds
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::debug!(error = %e, device, "relay: loopback WS error");
                        break;
                    }
                },
            }
        }
        let _ = sink.send(Message::Close(None)).await;
        tracing::debug!(device, "relay: loopback bridge ended");
    })
}

/// Read the freshest Smoo access token from the stored session. `None` when
/// signed out / no store — the supervisor waits and retries.
fn fresh_access_token() -> Option<String> {
    let store = CredentialsStore::default_user().ok()?;
    let creds = store.load().ok().flatten()?;
    Some(creds.access_token).filter(|t| !t.is_empty())
}

/// Spawn the relay supervisor.
///
/// (Re)connects to the relay with a fresh token, forwards envelopes phone ⇄
/// operator via per-device loopback bridges, backs off exponentially on drops,
/// and waits patiently while signed out. Never fails the daemon — every error
/// is a log line and a retry.
pub fn spawn_relay(relay_url: String, local_port: u16, local_token: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let local_ws_url = format!("ws://127.0.0.1:{local_port}/ws?token={}", urlencode(&local_token));
        let mut backoff = BACKOFF_MIN;
        loop {
            let Some(token) = fresh_access_token() else {
                tracing::debug!("relay: no Smoo session (signed out) — retrying in {SIGNED_OUT_RECHECK:?}");
                tokio::time::sleep(SIGNED_OUT_RECHECK).await;
                continue;
            };
            let url = format!("{relay_url}?token={}&device={DAEMON_DEVICE}", urlencode(&token));
            let connected_at = std::time::Instant::now();
            match tokio_tungstenite::connect_async(&url).await {
                Ok((stream, _)) => {
                    tracing::info!(relay = %relay_url, "relay: connected — Big Smooth is reachable without tailscale");
                    run_connection(stream, &local_ws_url).await;
                    tracing::warn!("relay: connection ended; reconnecting");
                }
                Err(e) => {
                    tracing::warn!(error = %e, relay = %relay_url, "relay: connect failed");
                }
            }
            // A connection that lived a while earns a fresh backoff.
            if connected_at.elapsed() > BACKOFF_RESET_AFTER {
                backoff = BACKOFF_MIN;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    })
}

/// One live relay connection: pump relay ⇄ bridges until the socket ends.
async fn run_connection(stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, local_ws_url: &str) {
    let (mut sink, mut source) = stream.split();
    // All bridges push outbound envelopes through one channel — the single
    // writer to the relay socket.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let mut bridges: HashMap<String, Bridge> = HashMap::new();

    loop {
        tokio::select! {
            envelope = out_rx.recv() => match envelope {
                // Bridges hold clones of out_tx, so recv() only ever yields
                // None when… it can't (we hold out_tx too). Guard anyway.
                Some(e) => {
                    if sink.send(Message::Text(e.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            msg = source.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        tracing::debug!(error = %e, "relay: socket error");
                        break;
                    }
                };
                match classify_relay_msg(&text) {
                    RelayMsg::Ping => {
                        if sink.send(Message::Text(r#"{"type":"pong"}"#.into())).await.is_err() {
                            break;
                        }
                    }
                    RelayMsg::Ignore => {}
                    RelayMsg::PeerOffline(device) => {
                        // The phone we last wrote to is gone — reap its bridge so a
                        // reconnecting phone gets a fresh operator session.
                        bridges.remove(&device);
                    }
                    RelayMsg::Frame(from, frame) => {
                        // Get-or-(re)spawn the bridge, then forward. A bridge whose
                        // task died (operator restart) fails the send — respawn once.
                        let delivered = bridges
                            .get(&from)
                            .is_some_and(|b| b.to_operator.send(frame.clone()).is_ok() && !b.task.is_finished());
                        if !delivered {
                            let (tx, rx) = mpsc::unbounded_channel();
                            let task = spawn_bridge(from.clone(), local_ws_url.to_string(), rx, out_tx.clone());
                            let _ = tx.send(frame);
                            bridges.insert(from, Bridge { to_operator: tx, task });
                        }
                    }
                }
            }
        }
    }
    // Dropping the map aborts every bridge task (Bridge::drop).
    bridges.clear();
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    // ── config resolution ─────────────────────────────────────────────────────

    #[test]
    fn relay_enabled_by_default_at_the_production_url() {
        assert_eq!(resolve_relay_url_from(None, None).as_deref(), Some(DEFAULT_RELAY_URL));
    }

    #[test]
    fn relay_disabled_by_kill_switch() {
        for v in ["0", "false", "off", "no", " 0 "] {
            assert_eq!(resolve_relay_url_from(Some(v), None), None, "SMOOTH_RELAY={v}");
        }
        // Anything else (incl. "1") leaves it on.
        assert!(resolve_relay_url_from(Some("1"), None).is_some());
    }

    #[test]
    fn relay_url_override_wins() {
        assert_eq!(
            resolve_relay_url_from(None, Some("wss://relay.dev.smoo.ai/ws")).as_deref(),
            Some("wss://relay.dev.smoo.ai/ws")
        );
        // Empty override ⇒ disabled rather than a junk dial loop.
        assert_eq!(resolve_relay_url_from(None, Some("")), None);
    }

    // ── inbound classification ────────────────────────────────────────────────

    #[test]
    fn classify_ping_and_control_noise() {
        assert_eq!(classify_relay_msg(r#"{"type":"ping"}"#), RelayMsg::Ping);
        assert_eq!(classify_relay_msg(r#"{"type":"connected"}"#), RelayMsg::Ignore);
        assert_eq!(classify_relay_msg(r#"{"type":"pong"}"#), RelayMsg::Ignore);
        assert_eq!(classify_relay_msg(r#"{"type":"error","message":"x"}"#), RelayMsg::Ignore);
        assert_eq!(classify_relay_msg("not json"), RelayMsg::Ignore);
        assert_eq!(classify_relay_msg(r#"{"unrelated":true}"#), RelayMsg::Ignore);
    }

    #[test]
    fn classify_peer_offline_names_the_device() {
        assert_eq!(
            classify_relay_msg(r#"{"type":"peer_offline","to":"phone-abc"}"#),
            RelayMsg::PeerOffline("phone-abc".to_string())
        );
        // Malformed peer_offline (no `to`) is noise, not a panic.
        assert_eq!(classify_relay_msg(r#"{"type":"peer_offline"}"#), RelayMsg::Ignore);
    }

    #[test]
    fn classify_frame_extracts_sender_and_opaque_frame() {
        let RelayMsg::Frame(from, frame) = classify_relay_msg(r#"{"from":"phone-a","frame":{"action":"send_message","message":"hi"}}"#) else {
            panic!("expected Frame");
        };
        assert_eq!(from, "phone-a");
        let v: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["action"], "send_message");
    }

    #[test]
    fn classify_frame_with_incidental_type_field_still_forwards() {
        // An envelope whose inner frame leaks a top-level `type` on the OUTER
        // object would be a relay bug, but a non-control `type` must not
        // swallow a real envelope… the relay never produces this; we classify
        // unknown types as Ignore deliberately (control-plane forward-compat)
        // and rely on the relay's envelope shape ({from, frame} with no type).
        assert_eq!(classify_relay_msg(r#"{"type":"future_control","from":"x","frame":{}}"#), RelayMsg::Ignore);
    }

    // ── outbound wrapping ─────────────────────────────────────────────────────

    #[test]
    fn wrap_out_addresses_the_device_and_embeds_json() {
        let w = wrap_out("phone-a", r#"{"type":"stream_token","token":"hi"}"#).unwrap();
        let v: Value = serde_json::from_str(&w).unwrap();
        assert_eq!(v["to"], "phone-a");
        assert_eq!(v["frame"]["type"], "stream_token");
    }

    #[test]
    fn wrap_out_drops_non_json_noise() {
        assert_eq!(wrap_out("phone-a", "not json"), None);
        assert_eq!(wrap_out("phone-a", ""), None);
    }

    #[test]
    fn urlencode_reserves() {
        assert_eq!(urlencode("abc-XYZ_0.9~"), "abc-XYZ_0.9~");
        assert_eq!(urlencode("a b+c"), "a%20b%2Bc");
    }

    // ── bridge integration: fake operator + fake relay channel ───────────────

    /// Spin a real loopback WS server that echoes one canned reply per inbound
    /// frame, then run a bridge against it and assert the round trip.
    #[tokio::test]
    async fn bridge_pumps_frames_both_ways() {
        use axum::extract::ws::{Message as AxMsg, WebSocket, WebSocketUpgrade};
        use axum::routing::get;
        use axum::Router;

        async fn fake_operator(mut ws: WebSocket) {
            while let Some(Ok(msg)) = ws.recv().await {
                if let AxMsg::Text(t) = msg {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    assert_eq!(v["action"], "send_message", "bridge must pass frames through untouched");
                    let _ = ws.send(AxMsg::Text(r#"{"type":"stream_token","token":"pong-from-operator"}"#.into())).await;
                }
            }
        }

        let app = Router::new().route("/ws", get(|u: WebSocketUpgrade| async move { u.on_upgrade(fake_operator) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let (to_op_tx, to_op_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let _task = spawn_bridge("phone-test".to_string(), format!("ws://{addr}/ws?token=x"), to_op_rx, out_tx);

        to_op_tx.send(r#"{"action":"send_message","message":"hello"}"#.to_string()).unwrap();

        let envelope = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
            .await
            .expect("reply within 5s")
            .expect("channel alive");
        let v: Value = serde_json::from_str(&envelope).unwrap();
        assert_eq!(v["to"], "phone-test");
        assert_eq!(v["frame"]["type"], "stream_token");
        assert_eq!(v["frame"]["token"], "pong-from-operator");
    }
}

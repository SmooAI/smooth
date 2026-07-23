//! SEP WebSocket turn helper — talk to the org Smooth Operator over its
//! WebSocket transport.
//!
//! The buffered REST path (`POST /organizations/{org}/smooth-operator/chat`) was
//! deleted in SMOODEV-2673; the supported transport is now the SEP WebSocket.
//! There is no importable Rust client in the `smooth-operator` crates (they are
//! server-side only), so this hand-rolls one buffered turn against the wire
//! shapes in `smooth-operator-server`'s `protocol.rs` / `handler.rs`:
//!
//! 1. mint a short-lived ES256 token — `POST /organizations/{org}/smooth-operator/token`
//!    (authed as the user session) → `{ token, expiresAt }`;
//! 2. connect `wss://smooth-operator.smoo.ai/ws?token=…`;
//! 3. `create_conversation_session` (pass `conversationId` to resume) → the
//!    `immediate_response` carries `data.sessionId`;
//! 4. `send_message { sessionId, message, stream: true }`;
//! 5. read frames, ignoring `stream_*`; on `write_confirmation_required` reply
//!    `confirm_tool_action { approved }` (never send/act without an explicit
//!    `approve = true`); stop on `eventual_response` and return its reply.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::smooai::user_client::UserClient;

const DEFAULT_WS_URL: &str = "wss://smooth-operator.smoo.ai/ws";
/// Per-frame read timeout — a healthy turn streams frequently; this bounds a
/// hung server without cutting off a long-running tool.
const FRAME_TIMEOUT: Duration = Duration::from_secs(90);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The buffered result of one operator turn.
pub struct OperatorTurn {
    /// The operator's final reply text.
    pub reply: String,
    /// The conversation id — pass it back to continue the thread.
    pub conversation_id: String,
    /// Human-readable descriptions of destructive actions the operator paused on
    /// that we DECLINED (because `approve` was false). Surfaced so the caller can
    /// re-run with `approve = true` to allow them.
    pub declined: Vec<String>,
}

fn ws_url() -> String {
    std::env::var("SMOOTH_OPERATOR_WS_URL").unwrap_or_else(|_| DEFAULT_WS_URL.to_string())
}

fn request_id() -> String {
    static RID: AtomicU64 = AtomicU64::new(1);
    format!("th-{}-{}", std::process::id(), RID.fetch_add(1, Ordering::Relaxed))
}

/// Run one buffered turn of the org Smooth Operator over the SEP WebSocket.
///
/// `approve` gates destructive tools: when the operator pauses on one
/// (`write_confirmation_required`), it runs only if `approve` is true; otherwise
/// it is declined and recorded in [`OperatorTurn::declined`]. Declining is the
/// safe default for a headless client.
///
/// # Errors
/// Returns an error if there is no Smoo user session, the token mint fails, the
/// WebSocket can't connect, the turn times out, or the operator emits an error /
/// an interactive step this client can't satisfy (OTP, rich interaction).
pub async fn operator_turn(org: &str, message: &str, conversation_id: Option<&str>, approve: bool) -> Result<OperatorTurn> {
    // 1. Mint the short-lived SEP token from api-prime (user session only).
    let http = UserClient::from_user_session().await.context("sign in to Smoo (run `th auth login`)")?;
    let minted = http
        .post(&format!("/organizations/{org}/smooth-operator/token"), &json!({}))
        .await
        .context("mint smooth-operator token")?;
    let token = minted
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("token endpoint returned no token"))?;

    // 2. Connect.
    let (mut ws, _resp) = connect_async(format!("{}?token={token}", ws_url()))
        .await
        .context("connect smooth-operator websocket")?;

    // 3. Create or resume the conversation session.
    let mut create = json!({ "action": "create_conversation_session", "requestId": request_id() });
    if let Some(cid) = conversation_id {
        create["conversationId"] = json!(cid);
    }
    send(&mut ws, &create).await?;
    let created = recv_until(&mut ws, "immediate_response").await?;
    let session_id = created
        .pointer("/data/sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("session create returned no sessionId"))?
        .to_string();
    let conv_id = created.pointer("/data/conversationId").and_then(Value::as_str).unwrap_or_default().to_string();

    // 4. Send the message.
    let send_rid = request_id();
    send(
        &mut ws,
        &json!({ "action": "send_message", "requestId": send_rid, "sessionId": session_id, "message": message, "stream": true }),
    )
    .await?;

    // 5. Buffer the streamed turn.
    let mut declined = Vec::new();
    loop {
        let ev = recv(&mut ws).await?;
        match ev.get("type").and_then(Value::as_str) {
            Some("write_confirmation_required") => {
                let rid = ev.get("requestId").and_then(Value::as_str).unwrap_or(&send_rid).to_string();
                let desc = ev
                    .pointer("/data/data/actionDescription")
                    .and_then(Value::as_str)
                    .unwrap_or("an action")
                    .to_string();
                if !approve {
                    declined.push(desc);
                }
                send(
                    &mut ws,
                    &json!({ "action": "confirm_tool_action", "requestId": rid, "sessionId": session_id, "approved": approve }),
                )
                .await?;
            }
            Some("eventual_response") => {
                let reply = extract_reply(ev.pointer("/data/data/response"));
                let _ = ws.close(None).await;
                return Ok(OperatorTurn {
                    reply,
                    conversation_id: conv_id,
                    declined,
                });
            }
            Some("otp_verification_required" | "interaction_required") => {
                let _ = ws.close(None).await;
                return Err(anyhow!(
                    "the operator needs an interactive step (verification / rich interaction) that isn't supported from this client"
                ));
            }
            Some("error") => {
                let msg = ev.get("message").and_then(Value::as_str).unwrap_or("unknown operator error");
                return Err(anyhow!("smooth-operator error: {msg}"));
            }
            // stream_preamble / stream_chunk / the send ack — ignore.
            _ => {}
        }
    }
}

async fn send(ws: &mut Ws, value: &Value) -> Result<()> {
    ws.send(Message::Text(value.to_string().into())).await.context("send websocket frame")
}

async fn recv(ws: &mut Ws) -> Result<Value> {
    let frame = tokio::time::timeout(FRAME_TIMEOUT, ws.next())
        .await
        .context("timed out waiting for the operator")?
        .ok_or_else(|| anyhow!("operator websocket closed unexpectedly"))?
        .context("operator websocket error")?;
    match frame {
        Message::Text(t) => serde_json::from_str(&t).context("parse operator event"),
        Message::Close(_) => Err(anyhow!("operator websocket closed")),
        // ping/pong/binary — not an event; caller loops.
        _ => Ok(json!({ "type": "_ignore" })),
    }
}

async fn recv_until(ws: &mut Ws, ty: &str) -> Result<Value> {
    loop {
        let ev = recv(ws).await?;
        match ev.get("type").and_then(Value::as_str) {
            Some(t) if t == ty => return Ok(ev),
            Some("error") => {
                let msg = ev.get("message").and_then(Value::as_str).unwrap_or("unknown");
                return Err(anyhow!("smooth-operator error: {msg}"));
            }
            _ => {}
        }
    }
}

/// Extract the reply text from an `eventual_response`'s `response` value. The
/// engine returns `{ responseParts: [...] }`; parts may be strings or
/// `{ text | content }` objects. Falls back to the raw value.
fn extract_reply(response: Option<&Value>) -> String {
    let Some(resp) = response else {
        return String::new();
    };
    if let Some(parts) = resp.get("responseParts").and_then(Value::as_array) {
        let mut out = String::new();
        for p in parts {
            if let Some(s) = p.as_str() {
                out.push_str(s);
            } else if let Some(s) = p.get("text").and_then(Value::as_str) {
                out.push_str(s);
            } else if let Some(s) = p.get("content").and_then(Value::as_str) {
                out.push_str(s);
            }
        }
        if !out.trim().is_empty() {
            return out.trim().to_string();
        }
    }
    if let Some(s) = resp.as_str() {
        return s.to_string();
    }
    if let Some(s) = resp.get("text").and_then(Value::as_str) {
        return s.to_string();
    }
    resp.to_string()
}

/// Render a buffered turn for a text client (the MCP tool / CLI): reply, any
/// declined-pending-approval actions, and the conversation id to continue.
#[must_use]
pub fn render_operator_turn(turn: &OperatorTurn) -> String {
    use std::fmt::Write as _;
    let mut out = turn.reply.clone();
    for d in &turn.declined {
        let _ = write!(
            out,
            "\n\n⏸ I did NOT do this without your approval: {d}\n   To allow it, re-run with approve=true (conversation_id=\"{}\").",
            turn.conversation_id
        );
    }
    if !turn.conversation_id.is_empty() {
        let _ = write!(out, "\n\n[conversation_id: {}]", turn.conversation_id);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_reply_from_response_parts() {
        let resp = json!({ "responseParts": [{ "text": "Revenue is " }, { "text": "up 12%." }] });
        assert_eq!(extract_reply(Some(&resp)), "Revenue is up 12%.");
    }

    #[test]
    fn extract_reply_falls_back_to_string() {
        assert_eq!(extract_reply(Some(&json!("plain text"))), "plain text");
        assert_eq!(extract_reply(None), "");
    }

    #[test]
    fn render_surfaces_declined_action_and_conversation() {
        let turn = OperatorTurn {
            reply: "Drafted the renewal.".to_string(),
            conversation_id: "c-9".to_string(),
            declined: vec!["send the renewal email to Acme".to_string()],
        };
        let out = render_operator_turn(&turn);
        assert!(out.contains("Drafted the renewal."));
        assert!(out.contains("did NOT do this without your approval: send the renewal email to Acme"));
        assert!(out.contains("approve=true"));
        assert!(out.contains("[conversation_id: c-9]"));
    }

    #[test]
    fn render_plain_reply_has_no_approval_note() {
        let turn = OperatorTurn {
            reply: "All good.".to_string(),
            conversation_id: "c-1".to_string(),
            declined: vec![],
        };
        let out = render_operator_turn(&turn);
        assert!(out.contains("All good."));
        assert!(!out.contains("approval"));
        assert!(out.contains("[conversation_id: c-1]"));
    }
}

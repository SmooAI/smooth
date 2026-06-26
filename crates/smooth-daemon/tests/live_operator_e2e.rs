//! Integration + live-LLM E2E for the operator **local deployment flavor** the
//! daemon runs (`th daemon operator`).
//!
//! Boots the local flavor in-process exactly the way [`smooth_daemon::
//! serve_local_flavor`] does — a [`LocalTokenVerifier`], the daemon's
//! [`local_tool_provider`] (workspace-confined fs/grep + OS-sandboxed `bash`),
//! and (for the live test) a real gateway config — then drives the canonical WS
//! protocol with a real client.
//!
//! Two tests:
//!   1. `local_flavor_tokenless_connection_is_anonymous` (always runs, no LLM):
//!      documents that the operator's `/ws` **degrades a missing token to an
//!      anonymous connection rather than rejecting it** — so `LocalTokenVerifier`
//!      does NOT gate connections, only ACL scope. (Surfaced by e2e testing; see
//!      the strict-auth follow-up.)
//!   2. `live_e2e_sandboxed_bash_executes_in_a_real_turn` (gated on
//!      `SMOOTH_AGENT_E2E=1` + `SMOOAI_GATEWAY_KEY`): a real LLM turn that makes
//!      the agent call the sandboxed `bash` tool and echoes a magic string back —
//!      proving protocol + agent loop + **sandboxed tool execution** + the live
//!      gateway end-to-end.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "unwrap/expect are the idiom for test assertions")]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use smooth_operator_server::config::ServerConfig;
use smooth_operator_server::local::LocalServer;
use smooth_operator_svc::auth::LocalTokenVerifier;

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

const GATEWAY_URL: &str = "https://llm.smoo.ai/v1";
const CHEAP_MODEL: &str = "claude-haiku-4-5";
const TURN_TIMEOUT: Duration = Duration::from_secs(120);
const MAGIC: &str = "SANDBOX_OK_4242";

async fn send_json(client: &mut Client, value: &Value) {
    client.send(WsMessage::Text(value.to_string().into())).await.expect("send frame");
}

async fn recv_json(client: &mut Client) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(30), client.next())
        .await
        .expect("recv timed out")
        .expect("stream ended")
        .expect("ws error");
    match frame {
        WsMessage::Text(t) => serde_json::from_str(&t).expect("parse json"),
        other => panic!("expected text frame, got {other:?}"),
    }
}

async fn recv_until(client: &mut Client, ty: &str, seen: &mut Vec<Value>, overall: Duration) -> Value {
    let deadline = tokio::time::Instant::now() + overall;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for '{ty}'; saw {:?}",
            seen.iter().map(|e| e["type"].clone()).collect::<Vec<_>>()
        );
        let frame = tokio::time::timeout(remaining, client.next())
            .await
            .expect("recv timed out")
            .expect("stream ended")
            .expect("ws error");
        if let WsMessage::Text(t) = frame {
            let ev: Value = serde_json::from_str(&t).expect("parse json");
            let this = ev["type"].as_str().unwrap_or_default().to_string();
            seen.push(ev.clone());
            if this == ty || this == "error" {
                return ev;
            }
        }
    }
}

/// Boot the local flavor on an ephemeral port with the given (optional) gateway
/// config + a `LocalTokenVerifier`, returning the running server handle.
async fn boot_local_flavor(gateway: Option<(String, String)>) -> LocalServer {
    let workspace = std::env::temp_dir();
    let provider = smooth_daemon::local_tool_provider(workspace, None);
    let mut builder = LocalServer::builder()
        .addr("127.0.0.1:0".parse().unwrap())
        .auth(std::sync::Arc::new(LocalTokenVerifier::new("e2e-tok")))
        // Match the daemon: strict auth rejects tokenless connections.
        .strict_auth(true)
        .tools(provider);
    if let Some((url, key)) = gateway {
        let mut cfg = ServerConfig::from_env();
        cfg.gateway_url = url;
        cfg.gateway_key = Some(key);
        cfg.model = CHEAP_MODEL.into();
        cfg.seed_kb = false;
        cfg.max_iterations = 6;
        cfg.max_tokens = 512;
        builder = builder.config(cfg);
    }
    builder.spawn().await.expect("boot local flavor")
}

async fn create_session(client: &mut Client) -> String {
    send_json(
        client,
        &json!({
            "action": "create_conversation_session",
            "requestId": "e2e-cs",
            "agentId": uuid::Uuid::new_v4().to_string(),
            "userName": "E2E",
        }),
    )
    .await;
    let ev = recv_json(client).await;
    assert_eq!(ev["type"], "immediate_response", "session creation failed: {ev}");
    ev["data"]["sessionId"].as_str().expect("sessionId").to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_auth_rejects_tokenless_and_accepts_with_token() {
    // No gateway needed — `ping` doesn't run a turn.
    let server = boot_local_flavor(None).await;
    // No ?token= → strict auth REJECTS the upgrade (the fix: LocalTokenVerifier
    // now genuinely gates connections; previously this degraded to anonymous and
    // succeeded — the security gap e2e testing surfaced).
    assert!(
        connect_async(server.ws_url()).await.is_err(),
        "strict auth must reject a tokenless /ws connection (not degrade to anonymous)"
    );
    // With the right token → connects + serves.
    let url = format!("{}?token=e2e-tok", server.ws_url());
    let (mut client, _) = connect_async(&url).await.expect("connect with valid token");
    send_json(&mut client, &json!({"action": "ping", "requestId": "p1"})).await;
    assert_eq!(recv_json(&mut client).await["type"], "pong", "valid-token connection is served");
    server.shutdown().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn live_e2e_sandboxed_bash_executes_in_a_real_turn() {
    if std::env::var("SMOOTH_AGENT_E2E").as_deref() != Ok("1") {
        eprintln!("[skip] live e2e: set SMOOTH_AGENT_E2E=1 to run");
        return;
    }
    let key = match std::env::var("SMOOAI_GATEWAY_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => {
            eprintln!("[skip] live e2e: SMOOAI_GATEWAY_KEY unset");
            return;
        }
    };

    let server = boot_local_flavor(Some((GATEWAY_URL.into(), key))).await;
    let url = format!("{}?token=e2e-tok", server.ws_url());
    let (mut client, _) = connect_async(&url).await.expect("connect ws with token");
    let session_id = create_session(&mut client).await;
    eprintln!("[live-e2e] session {session_id}");

    send_json(
        &mut client,
        &json!({
            "action": "send_message",
            "requestId": "turn-1",
            "sessionId": session_id,
            "message": format!("Use the bash tool to run exactly this command: echo {MAGIC}. Then reply with only the command's stdout, nothing else."),
        }),
    )
    .await;

    let mut seen = Vec::new();
    let eventual = recv_until(&mut client, "eventual_response", &mut seen, TURN_TIMEOUT).await;
    assert_ne!(eventual["type"], "error", "turn errored: {eventual}");

    let types: Vec<String> = seen.iter().filter_map(|e| e["type"].as_str().map(str::to_string)).collect();
    eprintln!("[live-e2e] event types: {types:?}");
    let streamed = types.iter().any(|t| t == "stream_token" || t == "stream_chunk");
    assert!(streamed, "expected streaming events, saw {types:?}");

    // The strongest end-to-end signal: the magic string only appears if the
    // agent actually called the sandboxed bash tool, it ran `echo`, and the LLM
    // saw the tool output and echoed it back.
    let blob = serde_json::to_string(&eventual).unwrap();
    assert!(
        blob.contains(MAGIC),
        "eventual_response should contain the sandboxed bash output {MAGIC}: {eventual}"
    );
    eprintln!("[live-e2e] PASS — sandboxed bash output round-tripped through a real LLM turn");
    server.shutdown().await.ok();
}

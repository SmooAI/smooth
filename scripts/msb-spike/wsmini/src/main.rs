// Minimal linux stand-in for a smooth-operator LocalServer, just enough
// canonical WS protocol to prove the msb-microVM transport end to end:
//   create_conversation_session  ->  immediate_response{ data.sessionId }
// ponytail: handshake-only fixture; the real proof is msb->WS transport,
// not the engine. Swap for `th daemon` once cross-compiled.
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let addr = std::env::var("SMOOTH_ADDR").unwrap_or_else(|_| "0.0.0.0:8791".into());
    let listener = TcpListener::bind(&addr).await.expect("bind");
    eprintln!("wsmini listening on {addr} (path /ws)");
    while let Ok((stream, peer)) = listener.accept().await {
        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    eprintln!("handshake failed from {peer}: {e}");
                    return;
                }
            };
            let (mut sink, mut source) = ws.split();
            while let Some(Ok(msg)) = source.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                if v.get("action").and_then(|a| a.as_str()) == Some("create_conversation_session") {
                    let sid = format!("sess-{}", &v.get("agentId").and_then(|a| a.as_str()).unwrap_or("x")[..8.min(v.get("agentId").and_then(|a| a.as_str()).unwrap_or("x").len())]);
                    let reply = serde_json::json!({
                        "type": "immediate_response",
                        "requestId": v.get("requestId"),
                        "data": { "sessionId": sid }
                    });
                    let _ = sink.send(Message::Text(reply.to_string().into())).await;
                }
            }
        });
    }
}

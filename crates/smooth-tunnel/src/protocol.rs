//! Wire protocol for th.smoo.ai reverse tunnel.
//!
//! # Transport
//!
//! A single **WebSocket** connection from the `th` daemon to the
//! rendezvous service (`wss://th.smoo.ai/tunnel`). Control frames are
//! JSON text messages; bulk request/response bodies are WebSocket
//! binary messages with a tiny framing header — see [`WireFrame`].
//!
//! Why a single WS rather than HTTP/2 streams: HTTP/2 clients don't
//! typically accept server-initiated streams, and the whole point
//! here is that the server receives an inbound request and needs to
//! hand it to the client. Custom multiplex over WS sidesteps that.
//!
//! # Session lifecycle
//!
//! ```text
//!   Client              Service
//!     │ ──WS upgrade─→    │
//!     │ ──ClientHello─→   │   (auth + slug preference)
//!     │ ←─ServerHello──   │   (assigned slug + public URL)
//!     │                   │
//!     │   ─ Ping/Pong ─   │   (keepalive)
//!     │                   │
//!     │ ←─StreamOpen───   │   (an inbound request arrived on the public URL)
//!     │ ←─StreamData──    │   (request body bytes)
//!     │ ──StreamData─→    │   (response body bytes back)
//!     │ ──StreamClose─→   │
//!     │        ⋮          │
//!     │ ──Bye─→           │
//! ```
//!
//! # Frame budget
//!
//! `WireFrame` values are tiny envelopes so both sides can hot-loop
//! without extra allocation. Frames that carry bulk bytes use
//! [`StreamData`] with a `base64`-free, length-prefixed binary body:
//! we send the control JSON as a **text** WS message and the body as
//! the **next** binary WS message, correlated by `stream_id`. This
//! avoids serializing large payloads through serde twice.

use serde::{Deserialize, Serialize};

/// Current protocol version. Bumped on breaking changes to
/// [`ClientHello`] / [`ServerHello`] / frame shapes. Both sides
/// refuse the connection on mismatch.
pub const PROTOCOL_VERSION: u32 = 1;

/// Envelope carried on every WebSocket **text** message. Binary
/// messages are body bytes correlated by the most-recent frame's
/// `stream_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireFrame {
    /// Client → Service. The very first message after the WS upgrade.
    ClientHello(ClientHello),
    /// Service → Client. Reply to [`ClientHello`] with the assigned
    /// slug + URL.
    ServerHello(ServerHello),
    /// Either side, any time. Cheap keepalive. Replies with
    /// [`WireFrame::Pong`] carrying the same nonce.
    Ping { nonce: u64 },
    /// Reply to [`WireFrame::Ping`].
    Pong { nonce: u64 },
    /// Service → Client. A new inbound request has arrived on the
    /// public URL. Open a local connection to Big Smooth and prepare
    /// to shuffle bytes under `stream_id`.
    StreamOpen(StreamOpen),
    /// Either direction. One chunk of body bytes for an open stream.
    /// The WS message carrying this frame is immediately followed by
    /// a WS binary message with the raw `len` bytes. Using binary-
    /// after-text avoids base64 double-encoding.
    StreamData(StreamData),
    /// Either direction. Stream is finished; no more data will flow.
    StreamClose(StreamClose),
    /// Structured error, preferred over closing the WS with a reason
    /// code so both sides can log consistently.
    Control(ControlFrame),
    /// Either direction. Graceful session teardown. Equivalent to a
    /// WS close but with a machine-readable reason.
    Bye { reason: String },
}

/// First message from client to service after the WS upgrade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientHello {
    /// Must equal [`PROTOCOL_VERSION`]. Service refuses on mismatch.
    pub protocol_version: u32,
    /// Short-lived token minted against the user's smooai Supabase
    /// session — see [`crate::client::TunnelConfig::auth_token`].
    pub auth_token: String,
    /// Optional slug the client is hoping to get (`"my-review"` →
    /// `my-review.th.smoo.ai`). Service may reject + assign a fresh
    /// one anyway; see [`ServerHello::assigned_slug`].
    pub slug_preference: Option<String>,
    /// Opaque version string for diagnostics (e.g. `"th 0.8.0"`).
    pub user_agent: String,
}

/// Reply to [`ClientHello`]. On error the service sends
/// [`ControlFrame::Error`] and closes instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerHello {
    pub protocol_version: u32,
    /// The subdomain actually assigned. May differ from
    /// [`ClientHello::slug_preference`] when the client asked for a
    /// taken slug or wasn't entitled to a stable one.
    pub assigned_slug: String,
    /// The full public URL the user should share —
    /// `https://<assigned_slug>.th.smoo.ai/`.
    pub public_url: String,
    /// Opaque server-side handle for the session; logged on both
    /// sides to correlate traces.
    pub session_id: String,
}

/// "A new inbound request has arrived on the public URL" —
/// framing enough for the client to replay it against Big Smooth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOpen {
    /// Unique ID within this session. Chosen by the service; the
    /// client echoes it on every related frame.
    pub stream_id: u64,
    pub kind: StreamKind,
    /// HTTP method (e.g. `GET`) for [`StreamKind::Http`], ignored for
    /// [`StreamKind::WebSocket`].
    pub method: String,
    /// Path-and-query on the public URL, starting with `/`.
    pub path: String,
    /// Flattened HTTP headers. Hop-by-hop headers are stripped on the
    /// service side; `X-Forwarded-For` etc. are rewritten to reflect
    /// the real remote IP.
    pub headers: Vec<(String, String)>,
}

/// What kind of stream is being proxied.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Http,
    WebSocket,
}

/// Body-chunk envelope. The bytes themselves arrive on the next WS
/// binary message — see [`WireFrame::StreamData`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamData {
    pub stream_id: u64,
    /// Number of bytes the recipient should expect on the following
    /// WS binary frame. Using the header-before-body pattern avoids
    /// base64 encoding bulk payloads.
    pub len: u32,
    /// For HTTP streams only: set true on the *response* side to
    /// mark that this is the final data frame and the service
    /// should flush trailers (if any) + close the HTTP response.
    #[serde(default)]
    pub final_chunk: bool,
}

/// "Stream's done." Sent by either side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamClose {
    pub stream_id: u64,
    /// Machine-readable reason code. Unknown codes are allowed;
    /// both sides should log + move on.
    pub reason_code: String,
    /// Free-form detail, safe to surface to the end-user.
    pub reason: String,
}

/// Structured errors. Kept separate from [`TunnelError`] because
/// these cross the wire — the Rust error type doesn't.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlFrame {
    /// Something went wrong that isn't stream-specific. Typically
    /// followed by a WS close.
    Error { code: String, message: String },
    /// Diagnostic log line the service wants the client to surface
    /// to the user (e.g. "your slug expired, reconnect to refresh").
    Notice { level: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_hello_round_trips() {
        let hello = ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth_token: "ey.test.token".into(),
            slug_preference: Some("my-review".into()),
            user_agent: "th 0.8.0".into(),
        };
        let frame = WireFrame::ClientHello(hello);
        let json = serde_json::to_string(&frame).expect("serialize");
        assert!(json.contains("\"type\":\"client_hello\""), "tagged enum shape: {json}");
        let back: WireFrame = serde_json::from_str(&json).expect("deserialize");
        match back {
            WireFrame::ClientHello(h) => {
                assert_eq!(h.protocol_version, PROTOCOL_VERSION);
                assert_eq!(h.slug_preference.as_deref(), Some("my-review"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn server_hello_round_trips() {
        let frame = WireFrame::ServerHello(ServerHello {
            protocol_version: PROTOCOL_VERSION,
            assigned_slug: "scratch-3f21".into(),
            public_url: "https://scratch-3f21.th.smoo.ai/".into(),
            session_id: "sess_abc123".into(),
        });
        let json = serde_json::to_string(&frame).expect("serialize");
        let back: WireFrame = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back, WireFrame::ServerHello(h) if h.assigned_slug == "scratch-3f21"));
    }

    #[test]
    fn stream_open_round_trips_with_headers() {
        let open = StreamOpen {
            stream_id: 42,
            kind: StreamKind::Http,
            method: "POST".into(),
            path: "/api/tasks?foo=1".into(),
            headers: vec![
                ("Authorization".into(), "Bearer xyz".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
        };
        let frame = WireFrame::StreamOpen(open);
        let json = serde_json::to_string(&frame).expect("serialize");
        let back: WireFrame = serde_json::from_str(&json).expect("deserialize");
        match back {
            WireFrame::StreamOpen(o) => {
                assert_eq!(o.stream_id, 42);
                assert_eq!(o.method, "POST");
                assert_eq!(o.path, "/api/tasks?foo=1");
                assert_eq!(o.kind, StreamKind::Http);
                assert_eq!(o.headers.len(), 2);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn stream_data_defaults_final_chunk_false() {
        // `final_chunk` is #[serde(default)] so old clients/servers
        // that omit it keep interoperating. Lock the default.
        let json = r#"{"type":"stream_data","stream_id":1,"len":100}"#;
        let back: WireFrame = serde_json::from_str(json).expect("deserialize");
        match back {
            WireFrame::StreamData(d) => {
                assert_eq!(d.stream_id, 1);
                assert_eq!(d.len, 100);
                assert!(!d.final_chunk);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn stream_kind_serializes_snake_case() {
        let json = serde_json::to_string(&StreamKind::WebSocket).unwrap();
        assert_eq!(json, "\"web_socket\"");
    }

    #[test]
    fn control_frame_variants_round_trip() {
        let err = WireFrame::Control(ControlFrame::Error {
            code: "slug_taken".into(),
            message: "slug 'ops' is taken".into(),
        });
        let json = serde_json::to_string(&err).expect("serialize");
        let back: WireFrame = serde_json::from_str(&json).expect("deserialize");
        match back {
            WireFrame::Control(ControlFrame::Error { code, .. }) => assert_eq!(code, "slug_taken"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn ping_pong_correlate_on_nonce() {
        let p = WireFrame::Ping { nonce: 0xdead_beef };
        let json = serde_json::to_string(&p).unwrap();
        let back: WireFrame = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, WireFrame::Ping { nonce: 0xdead_beef }));
    }
}

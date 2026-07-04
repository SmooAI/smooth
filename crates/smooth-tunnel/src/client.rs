//! Client builder + config + the dial/handshake/multiplex loop.
//!
//! [`TunnelClient::run`] dials the rendezvous over WebSocket, does the
//! [`ClientHello`]/[`ServerHello`] handshake, then multiplexes inbound
//! HTTP requests from the public URL back to the local Big Smooth.
//!
//! WebSocket-*stream* proxying (an inbound `wss://<slug>.th.smoo.ai`
//! upgrade tunneled to a local WS) is not wired yet — those streams are
//! rejected with a `StreamClose`. Tracked as a follow-up; the HTTP path
//! covers reviewing/driving from a phone, which is the headline use.

use std::collections::HashMap;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::protocol::{ClientHello, ServerHello, StreamClose, StreamData, StreamKind, StreamOpen, WireFrame, PROTOCOL_VERSION};
use crate::slug::SlugPreference;
use crate::{Result, TunnelError};

/// Everything the client needs to know to dial the rendezvous.
///
/// Constructed via [`TunnelConfig::builder`] so the CLI can validate
/// individual fields incrementally and print clear errors.
#[derive(Debug, Clone)]
pub struct TunnelConfig {
    /// Rendezvous endpoint. Defaults to `wss://th.smoo.ai/tunnel`
    /// via [`TunnelConfig::production`]; dev/staging override it.
    pub service_url: Url,
    /// Local Big Smooth address the tunnel forwards to.
    /// Default: `http://127.0.0.1:4400`.
    pub local_target: Url,
    /// Short-lived token minted against the user's smooai Supabase
    /// session. The `th auth login` flow (not in this scaffold) is
    /// responsible for producing one.
    pub auth_token: String,
    /// Slug preference. See [`SlugPreference`].
    pub slug: SlugPreference,
    /// What goes into [`crate::protocol::ClientHello::user_agent`].
    pub user_agent: String,
}

impl TunnelConfig {
    /// Start a builder with production defaults. The caller still has
    /// to supply `auth_token`.
    ///
    /// # Panics
    ///
    /// Can only panic if the two compile-time-constant URLs
    /// (`wss://th.smoo.ai/tunnel` and `http://127.0.0.1:4400`) fail
    /// to parse, which they can't.
    #[must_use]
    #[allow(clippy::expect_used)] // static URL literals — parse can't fail
    pub fn production() -> TunnelConfigBuilder {
        TunnelConfigBuilder {
            service_url: Some(Url::parse("wss://th.smoo.ai/tunnel").expect("static URL parses")),
            local_target: Some(Url::parse("http://127.0.0.1:4400").expect("static URL parses")),
            auth_token: None,
            slug: SlugPreference::Ephemeral,
            user_agent: default_user_agent(),
        }
    }

    /// Empty builder for tests and for custom dev endpoints.
    #[must_use]
    pub fn builder() -> TunnelConfigBuilder {
        TunnelConfigBuilder {
            service_url: None,
            local_target: None,
            auth_token: None,
            slug: SlugPreference::Ephemeral,
            user_agent: default_user_agent(),
        }
    }
}

/// Incremental builder. `build()` validates everything in one shot
/// and returns a concrete error for the first problem it finds.
#[derive(Debug, Clone)]
pub struct TunnelConfigBuilder {
    service_url: Option<Url>,
    local_target: Option<Url>,
    auth_token: Option<String>,
    slug: SlugPreference,
    user_agent: String,
}

impl TunnelConfigBuilder {
    #[must_use]
    pub fn service_url(mut self, url: Url) -> Self {
        self.service_url = Some(url);
        self
    }
    #[must_use]
    pub fn local_target(mut self, url: Url) -> Self {
        self.local_target = Some(url);
        self
    }
    #[must_use]
    pub fn auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }
    #[must_use]
    pub fn slug(mut self, pref: SlugPreference) -> Self {
        self.slug = pref;
        self
    }
    #[must_use]
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }

    /// Finalize + validate.
    ///
    /// # Errors
    ///
    /// [`TunnelError::InvalidConfig`] if any required field is
    /// missing or a URL has the wrong scheme. [`TunnelError::InvalidSlug`]
    /// if the slug fails local validation.
    pub fn build(self) -> Result<TunnelConfig> {
        let service_url = self.service_url.ok_or_else(|| TunnelError::InvalidConfig("service_url is required".into()))?;
        if !matches!(service_url.scheme(), "ws" | "wss") {
            return Err(TunnelError::InvalidConfig(format!(
                "service_url must be ws:// or wss:// (got {})",
                service_url.scheme()
            )));
        }
        let local_target = self.local_target.ok_or_else(|| TunnelError::InvalidConfig("local_target is required".into()))?;
        if !matches!(local_target.scheme(), "http" | "https") {
            return Err(TunnelError::InvalidConfig(format!(
                "local_target must be http:// or https:// (got {})",
                local_target.scheme()
            )));
        }
        let auth_token = self.auth_token.ok_or_else(|| TunnelError::InvalidConfig("auth_token is required".into()))?;
        if auth_token.trim().is_empty() {
            return Err(TunnelError::InvalidConfig("auth_token must not be empty".into()));
        }
        self.slug.validate()?;
        Ok(TunnelConfig {
            service_url,
            local_target,
            auth_token,
            slug: self.slug,
            user_agent: self.user_agent,
        })
    }
}

/// Default `User-Agent` string if the caller doesn't override.
fn default_user_agent() -> String {
    format!("th-tunnel/{}", env!("CARGO_PKG_VERSION"))
}

/// The client end of the tunnel.
///
/// Construct with [`TunnelConfig`]; call [`TunnelClient::run`] to
/// dial + block until the session ends. `run` returns
/// [`TunnelError::NotImplementedYet`] until the ECS rendezvous
/// service is live (tracked in smooai pearl th-8898f2 / SMOODEV-637).
#[derive(Debug, Clone)]
pub struct TunnelClient {
    config: TunnelConfig,
}

impl TunnelClient {
    #[must_use]
    pub const fn new(config: TunnelConfig) -> Self {
        Self { config }
    }

    /// Read-only view of the config. Used by the CLI to print a
    /// status banner before / during the run.
    #[must_use]
    pub const fn config(&self) -> &TunnelConfig {
        &self.config
    }

    /// Dial the rendezvous, do the hello handshake, and multiplex
    /// inbound requests against the local target until the session
    /// ends. `on_ready` is called once with the [`ServerHello`] so the
    /// caller can print the public URL the moment it's assigned.
    ///
    /// # Errors
    ///
    /// [`TunnelError::Transport`] if the dial or WS I/O fails,
    /// [`TunnelError::ServiceError`] if the service rejects the
    /// handshake (bad token, version mismatch, …).
    pub async fn run(&self, on_ready: impl FnOnce(&ServerHello)) -> Result<()> {
        let (ws, _resp) = tokio_tungstenite::connect_async(self.config.service_url.as_str())
            .await
            .map_err(|e| TunnelError::Transport(format!("dial {}: {e}", self.config.service_url)))?;
        let (mut sink, mut stream) = ws.split();

        // Handshake: ClientHello → ServerHello.
        let hello = ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth_token: self.config.auth_token.clone(),
            slug_preference: self.config.slug.to_wire(),
            user_agent: self.config.user_agent.clone(),
        };
        send_frame(&mut sink, &WireFrame::ClientHello(hello)).await?;

        let server_hello = match next_frame(&mut stream).await? {
            Some(WireFrame::ServerHello(h)) if h.protocol_version == PROTOCOL_VERSION => h,
            Some(WireFrame::ServerHello(h)) => {
                return Err(TunnelError::ServiceError {
                    code: "protocol_mismatch".into(),
                    message: format!("service speaks protocol {}, client speaks {PROTOCOL_VERSION}", h.protocol_version),
                });
            }
            Some(WireFrame::Control(crate::protocol::ControlFrame::Error { code, message })) => return Err(TunnelError::ServiceError { code, message }),
            Some(other) => {
                return Err(TunnelError::Transport(format!("expected ServerHello, got {other:?}")));
            }
            None => return Err(TunnelError::Transport("connection closed during handshake".into())),
        };
        on_ready(&server_hello);
        tracing::info!(slug = %server_hello.assigned_slug, url = %server_hello.public_url, "th tunnel: session established");

        // The house HTTP client against the local Big Smooth. Retries
        // OFF — a proxy must not silently re-send a visitor's request
        // (non-idempotent POSTs would double-apply); the timeout +
        // circuit breaker are what we want from smooai-fetch here.
        let http = smooai_fetch::FetchBuilder::<serde_json::Value>::new()
            .without_retry()
            .with_timeout(LOCAL_REQUEST_TIMEOUT_MS)
            .build();
        let mut streams: HashMap<u64, PendingHttp> = HashMap::new();
        // A StreamData text frame is immediately followed by a binary WS
        // message carrying the bytes — this holds the correlation until
        // that binary frame arrives.
        let mut expecting_body: Option<u64> = None;

        while let Some(frame) = next_message(&mut stream).await? {
            match frame {
                Frame::Binary(bytes) => {
                    if let Some(id) = expecting_body.take() {
                        if let Some(p) = streams.get_mut(&id) {
                            p.body.extend_from_slice(&bytes);
                        }
                    }
                }
                Frame::Text(WireFrame::Ping { nonce }) => send_frame(&mut sink, &WireFrame::Pong { nonce }).await?,
                Frame::Text(WireFrame::Pong { .. }) => {}
                Frame::Text(WireFrame::StreamOpen(open)) => match open.kind {
                    StreamKind::Http => {
                        streams.insert(open.stream_id, PendingHttp { open, body: Vec::new() });
                    }
                    // Follow-up: tunnel an inbound WS upgrade to a local
                    // WS. Reject cleanly for now so the service can fall
                    // back / surface a clear error to the visitor.
                    StreamKind::WebSocket => {
                        send_frame(
                            &mut sink,
                            &WireFrame::StreamClose(StreamClose {
                                stream_id: open.stream_id,
                                reason_code: "unsupported".into(),
                                reason: "WebSocket stream proxying not implemented in this client yet".into(),
                            }),
                        )
                        .await?;
                    }
                },
                Frame::Text(WireFrame::StreamData(d)) => expecting_body = Some(d.stream_id),
                Frame::Text(WireFrame::StreamClose(c)) => {
                    // Request fully received → replay it against the local
                    // target and stream the response back.
                    if let Some(pending) = streams.remove(&c.stream_id) {
                        proxy_http(&http, &self.config.local_target, pending, &mut sink).await?;
                    }
                }
                Frame::Text(WireFrame::Bye { reason }) => {
                    tracing::info!(%reason, "th tunnel: service closed the session");
                    break;
                }
                Frame::Text(WireFrame::Control(crate::protocol::ControlFrame::Notice { level, message })) => {
                    tracing::info!(%level, %message, "th tunnel: service notice");
                }
                Frame::Text(WireFrame::Control(crate::protocol::ControlFrame::Error { code, message })) => {
                    return Err(TunnelError::ServiceError { code, message });
                }
                Frame::Text(WireFrame::ClientHello(_) | WireFrame::ServerHello(_)) => {
                    tracing::warn!("th tunnel: unexpected handshake frame mid-session, ignoring");
                }
                Frame::Close => break,
            }
        }
        Ok(())
    }
}

/// An inbound HTTP request being assembled from the wire. `body` grows
/// as `StreamData` binary frames arrive; dispatched on `StreamClose`.
struct PendingHttp {
    open: StreamOpen,
    body: Vec<u8>,
}

/// The subset of WS messages the multiplex loop cares about, after
/// decoding text frames into [`WireFrame`].
enum Frame {
    Text(WireFrame),
    Binary(Vec<u8>),
    Close,
}

type WsSink = futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>;
type WsStream = futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>;

/// Serialize a control frame to JSON and send it as a WS text message.
async fn send_frame(sink: &mut WsSink, frame: &WireFrame) -> Result<()> {
    let json = serde_json::to_string(frame).map_err(|e| TunnelError::Transport(format!("serialize frame: {e}")))?;
    sink.send(Message::Text(json.into()))
        .await
        .map_err(|e| TunnelError::Transport(format!("send frame: {e}")))
}

/// Read the next WS message and decode text into a [`WireFrame`].
/// Returns `Ok(None)` on a clean close, skips WS-level ping/pong/frames
/// the loop doesn't model.
async fn next_message(stream: &mut WsStream) -> Result<Option<Frame>> {
    while let Some(msg) = stream.next().await {
        let msg = msg.map_err(|e| TunnelError::Transport(format!("ws recv: {e}")))?;
        match msg {
            Message::Text(t) => {
                let frame: WireFrame = serde_json::from_str(&t).map_err(|e| TunnelError::Transport(format!("decode frame {t}: {e}")))?;
                return Ok(Some(Frame::Text(frame)));
            }
            Message::Binary(b) => return Ok(Some(Frame::Binary(b.to_vec()))),
            Message::Close(_) => return Ok(Some(Frame::Close)),
            // WS-level ping/pong are handled by tungstenite; ignore.
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    Ok(None)
}

/// Handshake-only convenience: read the next message, requiring it be a
/// text frame (handshake never carries binary).
async fn next_frame(stream: &mut WsStream) -> Result<Option<WireFrame>> {
    match next_message(stream).await? {
        Some(Frame::Text(f)) => Ok(Some(f)),
        Some(Frame::Binary(_)) => Err(TunnelError::Transport("unexpected binary during handshake".into())),
        Some(Frame::Close) | None => Ok(None),
    }
}

/// Per-request timeout against the local Big Smooth.
const LOCAL_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// Replay an assembled inbound request against the local Big Smooth and
/// stream the response back over the tunnel as StreamData + StreamClose.
async fn proxy_http(http: &smooai_fetch::FetchClient<serde_json::Value>, local_target: &Url, pending: PendingHttp, sink: &mut WsSink) -> Result<()> {
    let id = pending.open.stream_id;
    let url = local_request_url(local_target, &pending.open.path)?;
    let init = build_request_init(&pending);
    let resp = match http.fetch(url.as_str(), init).await {
        Ok(r) => r,
        Err(e) => {
            // Surface a 502-ish close to the service rather than tearing
            // the whole tunnel down for one bad local request.
            return send_frame(
                sink,
                &WireFrame::StreamClose(StreamClose {
                    stream_id: id,
                    reason_code: "local_unreachable".into(),
                    reason: format!("local target error: {e}"),
                }),
            )
            .await;
        }
    };
    // ponytail: smooai-fetch hands back the body as a String, so this
    // path is UTF-8-faithful (JSON / HTML / text — the control-plane
    // traffic) but lossy for binary responses (fonts/images in the web
    // bundle). Fine for review-from-phone; binary fidelity is the
    // follow-up (raw-bytes response path) tracked on th-e82dac.
    let body = resp.body.into_bytes();
    let len = u32::try_from(body.len()).map_err(|_| TunnelError::Transport("local response exceeds 4 GiB".into()))?;
    send_frame(
        sink,
        &WireFrame::StreamData(StreamData {
            stream_id: id,
            len,
            final_chunk: true,
        }),
    )
    .await?;
    sink.send(Message::Binary(body.into()))
        .await
        .map_err(|e| TunnelError::Transport(format!("send response body: {e}")))?;
    send_frame(
        sink,
        &WireFrame::StreamClose(StreamClose {
            stream_id: id,
            reason_code: "ok".into(),
            reason: String::new(),
        }),
    )
    .await
}

/// Map an assembled inbound request to a smooai-fetch [`RequestInit`].
/// Split out so the method/header/body mapping is unit-testable without
/// a live socket.
fn build_request_init(pending: &PendingHttp) -> smooai_fetch::RequestInit {
    let headers = pending
        .open
        .headers
        .iter()
        // Drop Host — the client sets it from the local target; a stale
        // public-URL Host confuses the local server's routing.
        .filter(|(k, _)| !k.eq_ignore_ascii_case("host"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    smooai_fetch::RequestInit {
        method: wire_method(&pending.open.method),
        headers,
        body: (!pending.body.is_empty()).then(|| String::from_utf8_lossy(&pending.body).into_owned()),
    }
}

/// Map the wire method string to smooai-fetch's [`Method`]. Unknown
/// methods fall back to GET (the service only forwards standard HTTP
/// methods; a bad one shouldn't panic the loop).
fn wire_method(method: &str) -> smooai_fetch::Method {
    use smooai_fetch::Method;
    match method.to_ascii_uppercase().as_str() {
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "PATCH" => Method::PATCH,
        "DELETE" => Method::DELETE,
        "HEAD" => Method::HEAD,
        "OPTIONS" => Method::OPTIONS,
        _ => Method::GET,
    }
}

/// Join the public-URL path-and-query onto the local target base.
/// Pure — the core of request routing, tested directly.
fn local_request_url(local_target: &Url, path: &str) -> Result<Url> {
    // `path` already starts with `/` and includes the query. `Url::join`
    // replaces the base path with an absolute one, which is exactly what
    // we want.
    local_target
        .join(path)
        .map_err(|e| TunnelError::Transport(format!("join local path {path}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_token() -> String {
        "ey.test.token".to_string()
    }

    #[test]
    fn production_builder_has_defaults() {
        let cfg = TunnelConfig::production().auth_token(sample_token()).build().expect("build");
        assert_eq!(cfg.service_url.as_str(), "wss://th.smoo.ai/tunnel");
        // `url::Url` normalizes to add a trailing slash when the
        // path is empty — don't pin the exact string, just the host.
        assert_eq!(cfg.local_target.host_str(), Some("127.0.0.1"));
        assert_eq!(cfg.local_target.port(), Some(4400));
        assert!(matches!(cfg.slug, SlugPreference::Ephemeral));
        assert!(cfg.user_agent.starts_with("th-tunnel/"));
    }

    #[test]
    fn missing_service_url_rejected() {
        let err = TunnelConfig::builder()
            .local_target(Url::parse("http://127.0.0.1:4400").unwrap())
            .auth_token(sample_token())
            .build()
            .unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("service_url")));
    }

    #[test]
    fn missing_local_target_rejected() {
        let err = TunnelConfig::builder()
            .service_url(Url::parse("wss://th.smoo.ai/tunnel").unwrap())
            .auth_token(sample_token())
            .build()
            .unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("local_target")));
    }

    #[test]
    fn missing_auth_token_rejected() {
        let err = TunnelConfig::production().build().unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("auth_token")));
    }

    #[test]
    fn empty_auth_token_rejected() {
        let err = TunnelConfig::production().auth_token("   ").build().unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("auth_token")));
    }

    #[test]
    fn wrong_service_scheme_rejected() {
        let err = TunnelConfig::production()
            .service_url(Url::parse("http://th.smoo.ai/tunnel").unwrap())
            .auth_token(sample_token())
            .build()
            .unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("ws://")));
    }

    #[test]
    fn wrong_local_scheme_rejected() {
        let err = TunnelConfig::production()
            .local_target(Url::parse("wss://127.0.0.1:4400").unwrap())
            .auth_token(sample_token())
            .build()
            .unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("http://")));
    }

    #[test]
    fn invalid_slug_preference_rejected() {
        let err = TunnelConfig::production()
            .auth_token(sample_token())
            .slug(SlugPreference::Requested("-bad".into()))
            .build()
            .unwrap_err();
        assert!(matches!(err, TunnelError::InvalidSlug(_)));
    }

    #[test]
    fn wire_method_maps_known_and_falls_back() {
        use smooai_fetch::Method;
        assert!(matches!(wire_method("post"), Method::POST));
        assert!(matches!(wire_method("DELETE"), Method::DELETE));
        assert!(matches!(wire_method("get"), Method::GET));
        assert!(matches!(wire_method("BREW"), Method::GET), "unknown → GET fallback");
    }

    #[test]
    fn build_request_init_strips_host_and_maps_body() {
        let pending = PendingHttp {
            open: StreamOpen {
                stream_id: 1,
                kind: StreamKind::Http,
                method: "POST".into(),
                path: "/x".into(),
                headers: vec![("Host".into(), "pub.th.smoo.ai".into()), ("Content-Type".into(), "application/json".into())],
            },
            body: b"{\"a\":1}".to_vec(),
        };
        let init = build_request_init(&pending);
        assert!(matches!(init.method, smooai_fetch::Method::POST));
        assert!(!init.headers.keys().any(|k| k.eq_ignore_ascii_case("host")), "Host stripped");
        assert_eq!(init.headers.get("Content-Type").map(String::as_str), Some("application/json"));
        assert_eq!(init.body.as_deref(), Some("{\"a\":1}"));

        // Empty body → None (a bodyless GET must not send an empty string body).
        let getp = PendingHttp {
            open: StreamOpen {
                stream_id: 2,
                kind: StreamKind::Http,
                method: "GET".into(),
                path: "/".into(),
                headers: vec![],
            },
            body: vec![],
        };
        assert_eq!(build_request_init(&getp).body, None);
    }

    #[test]
    fn local_request_url_joins_path_and_query_onto_base() {
        let base = Url::parse("http://127.0.0.1:4400").unwrap();
        assert_eq!(
            local_request_url(&base, "/api/tasks?foo=1").unwrap().as_str(),
            "http://127.0.0.1:4400/api/tasks?foo=1"
        );
        assert_eq!(local_request_url(&base, "/").unwrap().as_str(), "http://127.0.0.1:4400/");
        // A base with its own path prefix is replaced by the absolute path.
        let based = Url::parse("http://127.0.0.1:4400/ignored").unwrap();
        assert_eq!(local_request_url(&based, "/health").unwrap().as_str(), "http://127.0.0.1:4400/health");
    }

    #[tokio::test]
    async fn dial_failure_is_a_transport_error() {
        // Nothing listening on this port → connect_async fails.
        let cfg = TunnelConfig::production()
            .service_url(Url::parse("ws://127.0.0.1:1/tunnel").unwrap())
            .auth_token(sample_token())
            .build()
            .expect("build");
        let client = TunnelClient::new(cfg);
        let err = client.run(|_| {}).await.unwrap_err();
        assert!(matches!(err, TunnelError::Transport(_)), "got {err:?}");
    }

    // Handshake + HTTP proxy round-trip against an in-process mock
    // rendezvous + mock local target. Proves dial → ClientHello →
    // ServerHello → StreamOpen → local request → response frames.
    #[tokio::test]
    async fn handshake_and_http_proxy_round_trip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio_tungstenite::tungstenite::Message;

        // Mock local Big Smooth: one-shot HTTP server that echoes a fixed body.
        let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = local.local_addr().unwrap();
        let local_task = tokio::spawn(async move {
            let (mut sock, _) = local.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")
                .await
                .unwrap();
            sock.flush().await.unwrap();
            req
        });

        // Mock rendezvous: accept WS, expect ClientHello, send ServerHello,
        // send a StreamOpen(GET /ping), then read back the proxied response.
        let rendezvous = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let rv_addr = rendezvous.local_addr().unwrap();
        let rv_task = tokio::spawn(async move {
            let (sock, _) = rendezvous.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();

            let Message::Text(t) = ws.next().await.unwrap().unwrap() else {
                panic!("want ClientHello text")
            };
            assert!(t.contains("client_hello"), "first frame is ClientHello: {t}");

            let hello = WireFrame::ServerHello(ServerHello {
                protocol_version: PROTOCOL_VERSION,
                assigned_slug: "scratch-abcd".into(),
                public_url: "https://scratch-abcd.th.smoo.ai/".into(),
                session_id: "sess_test".into(),
            });
            ws.send(Message::Text(serde_json::to_string(&hello).unwrap().into())).await.unwrap();

            let open = WireFrame::StreamOpen(StreamOpen {
                stream_id: 7,
                kind: StreamKind::Http,
                method: "GET".into(),
                path: "/ping".into(),
                headers: vec![("Host".into(), "scratch-abcd.th.smoo.ai".into())],
            });
            ws.send(Message::Text(serde_json::to_string(&open).unwrap().into())).await.unwrap();
            ws.send(Message::Text(
                serde_json::to_string(&WireFrame::StreamClose(StreamClose {
                    stream_id: 7,
                    reason_code: "eom".into(),
                    reason: String::new(),
                }))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

            // Read back: StreamData(text) + binary body + StreamClose.
            let Message::Text(sd) = ws.next().await.unwrap().unwrap() else {
                panic!("want StreamData")
            };
            assert!(sd.contains("stream_data") && sd.contains("\"final_chunk\":true"), "response StreamData: {sd}");
            let Message::Binary(body) = ws.next().await.unwrap().unwrap() else {
                panic!("want body binary")
            };
            assert_eq!(&body[..], b"hello");
            let Message::Text(sc) = ws.next().await.unwrap().unwrap() else {
                panic!("want StreamClose")
            };
            assert!(sc.contains("stream_close"), "response close: {sc}");

            ws.send(Message::Text(serde_json::to_string(&WireFrame::Bye { reason: "done".into() }).unwrap().into()))
                .await
                .unwrap();
        });

        let cfg = TunnelConfig::production()
            .service_url(Url::parse(&format!("ws://{rv_addr}/tunnel")).unwrap())
            .local_target(Url::parse(&format!("http://{local_addr}")).unwrap())
            .auth_token(sample_token())
            .build()
            .expect("build");
        let client = TunnelClient::new(cfg);

        let mut ready_url = None;
        client.run(|h| ready_url = Some(h.public_url.clone())).await.expect("run");
        assert_eq!(ready_url.as_deref(), Some("https://scratch-abcd.th.smoo.ai/"));

        let proxied_req = local_task.await.unwrap();
        assert!(proxied_req.starts_with("GET /ping "), "local got the proxied request: {proxied_req}");
        assert!(
            !proxied_req.to_lowercase().contains("host: scratch-abcd"),
            "public-URL Host must be stripped: {proxied_req}"
        );
        rv_task.await.unwrap();
    }
}

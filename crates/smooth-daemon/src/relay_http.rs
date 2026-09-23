//! HTTP over the Smoo Relay (pearl th-a49e21): the small REST surface a
//! Big Smooth window needs from a daemon it drives on ANOTHER computer.
//!
//! The relay is a WebSocket fan-out, so the canonical operator protocol (chat)
//! already crosses it unchanged. The web SPA also reads a handful of REST
//! routes, though: `/cd` and `/pwd`, the Plan/Auto mode, the Stats page, the
//! `@`-mention search, the safety-judge settings. Pointed at a remote computer,
//! those must reach THAT computer — `/cd` resolving against the laptop while the
//! agent runs on smoo-hub would be worse than not having it.
//!
//! So a request travels as one relay frame on its own channel:
//!
//! ```text
//! window → local daemon → relay → remote daemon:
//!   {"channel":"http","type":"http.request","id":"…","method":"GET",
//!    "path":"/api/session/cwd?session=abc","body":null}
//! remote daemon → relay → local daemon → window:
//!   {"channel":"http","type":"http.response","id":"…","status":200,
//!    "content_type":"application/json","body":"{…}"}
//! ```
//!
//! The remote daemon answers it by calling ITS OWN loopback server with its
//! own local token — the same seam the per-phone operator bridge uses. The
//! caller's local token never crosses the relay.
//!
//! **What may be asked.** Only the exact `(method, path)` pairs in
//! [`ALLOWED`]: the routes the SPA's chat surface reads. Nothing under
//! `/api/flow` (flow sessions are shells, with their own end-to-end encrypted
//! channel), `/api/relay` (no onward hops through a second daemon), `/auth` or
//! `/push` (sign-in and notifications belong to the computer you are sitting
//! at). An exact match also rules out every traversal trick: `/api/stats/..`,
//! `%2e%2e`, and `//host` are simply not on the list. The relay only ever
//! connects devices of the SAME Smoo user — a peer that can send this frame can
//! already drive the operator itself — so the allowlist narrows the surface
//! rather than guarding a privilege boundary; it is still checked on both ends.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Semaphore;

/// The frame channel that marks an HTTP-over-relay frame.
pub const CHANNEL: &str = "http";
/// A request frame's `type`.
pub const REQUEST_TYPE: &str = "http.request";
/// A response frame's `type`.
pub const RESPONSE_TYPE: &str = "http.response";
/// Cap on a request or response body. The SPA's routes answer in kilobytes; a
/// megabyte is generous and keeps one frame from ballooning on the relay.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Cap on a request id — it is echoed back, so it must not carry a payload.
const MAX_ID_CHARS: usize = 64;
/// Cap on the path + query.
const MAX_PATH_CHARS: usize = 2048;
/// How long the remote daemon waits on its own loopback server.
const LOOPBACK_TIMEOUT: Duration = Duration::from_secs(20);
/// How many relay HTTP requests one daemon serves at once. Anything past it is
/// answered 503 instead of queueing without bound.
const MAX_CONCURRENT: usize = 16;

/// Every `(method, path)` a relay peer may ask for. Exact match, path only
/// (the query string is passed through).
pub const ALLOWED: &[(&str, &str)] = &[
    ("GET", "/admin/me"),
    ("GET", "/admin/model-costs"),
    ("GET", "/api/session/cwd"),
    ("POST", "/api/session/cwd"),
    ("GET", "/api/session/mode"),
    ("POST", "/api/session/mode"),
    ("GET", "/api/stats"),
    ("POST", "/api/usage"),
    ("GET", "/api/judge"),
    ("POST", "/api/judge"),
    ("GET", "/search"),
    ("GET", "/api/mode"),
    ("GET", "/api/skills"),
    ("GET", "/api/plugins"),
    ("GET", "/api/model-catalog"),
    ("GET", "/api/llm/provider"),
];

/// Whether `(method, path_and_query)` is on the allowlist. The query string is
/// not part of the decision, but a path that carries a fragment, whitespace or
/// a control character is refused outright.
#[must_use]
pub fn allowed(method: &str, path_and_query: &str) -> bool {
    if path_and_query.len() > MAX_PATH_CHARS || path_and_query.chars().any(|c| c.is_control() || c.is_whitespace() || c == '#' || c == '\\') {
        return false;
    }
    let path = path_and_query.split_once('?').map_or(path_and_query, |(p, _)| p);
    ALLOWED.iter().any(|(m, p)| *m == method && *p == path)
}

/// Rebuild a query string without the caller's local token. The window
/// authenticates to ITS daemon with `?token=`; that value is meaningless to —
/// and must never reach — another computer. Every other pair passes through
/// with its encoding untouched.
#[must_use]
pub fn strip_token(query: Option<&str>) -> Option<String> {
    let kept: Vec<&str> = query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| pair.split_once('=').map_or(*pair, |(k, _)| k) != "token")
        .collect();
    (!kept.is_empty()).then(|| kept.join("&"))
}

/// A validated request, as the remote daemon executes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: String,
    pub method: String,
    pub path: String,
    pub body: Option<String>,
}

/// Build the wire frame for a request (the local daemon's side).
#[must_use]
pub fn request_frame(id: &str, method: &str, path: &str, body: Option<&str>) -> Value {
    json!({ "channel": CHANNEL, "type": REQUEST_TYPE, "id": id, "method": method, "path": path, "body": body })
}

/// Whether a relayed frame belongs on this channel.
#[must_use]
pub fn is_http_frame(frame: &Value) -> bool {
    frame.get("channel").and_then(Value::as_str) == Some(CHANNEL)
}

/// Why a request frame was refused, as the status + message the caller sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub id: Option<String>,
    pub status: u16,
    pub message: String,
}

/// Validate a request frame (the remote daemon's side). Pure.
///
/// # Errors
/// A [`Refusal`] naming what was wrong — malformed frames get a 400, a
/// well-formed request for a route that is not on the list a 403.
pub fn parse_request(frame: &Value) -> Result<Request, Refusal> {
    let id = frame
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.chars().count() <= MAX_ID_CHARS && !id.chars().any(char::is_control))
        .map(str::to_string);
    let refuse = |status: u16, message: &str| Refusal {
        id: id.clone(),
        status,
        message: message.to_string(),
    };
    if frame.get("type").and_then(Value::as_str) != Some(REQUEST_TYPE) {
        return Err(refuse(400, "not an http.request frame"));
    }
    let Some(id_ok) = id.clone() else {
        return Err(refuse(400, "http.request needs a short string id"));
    };
    let method = frame.get("method").and_then(Value::as_str).unwrap_or_default().to_ascii_uppercase();
    if method != "GET" && method != "POST" {
        return Err(refuse(405, "only GET and POST cross the relay"));
    }
    let Some(path) = frame.get("path").and_then(Value::as_str).filter(|p| p.starts_with('/') && !p.starts_with("//")) else {
        return Err(refuse(400, "http.request needs an absolute path"));
    };
    if !allowed(&method, path) {
        return Err(refuse(403, "that route is not available over the relay"));
    }
    let body = match frame.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.len() <= MAX_BODY_BYTES => Some(s.clone()),
        Some(Value::String(_)) => return Err(refuse(413, "request body too large for the relay")),
        Some(_) => return Err(refuse(400, "http.request body must be a string")),
    };
    Ok(Request {
        id: id_ok,
        method,
        path: path.to_string(),
        body,
    })
}

/// Build the wire frame for a response. Oversized bodies are replaced by a
/// 502 rather than truncated into invalid JSON.
#[must_use]
pub fn response_frame(id: Option<&str>, status: u16, content_type: &str, body: &str) -> Value {
    if body.len() > MAX_BODY_BYTES {
        return error_frame(id, 502, "the response is too large to send over the relay");
    }
    json!({ "channel": CHANNEL, "type": RESPONSE_TYPE, "id": id, "status": status, "content_type": content_type, "body": body })
}

/// A JSON `{error}` response frame.
#[must_use]
pub fn error_frame(id: Option<&str>, status: u16, message: &str) -> Value {
    json!({
        "channel": CHANNEL,
        "type": RESPONSE_TYPE,
        "id": id,
        "status": status,
        "content_type": "application/json",
        "body": json!({ "error": message }).to_string(),
    })
}

/// A response as the local daemon hands it back to the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

/// Read a response frame (the local daemon's side): `(id, response)`, or
/// `None` for anything that is not a well-formed `http.response`.
#[must_use]
pub fn parse_response(frame: &Value) -> Option<(String, Response)> {
    if !is_http_frame(frame) || frame.get("type").and_then(Value::as_str) != Some(RESPONSE_TYPE) {
        return None;
    }
    let id = frame.get("id").and_then(Value::as_str)?.to_string();
    let status = frame
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok())
        .filter(|s| (100..=599).contains(s))?;
    Some((
        id,
        Response {
            status,
            content_type: frame
                .get("content_type")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream")
                .to_string(),
            body: frame.get("body").and_then(Value::as_str).unwrap_or_default().to_string(),
        },
    ))
}

/// The remote daemon's side: answers relay HTTP frames against its own
/// loopback server. Cheap to clone.
#[derive(Clone)]
pub struct Server {
    base: Arc<String>,
    token: Arc<String>,
    client: reqwest::Client,
    slots: Arc<Semaphore>,
}

impl Server {
    /// `base` is the daemon's own `http://127.0.0.1:<port>`; `token` its local token.
    #[must_use]
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        // No proxy: the daemon may export HTTP(S)_PROXY for its sandbox's egress
        // boundary, and a loopback call must never be sent through it.
        let client = reqwest::Client::builder().no_proxy().timeout(LOOPBACK_TIMEOUT).build().unwrap_or_default();
        Self {
            base: Arc::new(base.into()),
            token: Arc::new(token.into()),
            client,
            slots: Arc::new(Semaphore::new(MAX_CONCURRENT)),
        }
    }

    /// Answer one request frame. Never fails: every problem is a response
    /// frame the caller can show.
    pub async fn answer(&self, frame: &Value) -> Value {
        let req = match parse_request(frame) {
            Ok(r) => r,
            Err(r) => return error_frame(r.id.as_deref(), r.status, &r.message),
        };
        let Ok(_permit) = self.slots.try_acquire() else {
            return error_frame(Some(&req.id), 503, "this computer is busy answering relay requests; try again");
        };
        let url = format!("{}{}", self.base, req.path);
        // Defence in depth: whatever the path did, the call stays on loopback.
        match reqwest::Url::parse(&url) {
            Ok(u) if u.host_str() == Some("127.0.0.1") && self.base.ends_with(&format!(":{}", u.port().unwrap_or(0))) => {}
            _ => return error_frame(Some(&req.id), 400, "bad path"),
        }
        let mut call = if req.method == "POST" {
            self.client.post(&url)
        } else {
            self.client.get(&url)
        }
        .bearer_auth(self.token.as_str());
        if let Some(body) = req.body {
            call = call.header("content-type", "application/json").body(body);
        }
        match call.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("application/octet-stream")
                    .to_string();
                match resp.bytes().await {
                    Ok(bytes) => response_frame(Some(&req.id), status, &content_type, &String::from_utf8_lossy(&bytes)),
                    Err(e) => error_frame(Some(&req.id), 502, &format!("reading this computer's reply failed: {e}")),
                }
            }
            Err(e) => error_frame(Some(&req.id), 502, &format!("this computer's daemon did not answer: {e}")),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn allowlist_is_exact_on_method_and_path() {
        assert!(allowed("GET", "/api/session/cwd?session=abc"));
        assert!(allowed("POST", "/api/session/cwd"));
        assert!(allowed("GET", "/search?q=src%2Fmain"));
        assert!(!allowed("DELETE", "/api/session/cwd"), "method is part of the match");
        assert!(!allowed("POST", "/api/stats"), "stats is read-only over the relay");
        assert!(!allowed("GET", "/api/stats/"), "a trailing slash is a different path");
        assert!(!allowed("GET", "/API/STATS"), "case matters");
    }

    #[test]
    fn allowlist_refuses_everything_that_is_not_the_chat_surface() {
        for path in [
            "/api/flow/sessions",
            "/api/flow/ws",
            "/api/flow/pair",
            "/api/relay/peers",
            "/api/relay/peers/daemon-x/api/stats",
            "/auth/login",
            "/api/auth/status",
            "/push/subscribe",
            "/ws",
            "/",
            "/admin/config",
        ] {
            assert!(!allowed("GET", path), "{path} must not cross the relay");
            assert!(!allowed("POST", path), "{path} must not cross the relay");
        }
    }

    #[test]
    fn allowlist_refuses_traversal_and_smuggling() {
        for path in [
            "/api/stats/../flow/sessions",
            "/api/stats/%2e%2e/flow",
            "/api/%73tats",
            "//evil.example/api/stats",
            "/api/stats#frag",
            "/api/stats?q=a b",
            "/api/stats?q=\n",
            "/api\\stats",
            "api/stats",
            "",
        ] {
            assert!(!allowed("GET", path), "{path:?} must be refused");
        }
        let long = format!("/search?q={}", "a".repeat(MAX_PATH_CHARS));
        assert!(!allowed("GET", &long), "an oversized path is refused");
    }

    #[test]
    fn strip_token_removes_only_the_local_token() {
        assert_eq!(strip_token(Some("token=secret")), None);
        assert_eq!(strip_token(Some("session=a&token=secret")).as_deref(), Some("session=a"));
        assert_eq!(strip_token(Some("token=secret&q=x%20y&token=again")).as_deref(), Some("q=x%20y"));
        assert_eq!(strip_token(Some("tokenish=keep&x=token")).as_deref(), Some("tokenish=keep&x=token"));
        assert_eq!(strip_token(Some("token")), None, "a bare key is still the token");
        assert_eq!(strip_token(None), None);
        assert_eq!(strip_token(Some("")), None);
        assert_eq!(strip_token(Some("&&a=1&")).as_deref(), Some("a=1"));
    }

    #[test]
    fn request_frames_round_trip() {
        let frame = request_frame("r1", "POST", "/api/session/cwd", Some(r#"{"path":"/tmp"}"#));
        assert!(is_http_frame(&frame));
        let req = parse_request(&frame).unwrap();
        assert_eq!(
            req,
            Request {
                id: "r1".into(),
                method: "POST".into(),
                path: "/api/session/cwd".into(),
                body: Some(r#"{"path":"/tmp"}"#.into()),
            }
        );
        let get = parse_request(&request_frame("r2", "get", "/api/stats", None)).unwrap();
        assert_eq!(get.method, "GET", "method is normalised");
        assert_eq!(get.body, None);
    }

    #[test]
    fn malformed_requests_are_refused_with_a_reason() {
        let cases = [
            (json!({"channel":"http","type":"http.response","id":"a"}), 400),
            (json!({"channel":"http","type":"http.request","method":"GET","path":"/api/stats"}), 400),
            (json!({"channel":"http","type":"http.request","id":"","method":"GET","path":"/api/stats"}), 400),
            (
                json!({"channel":"http","type":"http.request","id":"x".repeat(65),"method":"GET","path":"/api/stats"}),
                400,
            ),
            (
                json!({"channel":"http","type":"http.request","id":"a\nb","method":"GET","path":"/api/stats"}),
                400,
            ),
            (json!({"channel":"http","type":"http.request","id":"a","method":"PUT","path":"/api/stats"}), 405),
            (json!({"channel":"http","type":"http.request","id":"a","method":"GET","path":"api/stats"}), 400),
            (
                json!({"channel":"http","type":"http.request","id":"a","method":"GET","path":"//x/api/stats"}),
                400,
            ),
            (
                json!({"channel":"http","type":"http.request","id":"a","method":"GET","path":"/api/flow/sessions"}),
                403,
            ),
            (
                json!({"channel":"http","type":"http.request","id":"a","method":"POST","path":"/api/usage","body":{"x":1}}),
                400,
            ),
            (
                json!({"channel":"http","type":"http.request","id":"a","method":"POST","path":"/api/usage","body":"x".repeat(MAX_BODY_BYTES + 1)}),
                413,
            ),
        ];
        for (frame, status) in cases {
            let r = parse_request(&frame).unwrap_err();
            assert_eq!(r.status, status, "{frame}");
            assert!(!r.message.is_empty());
        }
        // A refusal echoes a usable id so the caller's request resolves.
        assert_eq!(
            parse_request(&json!({"channel":"http","type":"http.request","id":"a","method":"GET","path":"/auth/login"}))
                .unwrap_err()
                .id
                .as_deref(),
            Some("a")
        );
    }

    #[test]
    fn responses_round_trip_and_junk_is_ignored() {
        let (id, resp) = parse_response(&response_frame(Some("r1"), 200, "application/json", r#"{"cwd":"/x"}"#)).unwrap();
        assert_eq!(id, "r1");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, r#"{"cwd":"/x"}"#);
        let (_, err) = parse_response(&error_frame(Some("r2"), 403, "nope")).unwrap();
        assert_eq!(err.status, 403);
        assert!(err.body.contains("nope"));
        for junk in [
            json!({"type":"http.response","id":"a","status":200}),
            json!({"channel":"http","type":"http.request","id":"a"}),
            json!({"channel":"http","type":"http.response","status":200}),
            json!({"channel":"http","type":"http.response","id":"a","status":42}),
            json!({"channel":"http","type":"http.response","id":"a","status":"200"}),
            json!({"type":"stream_token","token":"hi"}),
        ] {
            assert!(parse_response(&junk).is_none(), "{junk}");
        }
    }

    #[test]
    fn an_oversized_response_becomes_a_502_not_broken_json() {
        let frame = response_frame(Some("r"), 200, "text/plain", &"x".repeat(MAX_BODY_BYTES + 1));
        let (_, resp) = parse_response(&frame).unwrap();
        assert_eq!(resp.status, 502);
    }

    /// A real loopback server: the answer carries the route's reply, the
    /// daemon's own token (never the caller's) authenticates the call, and a
    /// refused route never reaches the server at all.
    #[tokio::test]
    async fn server_answers_from_its_own_loopback_with_its_own_token() {
        use axum::http::HeaderMap;
        use axum::routing::get;
        use axum::Router;

        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_in = seen.clone();
        let app = Router::new()
            .route(
                "/api/session/cwd",
                get(move |headers: HeaderMap, uri: axum::http::Uri| {
                    let seen = seen_in.clone();
                    async move {
                        let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
                        seen.lock().unwrap().push(format!("{auth} {uri}"));
                        axum::Json(json!({"cwd": "/srv/project"}))
                    }
                }),
            )
            .route("/auth/login", get(|| async { "should never be called" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let server = Server::new(format!("http://{addr}"), "remote-local-token");
        let reply = server.answer(&request_frame("r1", "GET", "/api/session/cwd?session=abc", None)).await;
        let (id, resp) = parse_response(&reply).unwrap();
        assert_eq!(id, "r1");
        assert_eq!(resp.status, 200);
        assert!(resp.content_type.starts_with("application/json"));
        assert_eq!(serde_json::from_str::<Value>(&resp.body).unwrap()["cwd"], "/srv/project");
        assert_eq!(seen.lock().unwrap().as_slice(), ["Bearer remote-local-token /api/session/cwd?session=abc"]);

        let refused = server.answer(&request_frame("r2", "GET", "/auth/login", None)).await;
        assert_eq!(parse_response(&refused).unwrap().1.status, 403);
        assert_eq!(seen.lock().unwrap().len(), 1, "a refused route never reaches the server");
    }

    #[tokio::test]
    async fn server_reports_a_dead_loopback_as_502() {
        // Nothing listens on port 1.
        let server = Server::new("http://127.0.0.1:1", "t");
        let reply = server.answer(&request_frame("r", "GET", "/api/stats", None)).await;
        assert_eq!(parse_response(&reply).unwrap().1.status, 502);
    }
}

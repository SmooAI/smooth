//! Which model actually served each call? (SMOODEV-4125)
//!
//! llm.smoo.ai is a LiteLLM proxy with fallbacks configured. When the
//! OpenAI account behind it ran out of credits, every `gpt-6-luna` call
//! was answered by `groq-gpt-oss-120b` instead — and still came back as a
//! 200. A bench that only reads the status would publish Groq's score
//! under Luna's name.
//!
//! The obvious tells are not enough on their own. Probed 2026-10-26:
//!
//! * non-streaming: `x-litellm-model-group: groq-gpt-oss-120b` and
//!   `x-litellm-attempted-fallbacks: 1` say so, and the body's `model` is
//!   `openai/gpt-oss-120b`;
//! * streaming, even with `disable_fallbacks: true`: the headers say
//!   `gpt-6-luna` / `0` — they go out before the upstream fails — and the
//!   fallback happens MID-STREAM. Only the chunks' `model` field
//!   (`groq-gpt-oss-120b`) gives it away;
//! * a fallback can then be answered from the fallback group's response
//!   cache, so even a fresh-looking answer may not be a fresh call.
//!
//! So the agent's gateway traffic goes through [`ServedGuard`], a local
//! pass-through proxy that asks for no fallbacks and no cache on every
//! call, and records the model group header AND every `model` the
//! response body names. A trial during which any call was served by
//! something other than what it asked for is INVALID, not scored.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::Response;
use futures_util::StreamExt;
use serde_json::Value;

/// One LLM call as the guard saw it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServedCall {
    /// The `model` the caller asked for. `None` for calls without one
    /// (`/model/info`, `/key/info`), which are never a mismatch.
    pub requested: Option<String>,
    /// `x-litellm-model-group` — the group that took the request.
    pub model_group: Option<String>,
    /// `x-litellm-attempted-fallbacks`.
    pub attempted_fallbacks: u32,
    /// Every distinct `model` named in the response body: the top-level
    /// field of a JSON response, or of each SSE chunk, in first-seen order.
    pub served: Vec<String>,
}

impl ServedCall {
    /// Why this call was NOT served by the model it asked for, or `None`
    /// when nothing says otherwise.
    #[must_use]
    pub fn mismatch(&self) -> Option<String> {
        let requested = self.requested.as_deref()?;
        if self.attempted_fallbacks > 0 {
            let by = self.model_group.as_deref().unwrap_or("an unnamed fallback");
            return Some(format!("asked for {requested}, the gateway fell back to {by}"));
        }
        if let Some(group) = self.model_group.as_deref().filter(|g| !same_model(requested, g)) {
            return Some(format!("asked for {requested}, served by model group {group}"));
        }
        self.served
            .iter()
            .find(|s| !same_model(requested, s))
            .map(|s| format!("asked for {requested}, the response came from {s}"))
    }
}

/// Whether `served` is a name for `requested`.
///
/// Providers report the model under a different spelling than the
/// gateway alias: a dated snapshot (`gpt-6-luna-2026-09-01`), a provider
/// prefix (`openai/gpt-oss-120b`), or the alias's upstream name inside it
/// (`groq-gpt-oss-120b` → `gpt-oss-120b`). Those agree; a different
/// model family does not.
#[must_use]
pub fn same_model(requested: &str, served: &str) -> bool {
    let norm = |s: &str| s.rsplit('/').next().unwrap_or(s).trim().to_ascii_lowercase();
    let (r, s) = (norm(requested), norm(served));
    !s.is_empty() && (r == s || s.starts_with(&r) || r.ends_with(&s))
}

/// Ask LiteLLM for exactly the model named: no fallback to another group,
/// no answer from the response cache. Returns the requested model.
///
/// Only touches a JSON object that names a `model`; anything else is
/// passed through as-is.
pub fn pin_request(body: &mut Value) -> Option<String> {
    let obj = body.as_object_mut()?;
    let model = obj.get("model")?.as_str()?.to_string();
    obj.insert("disable_fallbacks".into(), Value::Bool(true));
    obj.insert("cache".into(), serde_json::json!({"no-cache": true}));
    Some(model)
}

/// Read the gateway's routing headers into `call`.
pub fn read_headers(call: &mut ServedCall, headers: &HeaderMap) {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty());
    call.model_group = text("x-litellm-model-group").map(ToString::to_string);
    call.attempted_fallbacks = text("x-litellm-attempted-fallbacks").and_then(|v| v.parse().ok()).unwrap_or(0);
}

/// Fail a call the bench makes itself (judge, convo driver) when the
/// gateway served it with another model. `body_model` is the response's
/// top-level `model`.
///
/// # Errors
/// Errors on a mismatch, so callers' existing error path (INCONCLUSIVE)
/// applies — a judge that silently became a smaller model is no judge.
pub fn ensure_served(requested: &str, headers: &HeaderMap, body_model: Option<&str>) -> Result<()> {
    let mut call = ServedCall {
        requested: Some(requested.to_string()),
        ..ServedCall::default()
    };
    read_headers(&mut call, headers);
    call.served.extend(body_model.map(ToString::to_string));
    match call.mismatch() {
        Some(why) => anyhow::bail!("gateway did not serve the requested model: {why}"),
        None => Ok(()),
    }
}

/// Every call the guard has proxied, in order. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct ServedLedger(Arc<Mutex<Vec<ServedCall>>>);

impl ServedLedger {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<ServedCall>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Calls recorded so far — a mark to pass to [`Self::mismatch_since`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Record a call; returns its index for [`Self::add_served`].
    pub fn push(&self, call: ServedCall) -> usize {
        let mut v = self.lock();
        v.push(call);
        v.len() - 1
    }

    /// Note a `model` seen in call `idx`'s response body.
    pub fn add_served(&self, idx: usize, model: &str) {
        if let Some(c) = self.lock().get_mut(idx) {
            if !c.served.iter().any(|m| m == model) {
                c.served.push(model.to_string());
            }
        }
    }

    /// Distinct mismatch reasons among the calls recorded since `mark`.
    #[must_use]
    pub fn mismatch_since(&self, mark: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for why in self.lock().iter().skip(mark).filter_map(ServedCall::mismatch) {
            if !out.contains(&why) {
                out.push(why);
            }
        }
        out
    }
}

/// Pulls `model` out of a response body as it streams past.
///
/// SSE bodies are scanned line by line (`data: {…}`); a plain JSON body
/// is buffered (up to a cap) and read when the stream ends. Finishing
/// happens on drop, so a body the client abandons halfway is still read.
struct Sniffer {
    ledger: ServedLedger,
    idx: usize,
    sse: bool,
    buf: Vec<u8>,
}

/// A JSON completion past this is not worth buffering to find `model`.
const JSON_SNIFF_CAP: usize = 4 * 1024 * 1024;

impl Sniffer {
    fn feed(&mut self, chunk: &[u8]) {
        if !self.sse {
            if self.buf.len() < JSON_SNIFF_CAP {
                self.buf.extend_from_slice(chunk);
            }
            return;
        }
        self.buf.extend_from_slice(chunk);
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            self.sse_line(&line);
        }
    }

    fn sse_line(&self, line: &[u8]) {
        let line = String::from_utf8_lossy(line);
        let Some(data) = line.trim().strip_prefix("data:") else { return };
        if let Some(m) = serde_json::from_str::<Value>(data.trim()).ok().as_ref().and_then(body_model) {
            self.ledger.add_served(self.idx, m);
        }
    }
}

impl Drop for Sniffer {
    fn drop(&mut self) {
        let rest = std::mem::take(&mut self.buf);
        if self.sse {
            self.sse_line(&rest);
        } else if let Some(m) = serde_json::from_slice::<Value>(&rest).ok().as_ref().and_then(body_model) {
            self.ledger.add_served(self.idx, m);
        }
    }
}

/// The top-level `model` of a completion (or completion chunk).
#[must_use]
pub fn body_model(v: &Value) -> Option<&str> {
    v.get("model").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty())
}

#[derive(Clone)]
struct ProxyState {
    upstream: String,
    client: reqwest::Client,
    ledger: ServedLedger,
}

/// Hop-by-hop and length headers the proxy must not copy: it re-frames
/// both the request and the (streamed) response body.
fn is_hop_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host" | "content-length" | "transfer-encoding" | "connection" | "keep-alive" | "proxy-connection" | "te" | "trailer" | "upgrade"
    )
}

async fn forward(State(st): State<ProxyState>, req: Request) -> Response {
    match forward_inner(&st, req).await {
        Ok(r) => r,
        Err(e) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(format!("smooth-bench served-model guard: {e:#}")))
            .unwrap_or_default(),
    }
}

async fn forward_inner(st: &ProxyState, req: Request) -> Result<Response> {
    let (parts, body) = req.into_parts();
    let path_q = parts.uri.path_and_query().map_or("/", axum::http::uri::PathAndQuery::as_str);
    let url = format!("{}{path_q}", st.upstream.trim_end_matches('/'));
    let mut bytes = axum::body::to_bytes(body, usize::MAX).await.context("reading the request body")?;

    let mut call = ServedCall::default();
    if let Ok(mut v) = serde_json::from_slice::<Value>(&bytes) {
        if let Some(model) = pin_request(&mut v) {
            call.requested = Some(model);
            bytes = Bytes::from(serde_json::to_vec(&v)?);
        }
    }

    let mut out = st.client.request(parts.method, &url);
    for (k, v) in &parts.headers {
        // Asking for an encoding the guard's client won't decode would hand
        // the sniffer compressed bytes it cannot read.
        if !is_hop_header(k) && k != axum::http::header::ACCEPT_ENCODING {
            out = out.header(k, v);
        }
    }
    let resp = out.body(bytes).send().await.with_context(|| format!("forwarding to {url}"))?;

    read_headers(&mut call, resp.headers());
    let sse = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream"));
    let idx = st.ledger.push(call);

    let mut builder = Response::builder().status(resp.status().as_u16());
    for (k, v) in resp.headers() {
        if !is_hop_header(k) {
            builder = builder.header(k, v);
        }
    }
    let mut sniffer = Sniffer {
        ledger: st.ledger.clone(),
        idx,
        sse,
        buf: Vec::new(),
    };
    let stream = resp.bytes_stream().map(move |chunk| {
        if let Ok(c) = &chunk {
            sniffer.feed(c);
        }
        chunk
    });
    Ok(builder.body(Body::from_stream(stream))?)
}

/// A running guard: point the agent at [`Self::url`] instead of the
/// gateway. Stops when dropped.
pub struct ServedGuard {
    pub url: String,
    pub ledger: ServedLedger,
    task: tokio::task::JoinHandle<()>,
}

impl ServedGuard {
    /// Serve on an ephemeral loopback port, forwarding to `upstream` (the
    /// gateway's OpenAI-compatible base, e.g. `https://llm.smoo.ai/v1`).
    ///
    /// # Errors
    /// Errors when the loopback listener cannot be bound.
    pub async fn start(upstream: &str) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .context("binding the served-model guard")?;
        let addr = listener.local_addr()?;
        let ledger = ServedLedger::default();
        let state = ProxyState {
            upstream: upstream.trim_end_matches('/').to_string(),
            // The Cloudflare in front of the gateway rejects requests with
            // no User-Agent (error 1010); the daemon's own is forwarded.
            client: reqwest::Client::builder().user_agent("smooth-bench").build()?,
            ledger: ledger.clone(),
        };
        let app = axum::Router::new().fallback(forward).with_state(state);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        // The guard's root stands for the upstream base: a client that
        // appends `/chat/completions` to the guard's URL reaches
        // `<upstream>/chat/completions`.
        Ok(Self {
            url: format!("http://{addr}"),
            ledger,
            task,
        })
    }
}

impl Drop for ServedGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(requested: &str, group: Option<&str>, fallbacks: u32, served: &[&str]) -> ServedCall {
        ServedCall {
            requested: Some(requested.into()),
            model_group: group.map(Into::into),
            attempted_fallbacks: fallbacks,
            served: served.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn same_model_accepts_provider_spellings_of_the_alias() {
        assert!(same_model("gpt-6-luna", "gpt-6-luna"));
        assert!(same_model("gpt-6-luna", "gpt-6-luna-2026-09-01"), "dated snapshot");
        assert!(same_model("gpt-6-luna", "openai/gpt-6-luna"), "provider prefix");
        assert!(same_model("groq-gpt-oss-120b", "openai/gpt-oss-120b"), "alias wraps the upstream name");
        assert!(same_model("claude-opus-5-5", "CLAUDE-OPUS-5-5"));
    }

    #[test]
    fn same_model_rejects_another_model() {
        assert!(!same_model("gpt-6-luna", "groq-gpt-oss-120b"));
        assert!(!same_model("gpt-6-luna", "openai/gpt-oss-120b"));
        assert!(!same_model("groq-gpt-oss-20b", "openai/gpt-oss-120b"), "a sibling size is not the same model");
        assert!(!same_model("gpt-6-luna", ""), "a blank name proves nothing either way");
    }

    #[test]
    fn a_clean_call_is_not_a_mismatch() {
        assert_eq!(call("gpt-6-luna", Some("gpt-6-luna"), 0, &["gpt-6-luna-2026-09-01"]).mismatch(), None);
        assert_eq!(call("gpt-6-luna", None, 0, &[]).mismatch(), None, "no evidence is not a mismatch");
        let unnamed = ServedCall {
            model_group: Some("groq-gpt-oss-120b".into()),
            ..ServedCall::default()
        };
        assert_eq!(unnamed.mismatch(), None, "a call that asked for no model cannot be mis-served");
    }

    #[test]
    fn the_non_streaming_fallback_is_caught_by_its_headers() {
        // Observed 2026-10-26 with the OpenAI account out of credits.
        let c = call("gpt-6-luna", Some("groq-gpt-oss-120b"), 1, &["openai/gpt-oss-120b"]);
        let why = c.mismatch().expect("a fallback is a mismatch");
        assert!(why.contains("gpt-6-luna") && why.contains("groq-gpt-oss-120b"), "{why}");
    }

    #[test]
    fn the_mid_stream_fallback_is_caught_by_the_body_alone() {
        // Headers claim Luna and zero fallbacks; only the chunks say Groq.
        let c = call("gpt-6-luna", Some("gpt-6-luna"), 0, &["groq-gpt-oss-120b"]);
        let why = c.mismatch().expect("a mid-stream fallback is a mismatch");
        assert!(why.contains("groq-gpt-oss-120b"), "{why}");
    }

    #[test]
    fn pin_request_disables_fallbacks_and_the_cache() {
        let mut v = serde_json::json!({"model": "gpt-6-luna", "stream": true, "messages": []});
        assert_eq!(pin_request(&mut v).as_deref(), Some("gpt-6-luna"));
        assert_eq!(v["disable_fallbacks"], true);
        assert_eq!(v["cache"]["no-cache"], true);
        assert_eq!(v["stream"], true, "the rest of the request is untouched");

        let mut no_model = serde_json::json!({"messages": []});
        assert_eq!(pin_request(&mut no_model), None);
        assert!(no_model.get("disable_fallbacks").is_none(), "only model calls are pinned");
        assert_eq!(pin_request(&mut serde_json::json!([1, 2])), None);
    }

    #[test]
    fn ensure_served_rejects_a_judge_answered_by_another_model() {
        let mut h = HeaderMap::new();
        h.insert("x-litellm-model-group", "claude-opus-5-5".parse().unwrap());
        h.insert("x-litellm-attempted-fallbacks", "0".parse().unwrap());
        assert!(ensure_served("claude-opus-5-5", &h, Some("claude-opus-5-5")).is_ok());
        assert!(ensure_served("claude-opus-5-5", &h, Some("groq-gpt-oss-120b")).is_err());
        assert!(
            ensure_served("claude-opus-5-5", &HeaderMap::new(), None).is_ok(),
            "a gateway that sends nothing is not accused"
        );
    }

    #[test]
    fn ledger_reports_mismatches_only_after_the_mark() {
        let l = ServedLedger::default();
        l.push(call("gpt-6-luna", Some("groq-gpt-oss-120b"), 1, &[]));
        let mark = l.len();
        assert!(l.mismatch_since(mark).is_empty(), "an earlier trial's fallback is not this trial's");
        let i = l.push(call("gpt-6-luna", Some("gpt-6-luna"), 0, &[]));
        l.add_served(i, "groq-gpt-oss-120b");
        l.add_served(i, "groq-gpt-oss-120b");
        assert_eq!(l.mismatch_since(mark).len(), 1, "reasons are deduplicated");
        assert_eq!(l.mismatch_since(0).len(), 2);
    }

    /// A stand-in gateway that always falls back mid-stream, like
    /// llm.smoo.ai with the OpenAI account out of credits.
    async fn fake_gateway(seen: Arc<Mutex<Vec<Value>>>) -> String {
        use axum::routing::post;
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move |body: axum::Json<Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(body.0.clone());
                    let sse = "data: {\"model\":\"groq-gpt-oss-120b\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                               data: {\"model\":\"groq-gpt-oss-120b\",\"choices\":[]}\n\ndata: [DONE]\n\n";
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .header("x-litellm-model-group", body.0["model"].as_str().unwrap_or_default())
                        .header("x-litellm-attempted-fallbacks", "0")
                        .body(Body::from(sse))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn guard_pins_the_request_streams_the_body_and_records_who_served_it() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let upstream = fake_gateway(seen.clone()).await;
        let guard = ServedGuard::start(&upstream).await.unwrap();

        let mark = guard.ledger.len();
        let resp = reqwest::Client::new()
            .post(format!("{}/chat/completions", guard.url))
            .json(&serde_json::json!({"model": "gpt-6-luna", "stream": true, "messages": []}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["x-litellm-model-group"], "gpt-6-luna", "upstream headers pass through");
        let text = resp.text().await.unwrap();
        assert!(text.contains("[DONE]"), "the body streams through intact: {text}");

        let sent = seen.lock().unwrap()[0].clone();
        assert_eq!(sent["disable_fallbacks"], true, "the gateway was asked not to fall back");
        assert_eq!(sent["cache"]["no-cache"], true);

        let why = guard.ledger.mismatch_since(mark);
        assert_eq!(why.len(), 1, "{why:?}");
        assert!(why[0].contains("groq-gpt-oss-120b"), "{why:?}");
    }

    #[tokio::test]
    async fn guard_passes_a_correctly_served_json_call_clean() {
        use axum::routing::post;
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Response::builder()
                    .header("content-type", "application/json")
                    .header("x-litellm-model-group", "claude-opus-5-5")
                    .body(Body::from(r#"{"model":"claude-opus-5-5","choices":[]}"#))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let guard = ServedGuard::start(&format!("http://{addr}/v1/")).await.unwrap();
        let body = reqwest::Client::new()
            .post(format!("{}/chat/completions", guard.url))
            .json(&serde_json::json!({"model": "claude-opus-5-5", "messages": []}))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(body.contains("claude-opus-5-5"));
        assert!(guard.ledger.mismatch_since(0).is_empty());
        assert_eq!(guard.ledger.len(), 1);
    }
}

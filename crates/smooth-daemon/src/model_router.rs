//! Loopback model router: per-model provider routing for the daemon's turns
//! (SMOODEV-4148).
//!
//! The daemon hosts smooth-operator's `LocalServer`, which holds ONE gateway
//! (url + key, OpenAI-compatible) and applies a per-turn `send_message.model`
//! by swapping only the model id. That is how Big Smooth's picker and `th code`'s
//! `/model` switch models — and it means a model the `llm.smoo.ai` gateway does
//! not serve (Claude Haiku 5.5) can never run, however it is picked.
//!
//! So when the user has a provider for a direct-route model
//! ([`smooth_policy::direct_models`]), the daemon points the `LocalServer` at
//! this router instead of the gateway. The router reads each request's `model`:
//! a direct-route model goes to its own provider's OpenAI-compatible endpoint
//! (fields that provider rejects removed), everything else is forwarded to the
//! real gateway byte for byte. Responses stream straight back, so SSE turns are
//! unchanged.
//!
//! The router listens on `127.0.0.1` only and requires a per-process random
//! bearer secret (the `LocalServer` sends it as the "gateway key"), so no other
//! local process can use it to spend the user's keys.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::Value;
use smooth_policy::direct_models::{DirectModel, DIRECT_MODELS};

/// Largest request body the router accepts. Turns carry the full conversation
/// plus inline images, so this is generous; it only bounds a runaway client.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// One LLM endpoint: an OpenAI-compatible base url (`…/v1`) and its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub api_url: String,
    pub api_key: String,
}

/// Where each model goes: the gateway, plus the direct-route models whose
/// provider is configured.
#[derive(Debug, Clone)]
pub struct RouteTable {
    pub upstream: Endpoint,
    pub direct: Vec<(&'static DirectModel, Endpoint)>,
}

impl RouteTable {
    /// Build the table from a parsed `providers.json`: every direct-route model
    /// whose provider has a key, and `upstream` for the rest.
    #[must_use]
    pub fn from_providers(providers: &Value, upstream: Endpoint) -> Self {
        let direct = DIRECT_MODELS
            .iter()
            .filter_map(|d| smooth_cast::providers::direct_provider_from_value(providers, d).map(|(api_url, api_key)| (d, Endpoint { api_url, api_key })))
            .collect();
        Self { upstream, direct }
    }

    /// Whether any model routes away from the gateway — the only case the
    /// router is worth starting for.
    #[must_use]
    pub fn has_direct(&self) -> bool {
        !self.direct.is_empty()
    }

    fn direct_for(&self, model: &str) -> Option<&(&'static DirectModel, Endpoint)> {
        let model = model.trim();
        self.direct.iter().find(|(d, _)| d.model.eq_ignore_ascii_case(model))
    }
}

/// The resolved destination of one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    pub url: String,
    pub api_key: String,
    pub body: Bytes,
}

/// Decide where a request goes. `path_and_query` is what the `LocalServer`
/// requested of the router (`/v1/chat/completions`); it is re-rooted on the
/// destination's base url. Only a parseable `chat/completions` body naming a
/// direct-route model leaves the gateway — anything else, including a body the
/// router cannot parse, is forwarded to the gateway untouched.
#[must_use]
pub fn plan(table: &RouteTable, path_and_query: &str, body: Bytes) -> Forward {
    let rest = path_and_query.strip_prefix("/v1").unwrap_or(path_and_query);
    if rest.starts_with("/chat/completions") {
        if let Ok(mut json) = serde_json::from_slice::<Value>(&body) {
            let direct = json.get("model").and_then(Value::as_str).and_then(|m| table.direct_for(m));
            if let (Some((model, endpoint)), Some(obj)) = (direct, json.as_object_mut()) {
                for field in model.strip_fields {
                    obj.remove(*field);
                }
                obj.insert("model".into(), Value::String(model.model.to_owned()));
                if let Ok(rewritten) = serde_json::to_vec(&json) {
                    return Forward {
                        url: join(&endpoint.api_url, rest),
                        api_key: endpoint.api_key.clone(),
                        body: Bytes::from(rewritten),
                    };
                }
            }
        }
    }
    Forward {
        url: join(&table.upstream.api_url, rest),
        api_key: table.upstream.api_key.clone(),
        body,
    }
}

fn join(base: &str, rest: &str) -> String {
    format!("{}{rest}", base.trim_end_matches('/'))
}

/// A running router: the base url and key to hand the `LocalServer` in place of
/// the gateway's.
#[derive(Debug, Clone)]
pub struct RouterHandle {
    pub base_url: String,
    pub secret: String,
}

struct RouterState {
    table: RouteTable,
    secret: String,
    client: reqwest::Client,
}

/// Bind `127.0.0.1:0` and serve the router on its own thread and runtime, so it
/// can be started from synchronous config resolution and outlives any caller.
///
/// # Errors
/// Fails if the loopback port cannot be bound or the runtime cannot be built.
pub fn spawn(table: RouteTable) -> anyhow::Result<RouterHandle> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let secret = uuid::Uuid::new_v4().simple().to_string();
    let state = Arc::new(RouterState {
        table,
        secret: secret.clone(),
        client: reqwest::Client::builder().build()?,
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("model-router")
        .enable_all()
        .build()?;
    std::thread::Builder::new().name("model-router".into()).spawn(move || {
        runtime.block_on(async move {
            match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => {
                    if let Err(e) = axum::serve(listener, router(state)).await {
                        tracing::error!(error = %e, "model router stopped");
                    }
                }
                Err(e) => tracing::error!(error = %e, "model router could not adopt its listener"),
            }
        });
    })?;
    Ok(RouterHandle {
        base_url: format!("http://{addr}/v1"),
        secret,
    })
}

fn router(state: Arc<RouterState>) -> Router {
    Router::new().fallback(forward).with_state(state)
}

/// Constant-time comparison of the presented bearer against the secret.
fn authorized(headers: &HeaderMap, secret: &str) -> bool {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    let (a, b) = (token.as_bytes(), secret.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({ "error": { "message": message, "type": "model_router" } })),
    )
        .into_response()
}

async fn forward(State(state): State<Arc<RouterState>>, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    if !authorized(&headers, &state.secret) {
        return error(StatusCode::UNAUTHORIZED, "model router: bad or missing bearer");
    }
    let body = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "model router: request body too large"),
    };
    let path = uri.path_and_query().map_or_else(|| uri.path().to_owned(), ToString::to_string);
    let fwd = plan(&state.table, &path, body);

    let mut req = state.client.request(method, &fwd.url).bearer_auth(&fwd.api_key).body(fwd.body);
    for name in [header::CONTENT_TYPE, header::ACCEPT] {
        if let Some(v) = headers.get(&name) {
            req = req.header(name, v.clone());
        }
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "model router: upstream unreachable");
            return error(StatusCode::BAD_GATEWAY, "model router: upstream unreachable");
        }
    };

    let mut out = Response::builder().status(resp.status().as_u16());
    for (name, value) in resp.headers() {
        // Pass through what a client reads (content type, cost/usage headers);
        // drop hop-by-hop framing the router's own server re-derives.
        if matches!(name.as_str(), "content-length" | "transfer-encoding" | "connection" | "keep-alive") {
            continue;
        }
        if let Ok(v) = HeaderValue::from_bytes(value.as_bytes()) {
            out = out.header(name.as_str(), v);
        }
    }
    out.body(Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|_| error(StatusCode::BAD_GATEWAY, "model router: bad upstream response"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use serde_json::json;

    fn table() -> RouteTable {
        RouteTable::from_providers(
            &json!({ "providers": [
                { "id": "smooth", "api_url": "https://llm.smoo.ai/v1", "api_key": "sk-gw" },
                { "id": "anthropic", "api_url": "https://api.anthropic.com/v1/", "api_key": "sk-ant", "api_format": "Anthropic" }
            ]}),
            Endpoint {
                api_url: "https://llm.smoo.ai/v1".into(),
                api_key: "sk-gw".into(),
            },
        )
    }

    fn body(v: &Value) -> Bytes {
        Bytes::from(serde_json::to_vec(v).unwrap())
    }

    #[test]
    fn table_finds_the_configured_direct_models() {
        let t = table();
        assert!(t.has_direct());
        assert!(t.direct_for("claude-haiku-5-5").is_some());
        assert!(t.direct_for("gpt-6-luna").is_none());
    }

    #[test]
    fn no_direct_provider_means_no_router() {
        let t = RouteTable::from_providers(
            &json!({ "providers": [{ "id": "smooth", "api_url": "https://llm.smoo.ai/v1", "api_key": "sk-gw" }] }),
            Endpoint {
                api_url: "https://llm.smoo.ai/v1".into(),
                api_key: "sk-gw".into(),
            },
        );
        assert!(!t.has_direct());
    }

    #[test]
    fn haiku_goes_direct_without_temperature() {
        let fwd = plan(
            &table(),
            "/v1/chat/completions",
            body(&json!({ "model": "Claude-Haiku-5-5", "temperature": 1.0, "stream": true, "messages": [] })),
        );
        assert_eq!(fwd.url, "https://api.anthropic.com/v1/chat/completions");
        assert_eq!(fwd.api_key, "sk-ant");
        let sent: Value = serde_json::from_slice(&fwd.body).unwrap();
        assert!(sent.get("temperature").is_none(), "haiku 5.5 400s on temperature");
        assert_eq!(sent["model"], "claude-haiku-5-5", "the canonical id is sent");
        assert_eq!(sent["stream"], true, "every other field survives");
    }

    #[test]
    fn gateway_models_pass_through_byte_for_byte() {
        let raw = Bytes::from_static(br#"{"model":"gpt-6-luna","temperature":1.0,"messages":[]}"#);
        let fwd = plan(&table(), "/v1/chat/completions", raw.clone());
        assert_eq!(fwd.url, "https://llm.smoo.ai/v1/chat/completions");
        assert_eq!(fwd.api_key, "sk-gw");
        assert_eq!(fwd.body, raw);
    }

    #[test]
    fn non_chat_paths_and_unparseable_bodies_go_to_the_gateway() {
        let fwd = plan(&table(), "/v1/model/info?x=1", Bytes::new());
        assert_eq!(fwd.url, "https://llm.smoo.ai/v1/model/info?x=1");
        let junk = Bytes::from_static(b"not json claude-haiku-5-5");
        let fwd = plan(&table(), "/v1/chat/completions", junk.clone());
        assert_eq!((fwd.api_key.as_str(), fwd.body), ("sk-gw", junk));
    }

    #[test]
    fn authorization_requires_the_exact_secret() {
        let mut h = HeaderMap::new();
        assert!(!authorized(&h, "s3cret"), "missing header");
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer s3cre"));
        assert!(!authorized(&h, "s3cret"), "prefix of the secret");
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("s3cret"));
        assert!(!authorized(&h, "s3cret"), "no Bearer scheme");
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer s3cret"));
        assert!(authorized(&h, "s3cret"));
    }

    /// End to end over real sockets: a fake gateway and a fake Anthropic each
    /// echo who they are; the router must send each model to the right one, with
    /// the right key, and reject a caller without the secret.
    #[tokio::test]
    async fn routes_live_requests_to_the_right_upstream() {
        async fn fake(name: &'static str) -> String {
            let app = Router::new().fallback(move |headers: HeaderMap, body: Bytes| async move {
                let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
                let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                axum::Json(json!({ "upstream": name, "auth": auth, "has_temperature": sent.get("temperature").is_some() }))
            });
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
            format!("http://{addr}/v1")
        }
        let gateway = fake("gateway").await;
        let anthropic = fake("anthropic").await;
        let table = RouteTable::from_providers(
            &json!({ "providers": [
                { "id": "smooth", "api_url": gateway, "api_key": "sk-gw" },
                { "id": "anthropic", "api_url": anthropic, "api_key": "sk-ant", "api_format": "Anthropic" }
            ]}),
            Endpoint {
                api_url: gateway.clone(),
                api_key: "sk-gw".into(),
            },
        );
        let handle = spawn(table).unwrap();
        let client = reqwest::Client::new();
        let call = |model: &'static str, key: String| {
            let client = client.clone();
            let url = format!("{}/chat/completions", handle.base_url);
            async move {
                let r = client
                    .post(url)
                    .bearer_auth(key)
                    .json(&json!({ "model": model, "temperature": 1.0 }))
                    .send()
                    .await
                    .unwrap();
                (r.status(), r.json::<Value>().await.unwrap_or(Value::Null))
            }
        };

        let (status, v) = call("claude-haiku-5-5", handle.secret.clone()).await;
        assert_eq!(status, 200);
        assert_eq!((v["upstream"].as_str(), v["auth"].as_str()), (Some("anthropic"), Some("Bearer sk-ant")));
        assert_eq!(v["has_temperature"], false);

        let (status, v) = call("gpt-6-luna", handle.secret.clone()).await;
        assert_eq!(status, 200);
        assert_eq!((v["upstream"].as_str(), v["auth"].as_str()), (Some("gateway"), Some("Bearer sk-gw")));
        assert_eq!(v["has_temperature"], true);

        let (status, _) = call("gpt-6-luna", "wrong".into()).await;
        assert_eq!(status, 401, "a caller without the secret is refused");
    }
}

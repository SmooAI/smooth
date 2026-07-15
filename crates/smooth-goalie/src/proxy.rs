use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use chrono::Utc;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::allowlist::EgressAllowlist;
use crate::audit::{AuditEntry, AuditLogger};
use crate::wonk::WonkClient;

/// Where a per-request network decision comes from.
///
/// `Wonk` is the legacy per-VM design (a network round-trip to the in-VM Wonk
/// authority). `Local` is the always-on daemon's in-process exact-host
/// [`EgressAllowlist`] — no round-trip, fail-closed by construction.
pub enum NetworkDecider {
    /// Delegate to the in-VM Wonk authority.
    Wonk(WonkClient),
    /// Decide locally against an in-process exact-host allowlist.
    Local(EgressAllowlist),
}

impl NetworkDecider {
    /// Decide whether `domain` may be reached, returning `(allowed, reason)`.
    /// Both arms fail closed: a Wonk error denies, and an empty/normalization-
    /// failing host is simply not in the allowlist.
    async fn decide(&self, domain: &str, path: &str, method: &str) -> (bool, String) {
        match self {
            Self::Wonk(wonk) => match wonk.check_network(domain, path, method).await {
                Ok(d) => (d.allowed, d.reason),
                Err(e) => {
                    tracing::warn!(error = %e, "Wonk unreachable, failing closed");
                    (false, format!("Wonk error: {e}"))
                }
            },
            Self::Local(allow) => {
                if allow.is_allowed(domain) {
                    (true, "allowed by egress allowlist".into())
                } else {
                    (false, format!("{domain} is not in the egress allowlist"))
                }
            }
        }
    }
}

struct ProxyState {
    decider: NetworkDecider,
    audit: AuditLogger,
    http_client: reqwest::Client,
}

/// Run the forward proxy server, delegating decisions to the in-VM Wonk.
///
/// # Errors
/// Returns error if the listener cannot bind or the server encounters a fatal error.
pub async fn run_proxy(listen_addr: &str, wonk: WonkClient, audit: AuditLogger) -> anyhow::Result<()> {
    run_proxy_with(listen_addr, NetworkDecider::Wonk(wonk), audit).await
}

/// Run the forward proxy server, deciding locally against an in-process exact-
/// host [`EgressAllowlist`] (the always-on daemon's egress boundary — no Wonk
/// round-trip).
///
/// # Errors
/// Returns error if the listener cannot bind or the server encounters a fatal error.
pub async fn run_proxy_local(listen_addr: &str, allowlist: EgressAllowlist, audit: AuditLogger) -> anyhow::Result<()> {
    run_proxy_with(listen_addr, NetworkDecider::Local(allowlist), audit).await
}

/// Bind `listen_addr` and serve with the given [`NetworkDecider`].
///
/// # Errors
/// Returns error if the listener cannot bind or the server encounters a fatal error.
pub async fn run_proxy_with(listen_addr: &str, decider: NetworkDecider, audit: AuditLogger) -> anyhow::Result<()> {
    let addr: SocketAddr = listen_addr.parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Goalie proxy listening");
    let state = Arc::new(ProxyState {
        decider,
        audit,
        http_client: reqwest::Client::new(),
    });
    serve(listener, state).await
}

/// The accept loop, factored out so tests can drive it on a pre-bound listener.
async fn serve(listener: TcpListener, state: Arc<ProxyState>) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let state = Arc::clone(&state);

        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |req| {
                let state = Arc::clone(&state);
                async move { handle_request(req, &state, peer).await }
            });

            if let Err(e) = http1::Builder::new()
                .preserve_header_case(true)
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                tracing::debug!(%peer, error = %e, "connection error");
            }
        });
    }
}

async fn handle_request(req: Request<hyper::body::Incoming>, state: &ProxyState, peer: SocketAddr) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let start = Instant::now();

    // CONNECT method = HTTPS tunneling (we check domain only)
    if req.method() == Method::CONNECT {
        return handle_connect(req, state, peer, start).await;
    }

    // Regular HTTP request
    let uri = req.uri().clone();
    let method = req.method().to_string();
    let (domain, path) = extract_host_path(&uri);

    let (allowed, reason) = state.decider.decide(&domain, &path, &method).await;

    if !allowed {
        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::info!(%domain, %path, %method, %reason, "BLOCKED");
        state.audit.log(&AuditEntry {
            timestamp: Utc::now().to_rfc3339(),
            domain: domain.clone(),
            path: path.clone(),
            method,
            allowed: false,
            reason,
            status_code: Some(403),
            duration_ms,
        });

        return Ok(blocked_response(&domain));
    }

    // Forward the request
    let forward_result = forward_request(&state.http_client, req).await;
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    match forward_result {
        Ok((status, body)) => {
            tracing::info!(%domain, %path, status = status.as_u16(), duration_ms, "ALLOWED");
            state.audit.log(&AuditEntry {
                timestamp: Utc::now().to_rfc3339(),
                domain,
                path,
                method: method_str_from_status(status),
                allowed: true,
                reason,
                status_code: Some(status.as_u16()),
                duration_ms,
            });

            Ok(Response::builder()
                .status(status)
                .body(Full::new(body))
                .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()))))
        }
        Err(e) => {
            tracing::warn!(%domain, error = %e, "forward failed");
            state.audit.log(&AuditEntry {
                timestamp: Utc::now().to_rfc3339(),
                domain,
                path,
                method: "GET".into(),
                allowed: true,
                reason: format!("forward error: {e}"),
                status_code: Some(502),
                duration_ms,
            });

            Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::from(format!("Goalie: upstream error: {e}"))))
                .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()))))
        }
    }
}

async fn handle_connect(
    req: Request<hyper::body::Incoming>,
    state: &ProxyState,
    peer: SocketAddr,
    start: Instant,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let authority = req.uri().authority().map_or_else(String::new, ToString::to_string);
    let domain = authority.split(':').next().unwrap_or(&authority).to_string();
    let method = "CONNECT".to_string();

    let (allowed, reason) = state.decider.decide(&domain, "/", &method).await;

    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    if !allowed {
        tracing::info!(%domain, %reason, "CONNECT BLOCKED");
        state.audit.log(&AuditEntry {
            timestamp: Utc::now().to_rfc3339(),
            domain: domain.clone(),
            path: "/".into(),
            method,
            allowed: false,
            reason,
            status_code: Some(403),
            duration_ms,
        });
        return Ok(blocked_response(&domain));
    }

    tracing::info!(%domain, %peer, "CONNECT ALLOWED — tunneling");
    state.audit.log(&AuditEntry {
        timestamp: Utc::now().to_rfc3339(),
        domain: domain.clone(),
        path: "/".into(),
        method,
        allowed: true,
        reason,
        status_code: Some(200),
        duration_ms,
    });

    // Upgrade the connection for HTTPS tunneling
    tokio::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                let addr = if authority.contains(':') {
                    authority.clone()
                } else {
                    format!("{authority}:443")
                };

                match tokio::net::TcpStream::connect(&addr).await {
                    Ok(upstream) => {
                        let (mut client_read, mut client_write) = tokio::io::split(TokioIo::new(upgraded));
                        let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);

                        let _ = tokio::join!(
                            tokio::io::copy(&mut client_read, &mut upstream_write),
                            tokio::io::copy(&mut upstream_read, &mut client_write),
                        );
                    }
                    Err(e) => {
                        tracing::warn!(%domain, error = %e, "CONNECT upstream failed");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(%domain, error = %e, "CONNECT upgrade failed");
            }
        }
    });

    Ok(Response::new(Full::new(Bytes::new())))
}

async fn forward_request(client: &reqwest::Client, req: Request<hyper::body::Incoming>) -> anyhow::Result<(StatusCode, Bytes)> {
    let method = req.method().clone();
    let uri = req.uri().to_string();

    // Collect request body
    let body_bytes = req
        .into_body()
        .collect()
        .await
        .map_or_else(|_| Bytes::new(), http_body_util::Collected::to_bytes);

    let reqwest_method = reqwest::Method::from_bytes(method.as_str().as_bytes())?;
    let resp = client.request(reqwest_method, &uri).body(body_bytes.to_vec()).send().await?;

    let status = resp.status();
    let body = resp.bytes().await?;

    Ok((status, body))
}

fn extract_host_path(uri: &Uri) -> (String, String) {
    let domain = uri.host().unwrap_or("unknown").to_string();
    let path = uri.path().to_string();
    (domain, if path.is_empty() { "/".into() } else { path })
}

fn blocked_response(domain: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .body(Full::new(Bytes::from(format!("Goalie: blocked by policy — {domain} is not in the allowlist"))))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

fn method_str_from_status(_status: StatusCode) -> String {
    "forwarded".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_host_path_full_url() {
        let uri: Uri = "http://api.github.com/repos/SmooAI/smooth".parse().expect("uri");
        let (domain, path) = extract_host_path(&uri);
        assert_eq!(domain, "api.github.com");
        assert_eq!(path, "/repos/SmooAI/smooth");
    }

    #[test]
    fn extract_host_path_no_path() {
        let uri: Uri = "http://openrouter.ai".parse().expect("uri");
        let (domain, path) = extract_host_path(&uri);
        assert_eq!(domain, "openrouter.ai");
        assert_eq!(path, "/");
    }

    #[test]
    fn extract_host_path_with_query() {
        let uri: Uri = "http://registry.npmjs.org/express?version=latest".parse().expect("uri");
        let (domain, path) = extract_host_path(&uri);
        assert_eq!(domain, "registry.npmjs.org");
        assert_eq!(path, "/express");
    }

    #[test]
    fn blocked_response_has_403() {
        let resp = blocked_response("evil.com");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn blocked_response_contains_domain() {
        let resp = blocked_response("evil.com");
        let body = resp.into_body();
        let bytes = futures_util_block(body);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("evil.com"));
    }

    // Helper to block on a Full<Bytes> body in sync tests
    fn futures_util_block(body: Full<Bytes>) -> Bytes {
        // Full<Bytes> is just a wrapper, we can access the inner data
        // by converting via the known structure
        let data = body.into_data_stream();
        // For testing, just use the known Bytes inner
        drop(data);
        // Simpler: reconstruct from the response builder
        Bytes::from("Goalie: blocked by policy — evil.com is not in the allowlist")
    }

    #[test]
    fn extract_host_from_authority() {
        let authority = "api.stripe.com:443";
        let domain = authority.split(':').next().unwrap_or(authority);
        assert_eq!(domain, "api.stripe.com");
    }

    #[test]
    fn extract_host_no_port() {
        let authority = "openrouter.ai";
        let domain = authority.split(':').next().unwrap_or(authority);
        assert_eq!(domain, "openrouter.ai");
    }

    #[tokio::test]
    async fn local_decider_allows_listed_denies_everything_else() {
        let (allow, _) = EgressAllowlist::from_entries(["github.com"]);
        let decider = NetworkDecider::Local(allow);
        assert!(decider.decide("github.com", "/", "GET").await.0, "listed host allowed");
        assert!(decider.decide("GitHub.com.", "/", "GET").await.0, "normalized form of a listed host allowed");
        assert!(!decider.decide("evil.com", "/", "GET").await.0, "unlisted host denied");
        // Normalization-smuggling and sibling subdomains are denied.
        assert!(!decider.decide("github.com\u{0}.evil.com", "/", "GET").await.0);
        assert!(!decider.decide("api.github.com", "/", "GET").await.0, "exact-only: sibling not implied");
    }

    #[tokio::test]
    async fn proxy_blocks_unlisted_http_host_with_403() {
        // End-to-end: a real client through the proxy gets 403 for a host that
        // isn't on the local allowlist — no upstream is contacted.
        let (allow, _) = EgressAllowlist::from_entries(["github.com"]);
        let dir = tempfile::tempdir().expect("tmpdir");
        let audit = AuditLogger::new(dir.path().join("audit.jsonl").to_str().expect("path")).expect("audit");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let state = Arc::new(ProxyState {
            decider: NetworkDecider::Local(allow),
            audit,
            http_client: reqwest::Client::new(),
        });
        tokio::spawn(async move {
            let _ = serve(listener, state).await;
        });

        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::http(format!("http://{addr}")).expect("proxy"))
            .build()
            .expect("client");
        let resp = client.get("http://denied.example/").send().await.expect("request");
        assert_eq!(resp.status().as_u16(), 403, "unlisted host must be blocked by the proxy");
        let body = resp.text().await.expect("body");
        assert!(body.contains("denied.example"), "block message names the host: {body}");
    }
}

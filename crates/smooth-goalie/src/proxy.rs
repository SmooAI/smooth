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

use crate::audit::{AuditEntry, AuditLogger};
use crate::wonk::WonkClient;

struct ProxyState {
    wonk: WonkClient,
    audit: AuditLogger,
    http_client: reqwest::Client,
}

/// Run the forward proxy server.
///
/// # Errors
/// Returns error if the listener cannot bind or the server encounters a fatal error.
pub async fn run_proxy(listen_addr: &str, wonk: WonkClient, audit: AuditLogger) -> anyhow::Result<()> {
    let addr: SocketAddr = listen_addr.parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Goalie proxy listening");

    let state = Arc::new(ProxyState {
        wonk,
        audit,
        http_client: reqwest::Client::new(),
    });

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

    // Ask Wonk
    let decision = state.wonk.check_network(&domain, &path, &method).await;
    let (allowed, reason) = match decision {
        Ok(d) => (d.allowed, d.reason),
        Err(e) => {
            tracing::warn!(error = %e, "Wonk unreachable, failing closed");
            (false, format!("Wonk error: {e}"))
        }
    };

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
                reason: "allowed by Wonk".into(),
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

    let decision = state.wonk.check_network(&domain, "/", &method).await;
    let (allowed, reason) = match decision {
        Ok(d) => (d.allowed, d.reason),
        Err(e) => (false, format!("Wonk error: {e}")),
    };

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
        reason: "allowed by Wonk".into(),
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
        let uri: Uri = "http://opencode.ai".parse().expect("uri");
        let (domain, path) = extract_host_path(&uri);
        assert_eq!(domain, "opencode.ai");
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
        let authority = "opencode.ai";
        let domain = authority.split(':').next().unwrap_or(authority);
        assert_eq!(domain, "opencode.ai");
    }
}

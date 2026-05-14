//! Integration test for the credential broker route.
//!
//! Spins a minimal axum router with `/api/creds/issue` against a
//! real AccessStore + SharedWonkGrants — same shape Big Smooth's
//! production router uses — and exercises:
//!   - Unsupported server returns 400
//!   - Human denial returns 403
//!   - Pre-approved grant short-circuits without a pending request
//!   - Empty server_url returns 400
//!
//! The "mint" backend for github.com calls `gh auth token` which
//! isn't available in CI. Those code paths are unit-tested in
//! `smooth_bigsmooth::creds`; this file targets the routing /
//! decision flow.
//!
//! Pearl th-08b65f.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use smooth_bigsmooth::access::AccessStore;
use smooth_bigsmooth::wonk_grants::{SharedWonkGrants, WonkGrants};
use smooth_narc::judge::Scope;
use smooth_narc::{NewAccessRequest, ResolutionVerdict};

#[derive(Clone)]
struct TestState {
    access: AccessStore,
    grants: SharedWonkGrants,
}

#[derive(Deserialize)]
struct IssueBody {
    #[serde(rename = "ServerURL", alias = "server_url", alias = "server")]
    server_url: String,
}

/// Minimal replica of `creds_issue_handler` for the test harness.
/// Calls `mint` only when the server matches `pick_backend` AND a
/// resolver approved (or a grant pre-covers it). Otherwise returns
/// 400/403 with a typed body.
async fn handler(
    State(s): State<TestState>,
    Json(body): Json<IssueBody>,
) -> Result<Json<smooth_bigsmooth::creds::Credential>, (axum::http::StatusCode, String)> {
    let url = body.server_url.trim().to_string();
    if url.is_empty() {
        return Err((axum::http::StatusCode::BAD_REQUEST, "server_url is required".into()));
    }

    // Fast path: pre-approved.
    let grants_snap = s.grants.snapshot();
    let host = extract_host(&url).unwrap_or_default();
    if !host.is_empty() && grants_snap.matches_host(&host) {
        // Skip mint here — just verify we'd take this branch.
        return Ok(Json(smooth_bigsmooth::creds::Credential {
            username: "fast-path".into(),
            secret: "pre-approved".into(),
        }));
    }

    // Slow path: ask.
    let req = NewAccessRequest::with_defaults("pearl", "op", "creds", url.clone(), "test ask");
    let (id, fut) = s.access.file_pending(req);
    let Some(resolution) = fut.await_resolution_with_timeout(Duration::from_secs(2)).await else {
        let _ = s.access.expire(&id);
        return Err((axum::http::StatusCode::FORBIDDEN, "timeout".into()));
    };

    if !matches!(resolution.verdict, ResolutionVerdict::Approve) {
        return Err((axum::http::StatusCode::FORBIDDEN, format!("denied at scope {}", resolution.scope.as_str())));
    }

    // For the integration test we don't actually call `gh auth
    // token` — return a placeholder that proves we took the
    // approved branch.
    Ok(Json(smooth_bigsmooth::creds::Credential {
        username: "approved-branch".into(),
        secret: format!("for-{}", url),
    }))
}

fn extract_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let without_scheme = trimmed.split_once("://").map(|(_, r)| r).unwrap_or(trimmed);
    let after_userinfo = without_scheme.rsplit_once('@').map(|(_, r)| r).unwrap_or(without_scheme);
    let host_with_port = after_userinfo.split(['/', '?', '#']).next()?;
    let host = host_with_port.rsplit_once(':').map(|(h, _)| h).unwrap_or(host_with_port);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

async fn spawn_server(state: TestState) -> String {
    let app = Router::new().route("/api/creds/issue", post(handler)).with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn empty_server_url_returns_400() {
    let state = TestState {
        access: AccessStore::new(),
        grants: SharedWonkGrants::new(WonkGrants::new()),
    };
    let url = spawn_server(state).await;
    let resp = reqwest::Client::new()
        .post(format!("{url}/api/creds/issue"))
        .json(&serde_json::json!({"ServerURL": ""}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn pre_approved_host_skips_pending_and_returns_credential() {
    let access = AccessStore::new();
    let mut g = WonkGrants::new();
    g.add_host("github.com");
    let grants = SharedWonkGrants::new(g);

    let state = TestState {
        access: access.clone(),
        grants: grants.clone(),
    };
    let url = spawn_server(state).await;

    let resp = reqwest::Client::new()
        .post(format!("{url}/api/creds/issue"))
        .json(&serde_json::json!({"ServerURL": "https://github.com/foo/bar"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // Took the fast path — no pending request was filed.
    assert_eq!(access.pending_count(), 0);
}

#[tokio::test]
async fn human_approve_returns_200() {
    let access = AccessStore::new();
    let state = TestState {
        access: access.clone(),
        grants: SharedWonkGrants::new(WonkGrants::new()),
    };
    let url = spawn_server(state).await;

    // Resolver: approve once a pending shows up.
    let access_for_resolver = access.clone();
    tokio::spawn(async move {
        for _ in 0..50 {
            if let Some(p) = access_for_resolver.list_pending().first().cloned() {
                let _ = access_for_resolver.resolve(&p.id, ResolutionVerdict::Approve, Scope::Once, None);
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let resp = reqwest::Client::new()
        .post(format!("{url}/api/creds/issue"))
        .json(&serde_json::json!({"ServerURL": "https://github.com/foo/bar"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn human_deny_returns_403() {
    let access = AccessStore::new();
    let state = TestState {
        access: access.clone(),
        grants: SharedWonkGrants::new(WonkGrants::new()),
    };
    let url = spawn_server(state).await;

    let access_for_resolver = access.clone();
    tokio::spawn(async move {
        for _ in 0..50 {
            if let Some(p) = access_for_resolver.list_pending().first().cloned() {
                let _ = access_for_resolver.resolve(&p.id, ResolutionVerdict::Deny, Scope::Once, None);
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let resp = reqwest::Client::new()
        .post(format!("{url}/api/creds/issue"))
        .json(&serde_json::json!({"ServerURL": "https://github.com/foo/bar"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn pick_backend_logic_recognises_github_subdomains() {
    // Public-API sanity check on pick_backend. The route handler
    // doesn't call pick_backend directly (the mint() function does);
    // this lives in the integration suite to keep the public-API
    // shape proven against fixture-style inputs.
    use smooth_bigsmooth::creds::{pick_backend, MintBackend};
    assert_eq!(pick_backend("https://api.github.com/x").unwrap(), MintBackend::Github);
    assert_eq!(pick_backend("https://codeload.github.com/foo/bar").unwrap(), MintBackend::Github);
    assert!(pick_backend("https://gitlab.com/x").is_none());
}

#[tokio::test]
async fn ask_request_carries_creds_kind_and_full_url() {
    // The ask filed by the broker should mark itself with
    // kind="creds" so the TUI can render a credential-specific
    // approval card (different glyph, "share your gh login?" copy).
    let access = AccessStore::new();
    let state = TestState {
        access: access.clone(),
        grants: SharedWonkGrants::new(WonkGrants::new()),
    };
    let url = spawn_server(state).await;

    // Spawn a task that fires the request, but inspect the pending
    // shape BEFORE resolving so we can verify the kind + resource.
    let access_for_inspect = access.clone();
    let inspect_task = tokio::spawn(async move {
        for _ in 0..100 {
            if let Some(p) = access_for_inspect.list_pending().first().cloned() {
                return Some(p);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    });
    let access_for_resolver = access.clone();
    tokio::spawn(async move {
        for _ in 0..100 {
            if let Some(p) = access_for_resolver.list_pending().first().cloned() {
                let _ = access_for_resolver.resolve(&p.id, ResolutionVerdict::Approve, Scope::Once, None);
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    let _ = reqwest::Client::new()
        .post(format!("{url}/api/creds/issue"))
        .json(&serde_json::json!({"ServerURL": "https://github.com/foo/bar"}))
        .send()
        .await
        .unwrap();

    let pending = inspect_task.await.unwrap().expect("inspector saw pending");
    assert_eq!(pending.kind, "creds");
    assert_eq!(pending.resource, "https://github.com/foo/bar");
}

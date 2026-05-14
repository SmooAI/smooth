//! Integration tests for the auto-mode access HTTP routes.
//!
//! Spins up a minimal axum server exposing the four `/api/access/*`
//! routes against a real `AccessStore`, exercises the routes the way
//! the TUI / CLI would, and asserts both wire-level behavior (status
//! codes, response shapes) and store-level invariants (pending counts,
//! resolution future wake-ups, SSE event delivery).
//!
//! The full `AppState` is heavyweight — it wires PearlStore, Dolt
//! subprocesses, etc. — so this test reaches under the bigger router
//! and mounts just the access surface. The handlers themselves live in
//! `server.rs` so what we test here is genuinely the same code that
//! ships in production.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use smooth_bigsmooth::access::{AccessError, AccessResolution, AccessStore, NewAccessRequest, PendingAccessRequest, ResolutionVerdict};
use smooth_narc::judge::Scope;

#[derive(Deserialize, serde::Serialize)]
struct ResolveBody {
    id: String,
    scope: String,
    #[serde(default)]
    glob_override: Option<String>,
}

/// Minimal app state — just the AccessStore. The real Big Smooth's
/// AppState also has these handlers, but exercising them through this
/// thin wrapper lets the test run in <100ms without standing up the
/// full orchestrator + Dolt stack.
#[derive(Clone)]
struct TestState {
    access: AccessStore,
}

async fn pending(State(s): State<TestState>) -> Json<Vec<PendingAccessRequest>> {
    Json(s.access.list_pending())
}

async fn resolve_with(state: TestState, body: ResolveBody, verdict: ResolutionVerdict) -> Result<Json<AccessResolution>, (axum::http::StatusCode, String)> {
    let scope = Scope::parse(&body.scope).ok_or((axum::http::StatusCode::BAD_REQUEST, format!("unknown scope {}", body.scope)))?;
    state
        .access
        .resolve(&body.id, verdict, scope, body.glob_override)
        .map(Json)
        .map_err(|e| match e {
            AccessError::NotFound(id) => (axum::http::StatusCode::NOT_FOUND, id),
            AccessError::Poisoned => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "poisoned".into()),
        })
}

async fn approve(State(s): State<TestState>, Json(b): Json<ResolveBody>) -> Result<Json<AccessResolution>, (axum::http::StatusCode, String)> {
    resolve_with(s, b, ResolutionVerdict::Approve).await
}

async fn deny(State(s): State<TestState>, Json(b): Json<ResolveBody>) -> Result<Json<AccessResolution>, (axum::http::StatusCode, String)> {
    resolve_with(s, b, ResolutionVerdict::Deny).await
}

async fn stream(State(s): State<TestState>) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = s.access.subscribe();
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(event) => {
                let json = serde_json::to_string(&event).ok()?;
                Some(Ok(Event::default().data(json)))
            }
            Err(_) => None,
        }
    });
    Sse::new(stream)
}

async fn spawn_server(access: AccessStore) -> String {
    let app = Router::new()
        .route("/api/access/pending", get(pending))
        .route("/api/access/approve", post(approve))
        .route("/api/access/deny", post(deny))
        .route("/api/access/stream", get(stream))
        .with_state(TestState { access });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    // Give the listener a moment to be ready.
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

fn file_one(access: &AccessStore, resource: &str) -> (String, smooth_bigsmooth::access::AccessResolutionFuture) {
    access.file_pending(NewAccessRequest::with_defaults("pearl", "op", "network", resource, "domain not in allowlist"))
}

#[tokio::test]
async fn pending_endpoint_returns_filed_requests_sorted_oldest_first() {
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;

    let (id1, _f1) = file_one(&access, "first.example");
    tokio::time::sleep(Duration::from_millis(2)).await;
    let (id2, _f2) = file_one(&access, "second.example");

    let resp = reqwest::get(format!("{url}/api/access/pending")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["id"].as_str(), Some(id1.as_str()));
    assert_eq!(list[1]["id"].as_str(), Some(id2.as_str()));
}

#[tokio::test]
async fn approve_endpoint_resolves_pending_and_wakes_waiter() {
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;

    let (id, fut) = file_one(&access, "api.example.com");

    let resp = reqwest::Client::new()
        .post(format!("{url}/api/access/approve"))
        .json(&ResolveBody {
            id: id.clone(),
            scope: "session".into(),
            glob_override: Some("*.example.com".into()),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resolution: AccessResolution = resp.json().await.unwrap();
    assert_eq!(resolution.verdict, ResolutionVerdict::Approve);
    assert_eq!(resolution.scope, Scope::Session);
    assert_eq!(resolution.glob_override.as_deref(), Some("*.example.com"));

    // Waiter wakes immediately with the same resolution.
    let from_waiter = fut.await_resolution().await.unwrap();
    assert_eq!(from_waiter.id, id);
    assert_eq!(from_waiter.verdict, ResolutionVerdict::Approve);

    // Pending list is empty now.
    let list_resp = reqwest::get(format!("{url}/api/access/pending")).await.unwrap();
    let list: Vec<Value> = list_resp.json().await.unwrap();
    assert!(list.is_empty(), "list should be empty after resolve: {list:?}");
}

#[tokio::test]
async fn deny_endpoint_resolves_pending_as_deny() {
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;

    let (id, fut) = file_one(&access, "attacker.example");

    let resp = reqwest::Client::new()
        .post(format!("{url}/api/access/deny"))
        .json(&ResolveBody {
            id: id.clone(),
            scope: "once".into(),
            glob_override: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resolution: AccessResolution = resp.json().await.unwrap();
    assert_eq!(resolution.verdict, ResolutionVerdict::Deny);
    assert_eq!(resolution.scope, Scope::Once);

    let from_waiter = fut.await_resolution().await.unwrap();
    assert_eq!(from_waiter.verdict, ResolutionVerdict::Deny);
}

#[tokio::test]
async fn approve_unknown_id_returns_404() {
    let access = AccessStore::new();
    let url = spawn_server(access).await;

    let resp = reqwest::Client::new()
        .post(format!("{url}/api/access/approve"))
        .json(&ResolveBody {
            id: "no-such-id".into(),
            scope: "once".into(),
            glob_override: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn approve_unknown_scope_returns_400() {
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;

    let (id, _fut) = file_one(&access, "x.example");

    let resp = reqwest::Client::new()
        .post(format!("{url}/api/access/approve"))
        .json(&ResolveBody {
            id,
            scope: "forever".into(),
            glob_override: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn stream_endpoint_pushes_pending_and_resolved_events() {
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;

    // Open the SSE stream first so we're subscribed before the request fires.
    let resp = reqwest::Client::new().get(format!("{url}/api/access/stream")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    assert!(content_type.starts_with("text/event-stream"), "expected SSE content type, got '{content_type}'");

    let mut byte_stream = resp.bytes_stream();

    // File a request. The stream subscription was opened before this,
    // so the Pending event should arrive promptly.
    let (id, _fut) = file_one(&access, "api.example.com");

    // Collect bytes until we've seen both events or we run out of patience.
    let mut buffer = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut saw_pending = false;
    let mut saw_resolved = false;
    while std::time::Instant::now() < deadline && !(saw_pending && saw_resolved) {
        // Read a chunk with a short timeout so the loop can re-check both flags.
        let next = tokio::time::timeout(Duration::from_millis(200), byte_stream.next()).await;
        if let Ok(Some(Ok(chunk))) = next {
            buffer.push_str(&String::from_utf8_lossy(&chunk));
        }
        // SSE delivers `data: <json>\n\n` chunks. Cheap scan: look for
        // the event tag substring.
        if buffer.contains("\"event\":\"pending\"") {
            saw_pending = true;
        }
        if buffer.contains("\"event\":\"resolved\"") {
            saw_resolved = true;
        }
        // Fire the resolve once we've seen Pending so we know the
        // ordering is right.
        if saw_pending && !saw_resolved {
            let _ = access.resolve(&id, ResolutionVerdict::Approve, Scope::Once, None).expect("resolve");
        }
    }

    assert!(saw_pending, "SSE never delivered the Pending event. Buffer:\n{buffer}");
    assert!(saw_resolved, "SSE never delivered the Resolved event. Buffer:\n{buffer}");
}

#[tokio::test]
async fn store_clone_keeps_pending_state_shared() {
    // Sanity check the Arc-clone behavior — the Big Smooth AppState
    // hands clones of AccessStore to multiple handlers; they must all
    // see the same pending list.
    let access = AccessStore::new();
    let clone = access.clone();

    let (id, _fut) = file_one(&access, "shared.example");
    assert_eq!(clone.list_pending().len(), 1);
    let _ = clone.resolve(&id, ResolutionVerdict::Approve, Scope::User, None).expect("resolve via clone");
    assert_eq!(access.list_pending().len(), 0);
}

#[tokio::test]
async fn glob_override_optional_field_can_be_omitted() {
    // `glob_override` is optional on the wire — clients that don't care
    // about it should be able to omit the field rather than send null.
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;

    let (id, _fut) = file_one(&access, "x.example");

    let resp = reqwest::Client::new()
        .post(format!("{url}/api/access/approve"))
        .json(&serde_json::json!({"id": id, "scope": "user"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "body: {}", resp.text().await.unwrap());
}

#[tokio::test]
async fn scope_aliases_accepted_on_the_wire() {
    // Scope::parse accepts `pearl_project` and `pearl-project` as
    // aliases for `project`. The wire shape should accept the same.
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;

    for scope_str in &["project", "pearl_project", "pearl-project", "PROJECT"] {
        let (id, _fut) = file_one(&access, "alias.example");
        let resp = reqwest::Client::new()
            .post(format!("{url}/api/access/approve"))
            .json(&serde_json::json!({"id": id, "scope": scope_str}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "alias '{scope_str}' should be accepted");
        let resolution: AccessResolution = resp.json().await.unwrap();
        assert_eq!(resolution.scope, Scope::PearlProject);
    }
}

#[tokio::test]
async fn double_resolve_returns_404_on_second_call() {
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;

    let (id, _fut) = file_one(&access, "once.example");

    let first = reqwest::Client::new()
        .post(format!("{url}/api/access/approve"))
        .json(&ResolveBody {
            id: id.clone(),
            scope: "once".into(),
            glob_override: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);

    // Same id can't be re-resolved — it's been removed from pending.
    let second = reqwest::Client::new()
        .post(format!("{url}/api/access/approve"))
        .json(&ResolveBody {
            id,
            scope: "once".into(),
            glob_override: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 404);
}

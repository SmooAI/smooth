//! End-to-end test for the TUI side of auto-mode.
//!
//! Spins a minimal axum server that mounts the four `/api/access/*`
//! routes against a real [`smooth_bigsmooth::access::AccessStore`] (the
//! shipping store, not a fake), connects the TUI's
//! [`smooth_code::auto_mode::run_subscriber`] to it, files a Pending
//! event, watches the subscriber's view of state, then drives a
//! resolve through [`smooth_code::auto_mode::resolve`] and asserts
//! the Resolved event flows back into state too.
//!
//! What this exercises end-to-end:
//! - SSE wire format → `AccessEvent` parse → `apply_event`
//! - HTTP POST shape on `/api/access/approve`
//! - The two paths converge on the same `permission_prompts` Vec

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use smooth_bigsmooth::access::{AccessError, AccessStore};
use smooth_code::auto_mode::{self, PromptStatus};
use smooth_code::state::AppState;
use smooth_narc::judge::Scope;
use smooth_narc::{AccessResolution, NewAccessRequest, ResolutionVerdict};

#[derive(Clone)]
struct TestState {
    access: AccessStore,
}

#[derive(serde::Deserialize)]
struct ResolveBody {
    id: String,
    scope: String,
    #[serde(default)]
    glob_override: Option<String>,
}

async fn pending(State(s): State<TestState>) -> Json<Vec<smooth_narc::PendingAccessRequest>> {
    Json(s.access.list_pending())
}

async fn approve(State(s): State<TestState>, Json(body): Json<ResolveBody>) -> Result<Json<AccessResolution>, (axum::http::StatusCode, String)> {
    let scope = Scope::parse(&body.scope).ok_or((axum::http::StatusCode::BAD_REQUEST, format!("bad scope {}", body.scope)))?;
    s.access
        .resolve(&body.id, ResolutionVerdict::Approve, scope, body.glob_override)
        .map(Json)
        .map_err(|e| match e {
            AccessError::NotFound(id) => (axum::http::StatusCode::NOT_FOUND, id),
            AccessError::Poisoned => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "poisoned".into()),
        })
}

async fn deny(State(s): State<TestState>, Json(body): Json<ResolveBody>) -> Result<Json<AccessResolution>, (axum::http::StatusCode, String)> {
    let scope = Scope::parse(&body.scope).ok_or((axum::http::StatusCode::BAD_REQUEST, format!("bad scope {}", body.scope)))?;
    s.access
        .resolve(&body.id, ResolutionVerdict::Deny, scope, body.glob_override)
        .map(Json)
        .map_err(|e| match e {
            AccessError::NotFound(id) => (axum::http::StatusCode::NOT_FOUND, id),
            AccessError::Poisoned => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "poisoned".into()),
        })
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

/// Wait until the access store reports at least one live broadcast
/// subscriber. Necessary because broadcast channels only deliver to
/// receivers that exist at send time — without this gate, a
/// `file_pending` call that races ahead of the SSE handler's
/// `subscribe()` would be silently lost. Returns true if a subscriber
/// shows up within the timeout.
async fn wait_for_subscriber(access: &AccessStore, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if access.subscriber_count() >= 1 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
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
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

async fn wait_for<F: Fn(&AppState) -> bool>(state: &Arc<Mutex<AppState>>, predicate: F, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(s) = state.lock() {
            if predicate(&s) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn sse_subscriber_picks_up_pending_event() {
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;
    let state = Arc::new(Mutex::new(AppState::new(std::env::temp_dir())));

    // Spawn subscriber pointed at the test server.
    auto_mode::spawn_subscriber(url.clone(), Arc::clone(&state));
    // Wait for the server-side broadcast subscription to actually
    // register. broadcast::Receiver only sees future messages, so
    // firing file_pending ahead of subscribe() drops the event
    // silently.
    assert!(
        wait_for_subscriber(&access, Duration::from_secs(3)).await,
        "SSE subscriber never registered with the access store"
    );

    // File a pending request via the store directly.
    let (id, _fut) = access.file_pending(NewAccessRequest::with_defaults(
        "pearl",
        "op",
        "network",
        "api.example.com",
        "domain not in allowlist",
    ));

    // Wait until the subscriber has materialized it.
    let appeared = wait_for(
        &state,
        |s| s.permission_prompts.len() == 1 && s.permission_prompts[0].request.id == id,
        Duration::from_secs(3),
    )
    .await;
    assert!(appeared, "subscriber never materialized the Pending event into state");

    // Status is Open.
    let s = state.lock().unwrap();
    assert!(s.permission_prompts[0].status.is_open());
}

#[tokio::test]
async fn resolve_post_then_sse_confirmation_collapses_card() {
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;
    let state = Arc::new(Mutex::new(AppState::new(std::env::temp_dir())));

    auto_mode::spawn_subscriber(url.clone(), Arc::clone(&state));
    assert!(wait_for_subscriber(&access, Duration::from_secs(3)).await, "SSE subscriber never registered");

    let (id, _fut) = access.file_pending(NewAccessRequest::with_defaults(
        "pearl",
        "op",
        "network",
        "api.example.com",
        "domain not in allowlist",
    ));

    assert!(
        wait_for(&state, |s| !s.permission_prompts.is_empty(), Duration::from_secs(3)).await,
        "subscriber didn't see Pending"
    );

    // Now POST a resolution as the TUI would.
    let client = reqwest::Client::new();
    auto_mode::resolve(&url, &client, &id, ResolutionVerdict::Approve, Scope::Session, Some("*.example.com"))
        .await
        .expect("resolve POST");

    // The SSE stream's Resolved event should flip the prompt's status.
    let resolved = wait_for(
        &state,
        |s| {
            matches!(
                s.permission_prompts.first().map(|p| &p.status),
                Some(PromptStatus::Approved { scope: Scope::Session, .. })
            )
        },
        Duration::from_secs(3),
    )
    .await;
    assert!(resolved, "Resolved event never flipped prompt to Approved");
}

#[tokio::test]
async fn deny_resolution_collapses_to_denied_status() {
    let access = AccessStore::new();
    let url = spawn_server(access.clone()).await;
    let state = Arc::new(Mutex::new(AppState::new(std::env::temp_dir())));

    auto_mode::spawn_subscriber(url.clone(), Arc::clone(&state));
    assert!(wait_for_subscriber(&access, Duration::from_secs(3)).await, "SSE subscriber never registered");

    let (id, _fut) = access.file_pending(NewAccessRequest::with_defaults(
        "pearl",
        "op",
        "network",
        "attacker.example",
        "domain not in allowlist",
    ));

    assert!(
        wait_for(&state, |s| !s.permission_prompts.is_empty(), Duration::from_secs(3)).await,
        "subscriber didn't see Pending"
    );

    let client = reqwest::Client::new();
    auto_mode::resolve(&url, &client, &id, ResolutionVerdict::Deny, Scope::Once, None)
        .await
        .expect("deny POST");

    let resolved = wait_for(
        &state,
        |s| {
            matches!(
                s.permission_prompts.first().map(|p| &p.status),
                Some(PromptStatus::Denied { scope: Scope::Once })
            )
        },
        Duration::from_secs(3),
    )
    .await;
    assert!(resolved, "deny resolution never flipped prompt status");
}

#[tokio::test]
async fn resolve_on_unknown_id_returns_err() {
    let access = AccessStore::new();
    let url = spawn_server(access).await;
    let client = reqwest::Client::new();

    let err = auto_mode::resolve(&url, &client, "no-such-id", ResolutionVerdict::Approve, Scope::Once, None)
        .await
        .expect_err("expected error");
    assert!(err.contains("404"), "expected 404, got: {err}");
}

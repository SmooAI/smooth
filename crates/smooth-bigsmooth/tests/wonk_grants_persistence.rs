//! Integration test for wonk-allow.toml persistence.
//!
//! Wires the full pipeline:
//!  - SharedWonkGrants seeded with one host
//!  - axum router exposing `/api/access/{pending,approve}`
//!  - a pending request filed into the store
//!  - HTTP POST to /api/access/approve with scope=user
//!  - verify: (a) the file got the new grant, (b) the live
//!    SharedWonkGrants snapshot now reflects it, (c) the host can
//!    be matched.
//!
//! Project-scope routing is deliberately out of scope here — the
//! current code routes project grants to the user file with a
//! comment that explicit project-aware routing is a sub-pearl.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use smooth_bigsmooth::access::{AccessError, AccessStore};
use smooth_bigsmooth::wonk_grants::{append_grant, project_grants_path, SharedWonkGrants, WonkGrants};
use smooth_narc::judge::Scope;
use smooth_narc::{AccessResolution, NewAccessRequest, PendingAccessRequest, ResolutionVerdict};
use tempfile::TempDir;

#[derive(Clone)]
struct TestState {
    access: AccessStore,
    grants: SharedWonkGrants,
    grants_path: PathBuf,
}

#[derive(Deserialize)]
struct ResolveBody {
    id: String,
    scope: String,
    #[serde(default)]
    glob_override: Option<String>,
}

async fn pending(State(s): State<TestState>) -> Json<Vec<PendingAccessRequest>> {
    Json(s.access.list_pending())
}

async fn approve(State(s): State<TestState>, Json(body): Json<ResolveBody>) -> Result<Json<AccessResolution>, (axum::http::StatusCode, String)> {
    let scope = Scope::parse(&body.scope).ok_or((axum::http::StatusCode::BAD_REQUEST, format!("bad scope {}", body.scope)))?;
    let snapshot = s.access.list_pending().into_iter().find(|r| r.id == body.id);
    let glob_override = body.glob_override.clone();
    let resolution = s
        .access
        .resolve(&body.id, ResolutionVerdict::Approve, scope, body.glob_override)
        .map_err(|e| match e {
            AccessError::NotFound(id) => (axum::http::StatusCode::NOT_FOUND, id),
            AccessError::Poisoned => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "poisoned".into()),
        })?;

    // Persistence path: same shape as Big Smooth's resolve_access.
    if matches!(scope, Scope::User | Scope::PearlProject) {
        if let Some(req) = snapshot {
            append_grant(&s.grants_path, &req.kind, &req.resource, glob_override.as_deref())
                .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("append: {e}")))?;
            if let Ok(fresh) = WonkGrants::load_from_path(&s.grants_path) {
                s.grants.merge_in(fresh);
            }
        }
    }

    Ok(Json(resolution))
}

async fn spawn_server(state: TestState) -> String {
    let app = Router::new()
        .route("/api/access/pending", get(pending))
        .route("/api/access/approve", post(approve))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn approve_at_user_scope_persists_to_file_and_live_grants() {
    let tmp = TempDir::new().unwrap();
    let grants_path = tmp.path().join("wonk-allow.toml");

    let access = AccessStore::new();
    let grants = SharedWonkGrants::new(WonkGrants::new());

    // File a pending request — same shape Safehouse Narc would file
    // when its judge returns Ask.
    let (id, _fut) = access.file_pending(NewAccessRequest::with_defaults(
        "pearl-1",
        "op-1",
        "network",
        "api.openai.com",
        "domain not in allowlist",
    ));

    let url = spawn_server(TestState {
        access: access.clone(),
        grants: grants.clone(),
        grants_path: grants_path.clone(),
    })
    .await;

    let resp = reqwest::Client::new()
        .post(format!("{url}/api/access/approve"))
        .json(&serde_json::json!({"id": id, "scope": "user", "glob_override": "*.openai.com"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "approve should succeed: {}", resp.text().await.unwrap_or_default());

    // (a) File materialized with the glob_override.
    let on_disk = WonkGrants::load_from_path(&grants_path).expect("load");
    assert!(on_disk.matches_host("api.openai.com"));
    assert!(on_disk.matches_host("foo.openai.com"));

    // (b) Live SharedWonkGrants snapshot includes the new grant.
    let live = grants.snapshot();
    assert!(live.matches_host("api.openai.com"));
    assert!(live.matches_host("foo.openai.com"));
}

#[tokio::test]
async fn approve_at_session_scope_does_not_touch_file() {
    let tmp = TempDir::new().unwrap();
    let grants_path = tmp.path().join("wonk-allow.toml");

    let access = AccessStore::new();
    let grants = SharedWonkGrants::new(WonkGrants::new());

    let (id, _fut) = access.file_pending(NewAccessRequest::with_defaults(
        "pearl-1",
        "op-1",
        "network",
        "ephemeral.example",
        "domain not in allowlist",
    ));

    let url = spawn_server(TestState {
        access: access.clone(),
        grants: grants.clone(),
        grants_path: grants_path.clone(),
    })
    .await;

    let resp = reqwest::Client::new()
        .post(format!("{url}/api/access/approve"))
        .json(&serde_json::json!({"id": id, "scope": "session"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // File never got created.
    assert!(!grants_path.exists(), "session-scope grants should not be persisted");
    // Live grants don't carry it either — session-scope grants live
    // in Wonk's runtime allowlist, not the SharedWonkGrants used for
    // persistent lookups.
    let live = grants.snapshot();
    assert!(!live.matches_host("ephemeral.example"));
}

#[tokio::test]
async fn append_grant_idempotent_on_repeated_approves() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("wonk-allow.toml");
    append_grant(&path, "network", "x.example", None).expect("append 1");
    append_grant(&path, "network", "x.example", None).expect("append 2");
    let g = WonkGrants::load_from_path(&path).expect("load");
    // BTreeSet de-dupes — the grant should appear exactly once.
    assert_eq!(g.network.allow_hosts.len(), 1);
}

#[tokio::test]
async fn project_grants_path_picks_workspace_relative_location() {
    // Smoke test the path helper — keeps the test file the canonical
    // place to look up the convention.
    let p = project_grants_path(std::path::Path::new("/tmp/example-project"));
    assert!(p.ends_with(".smooth/wonk-allow.toml"));
    assert!(p.starts_with("/tmp/example-project"));
}

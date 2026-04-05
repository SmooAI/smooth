//! Integration tests for Big Smooth server + IssueStore + Orchestrator.

use std::time::Instant;

use axum::body::Body;
use axum::Router;
use http_body_util::BodyExt;
use hyper::Request;
use smooth_bigsmooth::db::Database;
use smooth_bigsmooth::orchestrator::{Orchestrator, OrchestratorState};
use smooth_bigsmooth::server::{AppState, build_router};
use smooth_issues::IssueStore;
use tower::ServiceExt;

/// Build a self-contained test app backed by a temp SQLite database.
fn test_app() -> (Router, IssueStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path).expect("open db");
    let issue_store = IssueStore::open(&db_path).expect("open issue store");
    let state = AppState {
        db,
        issue_store: issue_store.clone(),
        start_time: Instant::now(),
    };
    let router = build_router(state);
    // Leak tempdir so it isn't deleted while tests run.
    std::mem::forget(dir);
    (router, issue_store)
}

/// Parse a JSON response body into a `serde_json::Value`.
async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.expect("collect body").to_bytes();
    serde_json::from_slice(&bytes).expect("parse json")
}

// ── 1. Health endpoint ────────────────────────────────────────

#[tokio::test]
async fn health_endpoint() {
    let (app, _store) = test_app();

    let resp = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).expect("request"))
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);

    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["service"], "smooth-leader");
    assert!(body["uptime"].as_f64().is_some());
    assert!(body["timestamp"].as_str().is_some());
}

// ── 2. System health ──────────────────────────────────────────

#[tokio::test]
async fn system_health() {
    let (app, _store) = test_app();

    let resp = app
        .oneshot(Request::builder().uri("/api/system/health").body(Body::empty()).expect("request"))
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);

    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);

    let data = &body["data"];
    assert_eq!(data["leader"]["status"], "healthy");
    assert_eq!(data["database"]["status"], "healthy");
    assert_eq!(data["sandbox"]["status"], "healthy");
    assert!(data["beads"]["open_issues"].as_u64().is_some());
}

// ── 3. Create issue via API ───────────────────────────────────

#[tokio::test]
async fn create_issue_via_api() {
    let (app, _store) = test_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/issues")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"Test issue","description":"Integration test","type":"task","priority":2}"#))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);

    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["title"], "Test issue");
    assert_eq!(body["data"]["description"], "Integration test");
    assert!(body["data"]["id"].as_str().is_some());
}

// ── 4. List issues via API ────────────────────────────────────

#[tokio::test]
async fn list_issues_via_api() {
    let (app, store) = test_app();

    // Seed two issues directly via the store.
    let new = smooth_issues::NewIssue {
        title: "Alpha".into(),
        description: String::new(),
        issue_type: smooth_issues::IssueType::Task,
        priority: smooth_issues::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    store.create(&new).expect("create 1");
    store.create(&new).expect("create 2");

    let resp = app
        .oneshot(Request::builder().uri("/api/issues").body(Body::empty()).expect("request"))
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);

    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);

    let data = body["data"].as_array().expect("data is array");
    assert_eq!(data.len(), 2);
}

// ── 5. Get issue via API ──────────────────────────────────────

#[tokio::test]
async fn get_issue_via_api() {
    let (app, store) = test_app();

    let new = smooth_issues::NewIssue {
        title: "Find me".into(),
        description: "details".into(),
        issue_type: smooth_issues::IssueType::Task,
        priority: smooth_issues::Priority::High,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    let created = store.create(&new).expect("create");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(&format!("/api/issues/{}", created.id))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);

    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["title"], "Find me");
    assert_eq!(body["data"]["description"], "details");
}

// ── 6. Close issue via API ────────────────────────────────────

#[tokio::test]
async fn close_issue_via_api() {
    let (app, store) = test_app();

    let new = smooth_issues::NewIssue {
        title: "Close me".into(),
        description: String::new(),
        issue_type: smooth_issues::IssueType::Task,
        priority: smooth_issues::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    let created = store.create(&new).expect("create");

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/issues/{}/close", created.id))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);

    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["closed"], 1);

    // Verify via store.
    let issue = store.get(&created.id).expect("get").expect("exists");
    assert_eq!(issue.status, smooth_issues::IssueStatus::Closed);
}

// ── 7. Ready issues via API ──────────────────────────────────

#[tokio::test]
async fn ready_issues_via_api() {
    let (app, store) = test_app();

    // Create two issues, close one — only the open one should be "ready".
    let new = smooth_issues::NewIssue {
        title: "Open issue".into(),
        description: String::new(),
        issue_type: smooth_issues::IssueType::Task,
        priority: smooth_issues::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    store.create(&new).expect("create open");

    let closed_new = smooth_issues::NewIssue {
        title: "Closed issue".into(),
        ..new.clone()
    };
    let closed = store.create(&closed_new).expect("create to-close");
    store.close(&[&closed.id]).expect("close");

    let resp = app
        .oneshot(Request::builder().uri("/api/issues/ready").body(Body::empty()).expect("request"))
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);

    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);

    let data = body["data"].as_array().expect("data is array");
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["title"], "Open issue");
}

// ── 8. Stats via API ──────────────────────────────────────────

#[tokio::test]
async fn stats_via_api() {
    let (app, store) = test_app();

    let new = smooth_issues::NewIssue {
        title: "A".into(),
        description: String::new(),
        issue_type: smooth_issues::IssueType::Task,
        priority: smooth_issues::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    store.create(&new).expect("create 1");
    let b = store.create(&smooth_issues::NewIssue { title: "B".into(), ..new.clone() }).expect("create 2");
    store.close(&[&b.id]).expect("close B");

    let resp = app
        .oneshot(Request::builder().uri("/api/issues/stats").body(Body::empty()).expect("request"))
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);

    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["open"], 1);
    assert_eq!(body["data"]["closed"], 1);
    assert_eq!(body["data"]["total"], 2);
}

// ── 9. Beads backward compatibility ──────────────────────────

#[tokio::test]
async fn beads_backward_compat() {
    let (app, store) = test_app();

    let new = smooth_issues::NewIssue {
        title: "Bead compat".into(),
        description: String::new(),
        issue_type: smooth_issues::IssueType::Task,
        priority: smooth_issues::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    let created = store.create(&new).expect("create");

    // /api/beads should list issues (alias for /api/issues).
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/api/beads").body(Body::empty()).expect("request"))
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);
    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);
    let data = body["data"].as_array().expect("array");
    assert_eq!(data.len(), 1);

    // /api/beads/:id should return the specific issue.
    let resp2 = app
        .oneshot(
            Request::builder()
                .uri(&format!("/api/beads/{}", created.id))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(resp2.status(), 200);
    let body2 = json_body(resp2).await;
    assert_eq!(body2["data"]["title"], "Bead compat");
}

// ── 10. Orchestrator schedules ready issues ──────────────────

#[tokio::test]
async fn orchestrator_schedules_ready_issues() {
    let store = IssueStore::open_in_memory().expect("in-memory store");

    // Seed ready issues.
    let new = smooth_issues::NewIssue {
        title: "Ready task 1".into(),
        description: String::new(),
        issue_type: smooth_issues::IssueType::Task,
        priority: smooth_issues::Priority::High,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    store.create(&new).expect("create 1");
    store.create(&smooth_issues::NewIssue { title: "Ready task 2".into(), ..new }).expect("create 2");

    let mut orch = Orchestrator::new(3, store);
    assert_eq!(orch.state_name(), "idle");

    // Step once — should transition from Idle to Scheduling with the 2 ready issues.
    orch.step().await.expect("step");
    assert_eq!(orch.state_name(), "scheduling");

    if let OrchestratorState::Scheduling { ready_beads } = &orch.state {
        assert_eq!(ready_beads.len(), 2);
    } else {
        panic!("expected Scheduling state");
    }
}

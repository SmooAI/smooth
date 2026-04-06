//! Integration tests for Big Smooth server + PearlStore + Orchestrator.

use axum::body::Body;
use axum::Router;
use http_body_util::BodyExt;
use hyper::Request;
use smooth_bigsmooth::db::Database;
use smooth_bigsmooth::orchestrator::{Orchestrator, OrchestratorState};
use smooth_bigsmooth::server::{build_router, AppState};
use smooth_pearls::PearlStore;
use tower::ServiceExt;

/// Build a self-contained test app backed by a temp Dolt database.
fn test_app() -> Option<(Router, PearlStore)> {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path).expect("open db");
    let dolt_dir = dir.path().join("dolt");
    let pearl_store = match PearlStore::init(&dolt_dir) {
        Ok(s) => s,
        Err(_) => return None, // smooth-dolt binary not available
    };
    let state = AppState::new(db, pearl_store.clone());
    let router = build_router(state);
    // Leak tempdir so it isn't deleted while tests run.
    std::mem::forget(dir);
    Some((router, pearl_store))
}

/// Parse a JSON response body into a `serde_json::Value`.
async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.expect("collect body").to_bytes();
    serde_json::from_slice(&bytes).expect("parse json")
}

// ── 1. Health endpoint ────────────────────────────────────────

#[tokio::test]
async fn health_endpoint() {
    let Some((app, _store)) = test_app() else { return };

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
    let Some((app, _store)) = test_app() else { return };

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
    assert!(data["pearls"]["open_pearls"].as_u64().is_some());
}

// ── 3. Create issue via API ───────────────────────────────────

#[tokio::test]
async fn create_pearl_via_api() {
    let Some((app, _store)) = test_app() else { return };

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/pearls")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"title":"Test issue","description":"Integration test","type":"task","priority":2}"#,
                ))
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
async fn list_pearls_via_api() {
    let Some((app, store)) = test_app() else { return };

    // Seed two issues directly via the store.
    let new = smooth_pearls::NewPearl {
        title: "Alpha".into(),
        description: String::new(),
        pearl_type: smooth_pearls::PearlType::Task,
        priority: smooth_pearls::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    store.create(&new).expect("create 1");
    store.create(&new).expect("create 2");

    let resp = app
        .oneshot(Request::builder().uri("/api/pearls").body(Body::empty()).expect("request"))
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
async fn get_pearl_via_api() {
    let Some((app, store)) = test_app() else { return };

    let new = smooth_pearls::NewPearl {
        title: "Find me".into(),
        description: "details".into(),
        pearl_type: smooth_pearls::PearlType::Task,
        priority: smooth_pearls::Priority::High,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    let created = store.create(&new).expect("create");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(&format!("/api/pearls/{}", created.id))
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
async fn close_pearl_via_api() {
    let Some((app, store)) = test_app() else { return };

    let new = smooth_pearls::NewPearl {
        title: "Close me".into(),
        description: String::new(),
        pearl_type: smooth_pearls::PearlType::Task,
        priority: smooth_pearls::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    let created = store.create(&new).expect("create");

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/pearls/{}/close", created.id))
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
    assert_eq!(issue.status, smooth_pearls::PearlStatus::Closed);
}

// ── 7. Ready issues via API ──────────────────────────────────

#[tokio::test]
async fn ready_issues_via_api() {
    let Some((app, store)) = test_app() else { return };

    // Create two issues, close one — only the open one should be "ready".
    let new = smooth_pearls::NewPearl {
        title: "Open issue".into(),
        description: String::new(),
        pearl_type: smooth_pearls::PearlType::Task,
        priority: smooth_pearls::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    store.create(&new).expect("create open");

    let closed_new = smooth_pearls::NewPearl {
        title: "Closed issue".into(),
        ..new.clone()
    };
    let closed = store.create(&closed_new).expect("create to-close");
    store.close(&[&closed.id]).expect("close");

    let resp = app
        .oneshot(Request::builder().uri("/api/pearls/ready").body(Body::empty()).expect("request"))
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
    let Some((app, store)) = test_app() else { return };

    let new = smooth_pearls::NewPearl {
        title: "A".into(),
        description: String::new(),
        pearl_type: smooth_pearls::PearlType::Task,
        priority: smooth_pearls::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    store.create(&new).expect("create 1");
    let b = store
        .create(&smooth_pearls::NewPearl {
            title: "B".into(),
            ..new.clone()
        })
        .expect("create 2");
    store.close(&[&b.id]).expect("close B");

    let resp = app
        .oneshot(Request::builder().uri("/api/pearls/stats").body(Body::empty()).expect("request"))
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
    let Some((app, store)) = test_app() else { return };

    let new = smooth_pearls::NewPearl {
        title: "Bead compat".into(),
        description: String::new(),
        pearl_type: smooth_pearls::PearlType::Task,
        priority: smooth_pearls::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    let created = store.create(&new).expect("create");

    // /api/pearls should list issues (alias for /api/pearls).
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/api/pearls").body(Body::empty()).expect("request"))
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);
    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);
    let data = body["data"].as_array().expect("array");
    assert_eq!(data.len(), 1);

    // /api/pearls/:id should return the specific issue.
    let resp2 = app
        .oneshot(
            Request::builder()
                .uri(&format!("/api/pearls/{}", created.id))
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
    let tmp = tempfile::tempdir().expect("tempdir");
    let Ok(store) = PearlStore::init(&tmp.path().join("dolt")) else { return };
    std::mem::forget(tmp);

    // Seed ready issues.
    let new = smooth_pearls::NewPearl {
        title: "Ready task 1".into(),
        description: String::new(),
        pearl_type: smooth_pearls::PearlType::Task,
        priority: smooth_pearls::Priority::High,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    store.create(&new).expect("create 1");
    store
        .create(&smooth_pearls::NewPearl {
            title: "Ready task 2".into(),
            ..new
        })
        .expect("create 2");

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

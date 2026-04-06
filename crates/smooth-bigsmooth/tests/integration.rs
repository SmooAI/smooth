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

// ── 11. Pearl CRUD lifecycle through Dolt ──────────────────────

#[tokio::test]
async fn pearl_full_lifecycle_through_dolt() {
    let Some((app, store)) = test_app() else { return };

    // Create via store (simulating what dispatch does)
    let pearl = store
        .create(&smooth_pearls::NewPearl {
            title: "E2E lifecycle pearl".into(),
            description: "Tests full Dolt lifecycle".into(),
            pearl_type: smooth_pearls::PearlType::Task,
            priority: smooth_pearls::Priority::High,
            assigned_to: None,
            parent_id: None,
            labels: vec!["e2e".into()],
        })
        .expect("create pearl");

    assert!(pearl.id.starts_with("th-"));
    assert_eq!(pearl.status, smooth_pearls::PearlStatus::Open);
    assert_eq!(pearl.labels, vec!["e2e"]);

    // Update via API
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(&format!("/api/pearls/{}", pearl.id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"in_progress"}"#))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), 200);

    // Verify via store
    let updated = store.get(&pearl.id).expect("get").expect("exists");
    assert_eq!(updated.status, smooth_pearls::PearlStatus::InProgress);

    // Add comment
    let comment = store.add_comment(&pearl.id, "Working on this now").expect("comment");
    assert!(comment.id.starts_with("th-"));

    // Add dependency
    let blocker = store
        .create(&smooth_pearls::NewPearl {
            title: "Blocker pearl".into(),
            description: String::new(),
            pearl_type: smooth_pearls::PearlType::Task,
            priority: smooth_pearls::Priority::Medium,
            assigned_to: None,
            parent_id: None,
            labels: Vec::new(),
        })
        .expect("create blocker");
    store.add_dep(&pearl.id, &blocker.id).expect("add dep");

    // Pearl should be blocked
    let blocked = store.blocked().expect("blocked");
    assert!(blocked.iter().any(|p| p.id == pearl.id));

    // Close blocker, pearl should be unblocked
    store.close(&[&blocker.id]).expect("close blocker");
    let blocked_after = store.blocked().expect("blocked after");
    assert!(!blocked_after.iter().any(|p| p.id == pearl.id));

    // Close pearl via API
    let resp2 = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/pearls/{}/close", pearl.id))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp2.status(), 200);

    // Verify closed
    let closed = store.get(&pearl.id).expect("get").expect("exists");
    assert_eq!(closed.status, smooth_pearls::PearlStatus::Closed);
    assert!(closed.closed_at.is_some());

    // Verify history via Dolt
    let history = store.get_history(&pearl.id).expect("history");
    assert!(!history.is_empty(), "Dolt should have recorded field change history");
    let status_changes: Vec<_> = history.iter().filter(|h| h.field == "status").collect();
    assert!(!status_changes.is_empty(), "Should have status change history");

    // Verify Dolt commit log has entries
    let log = store.dolt_log(10).expect("dolt log");
    assert!(!log.is_empty(), "Dolt should have commit history from pearl mutations");

    // Verify stats
    let stats = store.stats().expect("stats");
    assert_eq!(stats.closed, 2); // pearl + blocker
    assert_eq!(stats.total, 2);
}

// ── 12. Session messages saved in Dolt ─────────────────────────

#[tokio::test]
async fn session_messages_saved_in_dolt() {
    let Some((_app, store)) = test_app() else { return };

    // Use the DoltSessionStore directly
    use smooth_bigsmooth::session::{MessageType, SessionMessage, SessionStore};
    let session_store = smooth_bigsmooth::session::DoltSessionStore::new(&store);

    let session_id = "test-session-001";

    // Save messages
    session_store
        .save_message(SessionMessage {
            id: "msg-1".into(),
            session_id: session_id.into(),
            from: "user".into(),
            to: "bigsmooth".into(),
            content: "Write a Rust function to add two numbers".into(),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Command,
        })
        .expect("save msg 1");

    session_store
        .save_message(SessionMessage {
            id: "msg-2".into(),
            session_id: session_id.into(),
            from: "bigsmooth".into(),
            to: "operator-1".into(),
            content: "Dispatching Rust task to operator-1".into(),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Command,
        })
        .expect("save msg 2");

    session_store
        .save_message(SessionMessage {
            id: "msg-3".into(),
            session_id: session_id.into(),
            from: "operator-1".into(),
            to: "bigsmooth".into(),
            content: "Task completed. 12/12 tests pass.".into(),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Response,
        })
        .expect("save msg 3");

    // Retrieve messages
    let msgs = session_store.get_messages(session_id, 10).expect("get messages");
    assert_eq!(msgs.len(), 3, "should have 3 messages in session");
    // Messages may come back in any order when timestamps are identical (Dolt NOW() resolution)
    let ids: Vec<&str> = msgs.iter().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&"msg-1"), "should contain msg-1");
    assert!(ids.contains(&"msg-2"), "should contain msg-2");
    assert!(ids.contains(&"msg-3"), "should contain msg-3");
    let msg1 = msgs.iter().find(|m| m.id == "msg-1").unwrap();
    assert_eq!(msg1.from, "user");
    let msg3 = msgs.iter().find(|m| m.id == "msg-3").unwrap();
    assert_eq!(msg3.message_type, MessageType::Response);

    // Limit works
    let limited = session_store.get_messages(session_id, 2).expect("limited");
    assert_eq!(limited.len(), 2, "limit should cap at 2");

    // Different session is empty
    let other = session_store.get_messages("other-session", 10).expect("other session");
    assert!(other.is_empty());
}

// ── 13. Orchestrator snapshots saved in Dolt ───────────────────

#[tokio::test]
async fn orchestrator_snapshots_saved_in_dolt() {
    let Some((_app, store)) = test_app() else { return };

    use smooth_bigsmooth::session::{OrchestratorSnapshot, SessionStatus, SessionStore};
    let session_store = smooth_bigsmooth::session::DoltSessionStore::new(&store);

    // Save a snapshot
    session_store
        .save_snapshot(OrchestratorSnapshot {
            session_id: "sess-001".into(),
            bead_id: "th-abc123".into(),
            phase: "Monitoring".into(),
            operator_id: "operator-1".into(),
            dispatched_at: chrono::Utc::now(),
            last_checkpoint_id: None,
            status: SessionStatus::Active,
        })
        .expect("save snapshot");

    // Retrieve it
    let snap = session_store.get_snapshot("sess-001").expect("get").expect("exists");
    assert_eq!(snap.bead_id, "th-abc123");
    assert_eq!(snap.phase, "Monitoring");
    assert_eq!(snap.status, SessionStatus::Active);

    // List active sessions
    let active = session_store.list_active_sessions().expect("list active");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].session_id, "sess-001");

    // Mark completed
    session_store.mark_completed("sess-001").expect("mark completed");
    let snap2 = session_store.get_snapshot("sess-001").expect("get").expect("exists");
    assert_eq!(snap2.status, SessionStatus::Completed);

    // No longer in active list
    let active2 = session_store.list_active_sessions().expect("list active after");
    assert!(active2.is_empty());
}

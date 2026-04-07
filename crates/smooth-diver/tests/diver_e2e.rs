//! End-to-end test for the Diver pearl lifecycle.
//!
//! Exercises the full dispatch → sub-pearl → cost → complete cycle
//! through the Diver HTTP API backed by a real Dolt pearl store.
//! No VMs needed — this tests the Diver service in isolation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use smooth_diver::server::{build_router_with_state, AppState};
use smooth_diver::store::{CostEntry, DispatchResult, DiverStore};
use smooth_pearls::{PearlStatus, PearlStore};
use tower::ServiceExt;

fn test_app() -> Option<(axum::Router, Arc<DiverStore>)> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dolt_dir = tmp.path().join("dolt");
    let pearl_store = match PearlStore::init(&dolt_dir) {
        Ok(s) => s,
        Err(_) => return None, // smooth-dolt binary not available
    };
    let diver_store = Arc::new(DiverStore::new(pearl_store));
    let state = AppState {
        store: Arc::clone(&diver_store),
        jira: None,
    };
    let router = build_router_with_state(state);
    std::mem::forget(tmp);
    Some((router, diver_store))
}

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    serde_json::from_slice(&bytes).expect("parse json")
}

// ── Full lifecycle: dispatch → sub-pearl → cost → complete ────────

#[tokio::test]
async fn diver_full_lifecycle() {
    let Some((app, store)) = test_app() else {
        eprintln!("SKIP: smooth-dolt binary not available");
        return;
    };

    // 1. Dispatch a task
    let dispatch_body = serde_json::json!({
        "title": "Implement login page",
        "description": "Build OAuth2 login with Google and GitHub providers",
        "pearl_type": "feature",
        "priority": 1,
        "labels": ["auth", "frontend"],
        "operator_id": "op-rust-1"
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dispatch")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&dispatch_body).unwrap()))
                .unwrap(),
        )
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::CREATED, "dispatch should return 201");

    let result: DispatchResult = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let parent_id = result.pearl.id.clone();
    assert_eq!(result.pearl.title, "Implement login page");
    assert_eq!(result.pearl.status, PearlStatus::InProgress);
    assert_eq!(result.pearl.assigned_to.as_deref(), Some("op-rust-1"));
    eprintln!("✓ Dispatched pearl: {parent_id}");

    // 2. Verify via GET /pearl/:id
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(format!("/pearl/{parent_id}")).body(Body::empty()).unwrap())
        .await
        .expect("get pearl");
    assert_eq!(resp.status(), StatusCode::OK);
    let pearl_json = json_body(resp).await;
    assert_eq!(pearl_json["status"], "in_progress");
    eprintln!("✓ GET pearl confirms in_progress");

    // 3. Create sub-pearls (operator breaking work into subtasks)
    for (title, desc) in [
        ("Set up OAuth2 client", "Configure Google/GitHub OAuth2 credentials"),
        ("Build login UI component", "React component with provider buttons"),
        ("Add session management", "JWT tokens + refresh flow"),
    ] {
        let sub_body = serde_json::json!({
            "parent_id": parent_id,
            "title": title,
            "description": desc
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sub-pearl")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&sub_body).unwrap()))
                    .unwrap(),
            )
            .await
            .expect("sub-pearl");
        assert_eq!(resp.status(), StatusCode::CREATED, "sub-pearl should return 201");
        let sub_json = json_body(resp).await;
        assert_eq!(sub_json["parent_id"], parent_id);
        eprintln!("✓ Created sub-pearl: {title}");
    }

    // 4. Verify children via store
    let children = store.children(&parent_id).expect("children");
    assert_eq!(children.len(), 3, "should have 3 sub-pearls");
    eprintln!("✓ Parent has 3 children");

    // 5. Record costs (simulating LLM usage during agent work)
    for (i, cost) in [0.03, 0.05, 0.02].iter().enumerate() {
        let cost_body = serde_json::json!({
            "pearl_id": parent_id,
            "operator_id": "op-rust-1",
            "cost_usd": cost,
            "tokens_in": 500 + i * 100,
            "tokens_out": 200 + i * 50,
            "model": "gpt-4o",
            "timestamp": chrono::Utc::now()
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pearl/{parent_id}/cost"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&cost_body).unwrap()))
                    .unwrap(),
            )
            .await
            .expect("cost");
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    // Record cost on a sub-pearl too
    let child_id = &children[0].id;
    let cost_body = serde_json::json!({
        "pearl_id": child_id,
        "operator_id": "op-rust-1",
        "cost_usd": 0.04,
        "tokens_in": 300,
        "tokens_out": 150,
        "model": "gpt-4o",
        "timestamp": chrono::Utc::now()
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/pearl/{child_id}/cost"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&cost_body).unwrap()))
                .unwrap(),
        )
        .await
        .expect("child cost");
    assert_eq!(resp.status(), StatusCode::CREATED);
    eprintln!("✓ Recorded 4 cost entries");

    // 6. Verify costs
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(format!("/pearl/{parent_id}/costs")).body(Body::empty()).unwrap())
        .await
        .expect("get costs");
    let costs: Vec<CostEntry> = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(costs.len(), 3, "parent should have 3 direct cost entries");

    let total = store.total_cost(&parent_id);
    assert!((total - 0.14).abs() < 0.001, "total cost (including child) should be ~0.14, got {total}");
    eprintln!("✓ Total cost with sub-pearls: ${total:.2}");

    // 7. List all pearls — should show 4 (1 parent + 3 children)
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/pearls").body(Body::empty()).unwrap())
        .await
        .expect("list pearls");
    let all_pearls: Vec<serde_json::Value> = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(all_pearls.len(), 4, "should have 4 total pearls");
    eprintln!("✓ Listed 4 pearls");

    // 8. Complete the task
    let complete_body = serde_json::json!({
        "pearl_id": parent_id,
        "summary": "Login page implemented with Google and GitHub OAuth2. All 3 subtasks completed.",
        "cost_usd": 0.14
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/complete")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&complete_body).unwrap()))
                .unwrap(),
        )
        .await
        .expect("complete");
    assert_eq!(resp.status(), StatusCode::OK, "complete should return 200");
    let completed_json = json_body(resp).await;
    assert_eq!(completed_json["status"], "closed");
    eprintln!("✓ Pearl completed and closed");

    // 9. Verify final state via store
    let pearl = store.get(&parent_id).expect("get").expect("pearl exists");
    assert_eq!(pearl.status, PearlStatus::Closed);

    // Verify comments include the completion summary
    let comments = store.pearl_store().get_comments(&parent_id).expect("comments");
    assert!(!comments.is_empty(), "should have completion summary comment");
    assert!(comments.iter().any(|c| c.content.contains("Login page implemented")));
    eprintln!("✓ Completion summary saved as comment");

    // 10. Filter by status
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/pearls?status=closed").body(Body::empty()).unwrap())
        .await
        .expect("list closed");
    let closed: Vec<serde_json::Value> = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(closed.len(), 1, "only the parent was completed");

    let resp = app
        .oneshot(Request::builder().uri("/pearls?status=open").body(Body::empty()).unwrap())
        .await
        .expect("list open");
    let open: Vec<serde_json::Value> = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(open.len(), 3, "3 sub-pearls still open");
    eprintln!("✓ Status filtering correct: 1 closed, 3 open");

    eprintln!("\n🎯 Diver E2E lifecycle test PASSED");
}

// ── Error cases ───────────────────────────────────────────────────

#[tokio::test]
async fn dispatch_minimal_fields() {
    let Some((app, _store)) = test_app() else { return };

    // Only required fields
    let body = serde_json::json!({
        "title": "Quick fix",
        "description": "Fix the typo"
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dispatch")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let result: DispatchResult = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(result.pearl.priority.as_u8(), 2); // default
}

#[tokio::test]
async fn get_nonexistent_pearl_returns_404() {
    let Some((app, _store)) = test_app() else { return };
    let resp = app
        .oneshot(Request::builder().uri("/pearl/th-000000").body(Body::empty()).unwrap())
        .await
        .expect("get");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sub_pearl_with_invalid_parent_returns_400() {
    let Some((app, _store)) = test_app() else { return };
    let body = serde_json::json!({
        "parent_id": "th-nonexistent",
        "title": "Orphan",
        "description": "No parent"
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sub-pearl")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .expect("sub-pearl");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

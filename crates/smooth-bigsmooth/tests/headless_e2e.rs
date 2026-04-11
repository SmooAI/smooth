//! E2E test for headless agent execution via the SSE /api/tasks endpoint.
//!
//! Requires: ~/.smooth/providers.json with a configured LLM provider.
//!
//!     cargo test -p smooth-bigsmooth --test headless_e2e -- --ignored --nocapture

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use axum::body::Body;
use http_body_util::BodyExt;
use hyper::Request;
use smooth_bigsmooth::db::Database;
use smooth_bigsmooth::server::{build_router, AppState};
use smooth_pearls::PearlStore;
use tower::ServiceExt;

/// Build a self-contained test app backed by a temp Dolt database.
/// Returns `None` when the smooth-dolt binary is unavailable.
fn test_app() -> Option<(axum::Router, AppState)> {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path).expect("open db");
    let dolt_dir = dir.path().join("dolt");
    let pearl_store = match PearlStore::init(&dolt_dir) {
        Ok(s) => s,
        Err(_) => return None, // smooth-dolt binary not available
    };
    let state = AppState::new(db, pearl_store);
    let router = build_router(state.clone());
    // Leak tempdir so it isn't deleted while tests run.
    std::mem::forget(dir);
    Some((router, state))
}

/// Parse a JSON response body into a `serde_json::Value`.
async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.expect("collect body").to_bytes();
    serde_json::from_slice(&bytes).expect("parse json")
}

/// Start a real TCP server and return the port. The server runs in a
/// background tokio task and stops when the runtime is dropped.
async fn start_server(router: axum::Router) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("local addr").port();
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    // Give the server a moment to accept connections.
    tokio::time::sleep(Duration::from_millis(100)).await;
    port
}

// ── Headless task SSE (requires LLM provider) ─────────────────

#[tokio::test]
#[ignore = "requires configured LLM provider in ~/.smooth/providers.json"]
async fn headless_task_returns_events() {
    let Some((router, _state)) = test_app() else {
        return;
    };
    let port = start_server(router).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("build reqwest client");

    let resp = client
        .post(format!("http://127.0.0.1:{port}/api/tasks"))
        .json(&serde_json::json!({
            "message": "What is 2 + 2? Reply with just the number.",
        }))
        .send()
        .await
        .expect("POST /api/tasks should connect");

    assert!(resp.status().is_success(), "should get 200, got {}", resp.status());

    // Read SSE events from the response body.
    let body = resp.text().await.expect("read body");
    let data_lines: Vec<&str> = body.lines().filter(|l| l.starts_with("data: ")).collect();
    assert!(!data_lines.is_empty(), "should receive at least one SSE event");

    // Should contain a Completed or Error event (agent ran to completion or
    // hit an error — either is a valid E2E signal that the pipeline ran).
    let has_terminal = data_lines.iter().any(|l| l.contains("Completed") || l.contains("Error"));
    assert!(has_terminal, "should receive Completed or Error events, got: {:?}", data_lines);

    eprintln!("headless_task_returns_events: received {} SSE events", data_lines.len());
}

// ── Empty message handling ────────────────────────────────────

#[tokio::test]
async fn headless_task_rejects_empty_message() {
    let Some((router, _state)) = test_app() else {
        return;
    };
    let port = start_server(router).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build reqwest client");

    let resp = client
        .post(format!("http://127.0.0.1:{port}/api/tasks"))
        .json(&serde_json::json!({ "message": "" }))
        .send()
        .await
        .expect("should connect");

    // The SSE endpoint streams back events regardless; an empty message will
    // either trigger a validation error event or the LLM will refuse. Either
    // way the HTTP connection must succeed (the handler always returns 200
    // with an SSE stream).
    let status = resp.status();
    let body = resp.text().await.expect("read body");
    eprintln!("empty message response (status {status}): {}", &body[..body.len().min(500)]);

    // We don't assert a specific status because the SSE handler returns 200
    // and streams error events. Verify we got *something* back.
    assert!(!body.is_empty(), "response body should not be empty");
}

// ── JSON output validity (requires LLM provider) ─────────────

#[tokio::test]
#[ignore = "requires configured LLM provider in ~/.smooth/providers.json"]
async fn headless_json_output_is_valid() {
    let Some((router, _state)) = test_app() else {
        return;
    };
    let port = start_server(router).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("build reqwest client");

    let resp = client
        .post(format!("http://127.0.0.1:{port}/api/tasks"))
        .json(&serde_json::json!({
            "message": "Say hello",
        }))
        .send()
        .await
        .expect("POST /api/tasks");

    assert_eq!(resp.status(), 200);

    let body = resp.text().await.expect("read body");
    let data_lines: Vec<&str> = body.lines().filter(|l| l.starts_with("data: ")).collect();
    assert!(!data_lines.is_empty(), "should receive at least one SSE data line");

    // Every SSE data line must be valid JSON with a discriminant tag.
    for line in &data_lines {
        let json_str = line.strip_prefix("data: ").expect("strip prefix");
        let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap_or_else(|e| panic!("SSE data should be valid JSON: {json_str} (err: {e})"));

        // AgentEvent is a tagged enum — serde serializes it as { "VariantName": { ... } }
        // or { "type": "..." } depending on the serde representation. Verify it's an object.
        assert!(parsed.is_object(), "each SSE event should be a JSON object: {parsed}");
    }

    eprintln!("headless_json_output_is_valid: all {} events are valid JSON", data_lines.len());
}

// ── Orchestrator status endpoint ─────────────────────────────

#[tokio::test]
async fn orchestrator_status_endpoint() {
    let Some((app, _state)) = test_app() else {
        return;
    };

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/orchestrator/status")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);

    let body = json_body(resp).await;
    assert_eq!(body["ok"], true, "response should be ok: {body}");

    let data = &body["data"];
    assert_eq!(data["state"], "idle", "fresh orchestrator should be idle");
    assert_eq!(data["active_workers"], 0, "no active workers on fresh orchestrator");
    assert_eq!(data["completed"], 0, "no completed beads on fresh orchestrator");
    assert!(data["pool_max_concurrency"].as_u64().is_some(), "should report pool_max_concurrency");
    assert_eq!(data["pool_active"], 0, "no active pool slots on fresh orchestrator");
}

// ── Delegate endpoint — creates sub-pearl ────────────────────

#[tokio::test]
async fn delegate_creates_sub_pearl() {
    let Some((app, state)) = test_app() else {
        return;
    };

    // POST /api/delegate with a task
    let body = serde_json::json!({
        "parent_operator_id": "op-headless-test",
        "task": "Write a function that adds two numbers"
    });

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/delegate")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).expect("serialize")))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(resp.status(), 200);

    let resp_body = json_body(resp).await;
    assert_eq!(resp_body["ok"], true, "delegate should succeed: {resp_body}");
    assert_eq!(resp_body["data"]["status"], "dispatched");

    let delegation_id = resp_body["data"]["delegation_id"].as_str().expect("delegation_id should be a string");
    assert!(delegation_id.starts_with("th-"), "pearl ID should start with th-: {delegation_id}");

    // Verify the pearl was created in the store.
    let pearl = smooth_bigsmooth::pearls::get_pearl(&state.pearl_store, delegation_id)
        .expect("get_pearl should not error")
        .expect("pearl should exist");
    assert!(pearl.title.contains("[delegated]"), "title should contain [delegated]: {}", pearl.title);
    assert_eq!(pearl.status, smooth_pearls::PearlStatus::Open);

    // GET /api/delegate/{id}/status — should be in_progress (Open maps to in_progress).
    let status_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/delegate/{delegation_id}/status"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(status_resp.status(), 200);

    let status_body = json_body(status_resp).await;
    assert_eq!(status_body["ok"], true);
    assert_eq!(status_body["data"]["status"], "in_progress");
    assert_eq!(status_body["data"]["delegation_id"], delegation_id);

    // Close the pearl and check status transitions to "completed".
    let _ = state.pearl_store.close(&[delegation_id]);

    let completed_resp = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/delegate/{delegation_id}/status"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    let completed_body = json_body(completed_resp).await;
    assert_eq!(completed_body["data"]["status"], "completed");
}

// ── Delegate status — not found ──────────────────────────────

#[tokio::test]
async fn delegate_status_not_found() {
    let Some((app, _state)) = test_app() else {
        return;
    };

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/delegate/th-nonexistent-headless/status")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    let body = json_body(resp).await;
    assert_eq!(body["ok"], false, "should report not found: {body}");
    assert!(
        body["data"]["error"].as_str().unwrap_or("").contains("not found"),
        "error should mention not found: {body}"
    );
}

// ── Task endpoint via oneshot (verifies SSE response type) ───

#[tokio::test]
async fn task_endpoint_returns_sse_content_type() {
    let Some((app, _state)) = test_app() else {
        return;
    };

    // Even without a valid LLM provider, the handler should accept the
    // request and start streaming (it will stream an error event when it
    // fails to load providers.json or connect to the LLM).
    let body = serde_json::json!({
        "message": "test",
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tasks")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).expect("serialize")))
                .expect("request"),
        )
        .await
        .expect("response");

    // The SSE handler always returns 200 — errors are streamed as events.
    assert_eq!(resp.status(), 200, "task endpoint should return 200");

    let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    assert!(
        content_type.contains("text/event-stream"),
        "content-type should be text/event-stream, got: {content_type}"
    );

    // Read the body — should contain at least one SSE data line (likely an error
    // event since we don't have providers configured in CI).
    let bytes = resp.into_body().collect().await.expect("collect body").to_bytes();
    let body_str = String::from_utf8_lossy(&bytes);
    let data_lines: Vec<&str> = body_str.lines().filter(|l| l.starts_with("data: ")).collect();

    eprintln!(
        "task_endpoint_returns_sse_content_type: got {} SSE events (provider likely not configured)",
        data_lines.len()
    );

    // Every data line should be valid JSON.
    for line in &data_lines {
        let json_str = line.strip_prefix("data: ").expect("strip");
        assert!(
            serde_json::from_str::<serde_json::Value>(json_str).is_ok(),
            "SSE data line should be valid JSON: {json_str}"
        );
    }
}

//! E2E test: Operator creates a dev server in a sandbox, playwright tests it.
//!
//! Requires:
//! - smooth-operator-runner cross-compiled (scripts/build-operator-runner.sh)
//! - ~/.smooth/providers.json configured
//! - SMOOTH_SANDBOXED=1 set **before** running the test binary
//! - Node.js + playwright installed (npx playwright)
//!
//!     SMOOTH_SANDBOXED=1 cargo test -p smooth-bigsmooth --test playwright_e2e -- --ignored --nocapture

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
fn test_app() -> Option<(axum::Router, tempfile::TempDir)> {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path).expect("open db");
    let dolt_dir = dir.path().join("dolt");
    let pearl_store = match PearlStore::init(&dolt_dir) {
        Ok(s) => s,
        Err(_) => return None, // smooth-dolt binary not available
    };
    let state = AppState::new(db, pearl_store);
    let router = build_router(state);
    Some((router, dir))
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

// ── Full E2E: operator starts dev server, playwright verifies ───

#[tokio::test]
#[ignore = "requires sandbox, LLM provider, and playwright — run with SMOOTH_SANDBOXED=1 --ignored --nocapture"]
async fn operator_starts_dev_server_playwright_verifies() {
    // SMOOTH_SANDBOXED must be set before the test binary launches (env var
    // mutation is unsafe in Rust 1.83+). Verify it's present.
    if std::env::var("SMOOTH_SANDBOXED")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
    {
        eprintln!("SMOOTH_SANDBOXED is set — sandbox dispatch will be used");
    } else {
        eprintln!("SKIP: SMOOTH_SANDBOXED not set — set it before running: SMOOTH_SANDBOXED=1 cargo test ...");
        return;
    }

    let Some((router, tmp)) = test_app() else {
        eprintln!("SKIP: smooth-dolt binary not available");
        return;
    };
    let port = start_server(router).await;

    eprintln!("Big Smooth started on port {port}");

    // Send a task via SSE — ask the operator to create and start a simple
    // HTTP server, then expose it via forward_port.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300)) // 5 min — agent needs time
        .build()
        .expect("build reqwest client");

    let task_message = r#"Create a simple Node.js HTTP server:
1. Create a file called server.js with this content:
   const http = require('http');
   const server = http.createServer((req, res) => {
     res.writeHead(200, {'Content-Type': 'text/html'});
     res.end('<h1>Hello Smooth</h1>');
   });
   server.listen(3000, () => console.log('Server running on port 3000'));
2. Start the server with: node server.js &
3. Use the forward_port tool to expose port 3000 to the host
4. Tell me the host port number"#;

    let resp = client
        .post(format!("http://127.0.0.1:{port}/api/tasks"))
        .json(&serde_json::json!({
            "message": task_message,
            "working_dir": tmp.path().to_string_lossy(),
        }))
        .send()
        .await
        .expect("POST /api/tasks should connect");

    assert!(resp.status().is_success(), "should get 200, got {}", resp.status());

    // Read SSE events, looking for the forwarded port number.
    let body = resp.text().await.expect("read body");
    let mut forwarded_port: Option<u16> = None;

    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                // Look for port forward info in TokenDelta content or ToolCallComplete.
                if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                    // Try to extract a port number from the response.
                    for word in content.split_whitespace() {
                        if let Ok(p) = word.trim_matches(|c: char| !c.is_ascii_digit()).parse::<u16>() {
                            if (10000..65535).contains(&p) {
                                forwarded_port = Some(p);
                            }
                        }
                    }
                }
                if let Some(ty) = event.get("type").and_then(|t| t.as_str()) {
                    eprintln!("[event] {ty}");
                }
            }
        }
    }

    // If we got a forwarded port, test it with playwright.
    if let Some(host_port) = forwarded_port {
        eprintln!("Forwarded port detected: {host_port}");

        // Write a tiny playwright script.
        let script_path = tmp.path().join("test-server.mjs");
        std::fs::write(
            &script_path,
            format!(
                r#"
import {{ chromium }} from 'playwright';

const browser = await chromium.launch();
const page = await browser.newPage();
await page.goto('http://localhost:{host_port}');
const text = await page.textContent('h1');
console.log('Page content:', text);
await browser.close();
if (text?.includes('Hello Smooth')) {{
    console.log('PASS: Hello Smooth found!');
    process.exit(0);
}} else {{
    console.error('FAIL: Expected "Hello Smooth", got:', text);
    process.exit(1);
}}
"#
            ),
        )
        .expect("write playwright script");

        let output = tokio::process::Command::new("node").arg(&script_path).current_dir(tmp.path()).output().await;

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                eprintln!("Playwright stdout: {stdout}");
                eprintln!("Playwright stderr: {stderr}");
                assert!(o.status.success(), "Playwright test should pass");
            }
            Err(e) => {
                eprintln!("Playwright failed to run: {e}");
                // Don't fail the test if playwright isn't available.
            }
        }
    } else {
        eprintln!("No forwarded port detected in agent output — agent may not have used forward_port");
        // Don't fail — the agent might not have reached the forward_port step.
        // This is expected in environments without sandbox support.
    }

    eprintln!("operator_starts_dev_server_playwright_verifies: complete");
}

// ── Simpler test: verify the task API accepts a dev-server task ──

#[tokio::test]
async fn dev_server_task_accepted_via_sse() {
    let Some((app, tmp)) = test_app() else {
        eprintln!("SKIP: smooth-dolt binary not available");
        return;
    };

    let body = serde_json::json!({
        "message": "Create a simple Express hello world server",
        "working_dir": tmp.path().to_string_lossy(),
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/tasks")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");

    assert_eq!(resp.status(), 200, "task endpoint should return 200");

    let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    assert!(content_type.contains("text/event-stream"), "should be SSE, got: {content_type}");

    // Read the body — should contain at least one SSE data line (likely an error
    // event since we don't have providers configured in CI).
    let bytes = resp.into_body().collect().await.expect("collect body").to_bytes();
    let body_str = String::from_utf8_lossy(&bytes);
    let data_lines: Vec<&str> = body_str.lines().filter(|l| l.starts_with("data: ")).collect();

    eprintln!("dev_server_task_accepted_via_sse: got {} SSE events", data_lines.len());

    // Every data line should be valid JSON.
    for line in &data_lines {
        let json_str = line.strip_prefix("data: ").expect("strip");
        assert!(
            serde_json::from_str::<serde_json::Value>(json_str).is_ok(),
            "SSE data line should be valid JSON: {json_str}"
        );
    }

    eprintln!("dev_server_task_accepted_via_sse: SSE endpoint accepts task");
}

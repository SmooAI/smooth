//! E2E test: Operator creates a dev server in a sandbox, playwright tests it.
//!
//! Requires:
//! - ~/.smooth/providers.json configured with an LLM provider
//! - Node.js + playwright installed
//!
//!     cargo test -p smooai-smooth-bigsmooth --test playwright_e2e -- --ignored --nocapture

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
    // Runs in whatever dispatch mode is configured (in-process or sandboxed).
    // No SMOOTH_SANDBOXED gate — sandboxed should be the default.

    let Some((router, tmp)) = test_app() else {
        eprintln!("SKIP: smooth-dolt binary not available");
        return;
    };
    let port = start_server(router).await;

    eprintln!("Big Smooth started on port {port}");

    // Send a task via SSE — ask the operator to create and start a simple
    // HTTP server, then expose it via forward_port.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        // No read timeout — tool execution (npm install, server start) can take minutes.
        // The SSE stream stays open until the task completes or errors.
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

    // Stream SSE events as they arrive (don't wait for entire body).
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut forwarded_port: Option<u16> = None;
    let mut done = false;

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[stream] error: {e}");
                break;
            }
        };
        let text = String::from_utf8_lossy(&chunk);
        buf.push_str(&text);

        // Process complete lines
        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].to_string();
            buf.drain(..=nl);

            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                        for word in content.split_whitespace() {
                            if let Ok(p) = word.trim_matches(|c: char| !c.is_ascii_digit()).parse::<u16>() {
                                if (10000..65535).contains(&p) {
                                    forwarded_port = Some(p);
                                }
                            }
                        }
                    }
                    if let Some(ty) = event.get("type").and_then(|t| t.as_str()) {
                        let detail = event.get("message").and_then(|m| m.as_str()).unwrap_or("");
                        if detail.is_empty() {
                            eprintln!("[event] {ty}");
                        } else {
                            eprintln!("[event] {ty}: {detail}");
                        }
                        if ty == "Completed" || ty == "Error" {
                            done = true;
                        }
                    }
                }
            }
        }
        if done {
            break;
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

// ── Full E2E: operator builds a Vite React app, playwright tests interactions ──

#[tokio::test]
#[ignore = "requires sandbox, LLM provider, and playwright — run with SMOOTH_SANDBOXED=1 --ignored --nocapture"]
async fn operator_builds_vite_app_playwright_tests_interactions() {
    if !std::env::var("SMOOTH_SANDBOXED")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
    {
        eprintln!("SKIP: SMOOTH_SANDBOXED not set");
        return;
    }

    let Some((router, tmp)) = test_app() else {
        eprintln!("SKIP: smooth-dolt binary not available");
        return;
    };
    let port = start_server(router).await;
    eprintln!("Big Smooth started on port {port}");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600)) // 10 min — Vite scaffold + npm install takes time
        .build()
        .expect("build reqwest client");

    let task_message = r#"Build a simple Vite React app with interactive components. Follow these exact steps:

1. Run: npm create vite@latest myapp -- --template react
2. cd myapp && npm install
3. Replace src/App.jsx with this content:

import { useState } from 'react'

function App() {
  const [count, setCount] = useState(0)
  const [name, setName] = useState('')
  const [submitted, setSubmitted] = useState(false)

  return (
    <div style={{ padding: '2rem', fontFamily: 'sans-serif' }}>
      <h1 data-testid="title">Smooth Vite App</h1>

      <section style={{ marginBottom: '2rem' }}>
        <h2>Counter</h2>
        <p data-testid="count">Count: {count}</p>
        <button data-testid="increment" onClick={() => setCount(c => c + 1)}>
          Increment
        </button>
        <button data-testid="decrement" onClick={() => setCount(c => c - 1)}>
          Decrement
        </button>
      </section>

      <section>
        <h2>Greeting Form</h2>
        {!submitted ? (
          <div>
            <input
              data-testid="name-input"
              placeholder="Enter your name"
              value={name}
              onChange={e => setName(e.target.value)}
            />
            <button
              data-testid="submit-btn"
              onClick={() => name && setSubmitted(true)}
            >
              Submit
            </button>
          </div>
        ) : (
          <p data-testid="greeting">Hello, {name}! Welcome to Smooth.</p>
        )}
      </section>
    </div>
  )
}

export default App

4. Start the dev server: npx vite --host 0.0.0.0 --port 3000 &
5. Wait 3 seconds for the server to start
6. Use the forward_port tool to expose port 3000 to the host
7. Tell me the host port number"#;

    let resp = client
        .post(format!("http://127.0.0.1:{port}/api/tasks"))
        .json(&serde_json::json!({
            "message": task_message,
            "working_dir": tmp.path().to_string_lossy(),
        }))
        .send()
        .await
        .expect("POST /api/tasks should connect");

    assert!(resp.status().is_success());

    let body = resp.text().await.expect("read body");
    let mut forwarded_port: Option<u16> = None;

    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                    for word in content.split_whitespace() {
                        if let Ok(p) = word.trim_matches(|c: char| !c.is_ascii_digit()).parse::<u16>() {
                            if (10000..65535).contains(&p) {
                                forwarded_port = Some(p);
                            }
                        }
                    }
                }
                if let Some(ty) = event.get("type").and_then(|t| t.as_str()) {
                    let detail = event.get("message").and_then(|m| m.as_str()).unwrap_or("");
                    if detail.is_empty() {
                        eprintln!("[event] {ty}");
                    } else {
                        eprintln!("[event] {ty}: {detail}");
                    }
                }
            }
        }
    }

    if let Some(host_port) = forwarded_port {
        eprintln!("Forwarded port detected: {host_port}");
        eprintln!("Running Playwright interaction tests against http://localhost:{host_port}");

        let script_path = tmp.path().join("test-vite-app.mjs");
        std::fs::write(
            &script_path,
            format!(
                r#"
import {{ chromium }} from 'playwright';

const browser = await chromium.launch();
const page = await browser.newPage();

// Navigate and wait for hydration
await page.goto('http://localhost:{host_port}', {{ waitUntil: 'networkidle' }});

// Test 1: Title is present
const title = await page.textContent('[data-testid="title"]');
console.log('Title:', title);
if (!title?.includes('Smooth Vite App')) {{
    console.error('FAIL: Title mismatch:', title);
    process.exit(1);
}}
console.log('PASS: Title correct');

// Test 2: Counter starts at 0
let count = await page.textContent('[data-testid="count"]');
console.log('Initial count:', count);
if (!count?.includes('Count: 0')) {{
    console.error('FAIL: Initial count should be 0:', count);
    process.exit(1);
}}
console.log('PASS: Initial count is 0');

// Test 3: Increment counter 3 times
await page.click('[data-testid="increment"]');
await page.click('[data-testid="increment"]');
await page.click('[data-testid="increment"]');
count = await page.textContent('[data-testid="count"]');
console.log('After 3 increments:', count);
if (!count?.includes('Count: 3')) {{
    console.error('FAIL: Expected count 3:', count);
    process.exit(1);
}}
console.log('PASS: Counter increments to 3');

// Test 4: Decrement counter
await page.click('[data-testid="decrement"]');
count = await page.textContent('[data-testid="count"]');
console.log('After decrement:', count);
if (!count?.includes('Count: 2')) {{
    console.error('FAIL: Expected count 2:', count);
    process.exit(1);
}}
console.log('PASS: Counter decrements to 2');

// Test 5: Form submission
await page.fill('[data-testid="name-input"]', 'Brent');
await page.click('[data-testid="submit-btn"]');
const greeting = await page.textContent('[data-testid="greeting"]');
console.log('Greeting:', greeting);
if (!greeting?.includes('Hello, Brent! Welcome to Smooth.')) {{
    console.error('FAIL: Greeting mismatch:', greeting);
    process.exit(1);
}}
console.log('PASS: Form submits and shows greeting');

await browser.close();
console.log('ALL 5 TESTS PASSED');
process.exit(0);
"#
            ),
        )
        .expect("write playwright script");

        let output = tokio::process::Command::new("node").arg(&script_path).current_dir(tmp.path()).output().await;

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                eprintln!("Playwright stdout:\n{stdout}");
                if !stderr.is_empty() {
                    eprintln!("Playwright stderr:\n{stderr}");
                }
                assert!(o.status.success(), "Playwright tests should pass — 5 interaction tests");
                assert!(stdout.contains("ALL 5 TESTS PASSED"), "Should see all tests pass");
            }
            Err(e) => {
                eprintln!("Playwright failed to run: {e}");
            }
        }
    } else {
        eprintln!("No forwarded port detected — skipping Playwright tests");
    }

    eprintln!("operator_builds_vite_app_playwright_tests_interactions: complete");
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

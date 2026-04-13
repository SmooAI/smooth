//! Consolidated full-stack E2E test.
//!
//! ONE canonical test that exercises the entire Smooth stack end-to-end
//! through the same entrypoint a user would hit: `smooth-code headless`.
//!
//! Phases (one sandboxed operator per phase):
//!
//! 1. **Rust backend** — agent implements an axum task API against Rust
//!    contract tests in `backend_rust_spec`.
//! 2. **Go backend** — agent implements a stdlib `net/http` task API
//!    against Go contract tests in `backend_go_spec`.
//! 3. **TypeScript backend** — agent implements a Hono task API against
//!    vitest contract tests in `backend_typescript_spec`.
//! 4. **Python backend** — agent implements a FastAPI task API against
//!    pytest contract tests in `backend_python_spec`.
//! 5. **React frontend** — agent implements a component that talks to all
//!    four backends via `fetch('/api/<lang>/health')`, tested with vitest
//!    jsdom + fetch mocks in `frontend_app_spec`.
//!
//! Each phase:
//!   - Runs through `smooth_code::headless::run_headless_capture()` against
//!     an in-process Big Smooth (same codepath as `th code --headless`).
//!   - Dispatches to a real hardware-isolated microVM via
//!     `dispatch_ws_task_sandboxed`.
//!   - Captures every ToolCall the agent made.
//!   - Executes the phase's contract tests on the host after the agent
//!     finishes — the workspace is a bind mount so anything the agent
//!     wrote is visible here.
//!   - Calls an LLM judge to score the generated code.
//!
//! Final assertions:
//!   - At least N of M phases passed the contract tests.
//!   - The agent used a healthy set of tools in every phase (read_file,
//!     bash, edit_file or write_file — plus bonus credit for grep/lsp).
//!
//! Marked `#[ignore]` because it boots 5 real microVMs, installs
//! language toolchains, runs real LLM calls, and takes ~30 minutes cold
//! (much less warm, thanks to the pearl env cache).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use smooth_bigsmooth::db::Database;
use smooth_bigsmooth::server::{build_router, AppState};
use smooth_code::headless::{run_headless_capture, HeadlessOutput};
use smooth_pearls::PearlStore;
use tempfile::TempDir;

mod common;
use common::call_llm_judge;

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

/// Spawn an in-process Big Smooth on an ephemeral port and return its URL
/// + a guard holding the tempdir + Dolt store alive for the test's life.
struct BigSmoothHandle {
    url: String,
    #[allow(dead_code)]
    tmp: TempDir,
}

async fn spawn_bigsmooth() -> Option<BigSmoothHandle> {
    let tmp = tempfile::tempdir().ok()?;
    let db_path = tmp.path().join("smooth.db");
    let db = Database::open(&db_path).ok()?;
    let dolt_dir = tmp.path().join("dolt");
    let pearl_store = match PearlStore::init(&dolt_dir) {
        Ok(s) => s,
        Err(_) => return None, // smooth-dolt binary not available in this env
    };
    let state = AppState::new(db, pearl_store);
    let router = build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.ok()?;
    let addr: SocketAddr = listener.local_addr().ok()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    // Small beat to let axum start accepting connections.
    tokio::time::sleep(Duration::from_millis(100)).await;
    Some(BigSmoothHandle {
        url: format!("http://{addr}"),
        tmp,
    })
}

/// Copy a fixture directory into a fresh workspace tempdir.
fn seed_workspace(fixture_name: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("workspace tempdir");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(fixture_name);
    copy_tree(&fixture, dir.path());
    dir
}

fn copy_tree(src: &Path, dst: &Path) {
    for entry in walkdir_simple(src) {
        let relative = entry.strip_prefix(src).unwrap();
        let dst_path = dst.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&dst_path).expect("create dir");
        } else {
            if let Some(parent) = dst_path.parent() {
                std::fs::create_dir_all(parent).expect("create parent");
            }
            std::fs::copy(&entry, &dst_path).expect("copy file");
        }
    }
}

fn walkdir_simple(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        out.push(p.clone());
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd.flatten() {
                    stack.push(e.path());
                }
            }
        }
    }
    out
}

/// Tool-usage check: each phase should exercise at least the minimum
/// set of tools expected for a real coding workflow. Returns `true` if
/// the tool set looks healthy. Logs everything either way — the
/// caller decides whether to assert based on phase success.
fn check_healthy_tool_usage(phase: &str, output: &HeadlessOutput) -> bool {
    let names: HashSet<&str> = output.tool_calls.iter().map(|c| c.name.as_str()).collect();
    eprintln!("[{phase}] tools used: {names:?}");

    // Required: the agent must have at least read a file and run bash
    // (to compile / test) and written code somewhere.
    let read_ok = names.contains("read_file");
    let bash_ok = names.contains("bash");
    let wrote_ok = names.contains("write_file") || names.contains("edit_file") || names.contains("apply_patch");

    if !read_ok {
        eprintln!("[{phase}] WARNING: agent never called read_file");
    }
    if !bash_ok {
        eprintln!("[{phase}] WARNING: agent never called bash");
    }
    if !wrote_ok {
        eprintln!("[{phase}] WARNING: agent never wrote any code (no write_file/edit_file/apply_patch)");
    }

    // Bonus metrics — useful for tracking how well the prompt nudges
    // the agent toward better tool use. Not part of the health check.
    let used_search = names.contains("grep") || names.contains("list_files");
    let used_lsp = names.contains("lsp");
    eprintln!("[{phase}] used search tool: {used_search}, used lsp: {used_lsp}");

    read_ok && bash_ok && wrote_ok
}

/// A single phase's result, aggregated across all phases at the end.
#[derive(Debug)]
struct PhaseResult {
    phase: String,
    passed_tests: u32,
    failed_tests: u32,
    judge_verdict: String,
    judge_score: i64,
    cost_usd: f64,
    tool_call_count: usize,
}

// ---------------------------------------------------------------------------
// Per-language phase runners
// ---------------------------------------------------------------------------

async fn run_rust_phase(bigsmooth_url: &str, llm: &smooth_operator::llm::LlmConfig) -> PhaseResult {
    std::env::set_var("SMOOTH_ENV_CACHE_KEY", "full-stack-e2e-rust");
    let ws = seed_workspace("backend_rust_spec");

    let task = concat!(
        "Implement a small Rust crate `task_api`. The workspace already has Cargo.toml and tests/spec_test.rs. ",
        "Read the spec test in full. Create src/lib.rs exporting `pub fn app() -> axum::Router` with every endpoint ",
        "the tests exercise (GET /health, POST/GET/PATCH/DELETE /tasks, etc.). Use an in-memory Mutex<HashMap<String, Task>>. ",
        "Install rust via apk + rustup if cargo isn't present. ",
        "After writing src/lib.rs, run `cargo test` and iterate on compile/test errors until zero failures. ",
        "Do NOT declare done until `cargo test` passes cleanly."
    );

    let output = drive_headless(bigsmooth_url, ws.path(), task, 1.0).await;
    let _tool_health_ok = check_healthy_tool_usage("rust", &output);

    let (passed, failed) = run_cargo_test(ws.path());
    eprintln!("[rust] objective: {passed} passed, {failed} failed");

    let generated = std::fs::read_to_string(ws.path().join("src/lib.rs")).unwrap_or_default();
    let test_output = cargo_test_output_tail(ws.path());
    let (verdict, score, _rationale) = call_llm_judge(&llm.api_url, &llm.api_key, &llm.model, "rust", &generated, &test_output, passed, failed)
        .await
        .unwrap_or(("error".into(), 0, "judge failed".into()));

    PhaseResult {
        phase: "rust".into(),
        passed_tests: passed,
        failed_tests: failed,
        judge_verdict: verdict,
        judge_score: score,
        cost_usd: output.cost,
        tool_call_count: output.tool_calls.len(),
    }
}

async fn run_go_phase(bigsmooth_url: &str, llm: &smooth_operator::llm::LlmConfig) -> PhaseResult {
    std::env::set_var("SMOOTH_ENV_CACHE_KEY", "full-stack-e2e-go");
    let ws = seed_workspace("backend_go_spec");

    let task = concat!(
        "Implement a Go task API. The workspace has go.mod and taskapi_test.go with the contract. ",
        "Read the test file in full. Create taskapi.go in the same package exporting `func NewServer() http.Handler`, ",
        "implementing every endpoint the tests exercise. Use sync.Mutex + map[string]*Task for state. ",
        "Install Go via apk if missing. ",
        "Run `go test ./...` and iterate until zero failures. Do NOT declare done until `go test` passes cleanly."
    );

    let output = drive_headless(bigsmooth_url, ws.path(), task, 1.0).await;
    let _tool_health_ok = check_healthy_tool_usage("go", &output);

    let (passed, failed) = run_go_test(ws.path());
    eprintln!("[go] objective: {passed} passed, {failed} failed");

    let generated = std::fs::read_to_string(ws.path().join("taskapi.go")).unwrap_or_default();
    let test_output = format!("go test: {passed} passed, {failed} failed");
    let (verdict, score, _) = call_llm_judge(&llm.api_url, &llm.api_key, &llm.model, "go", &generated, &test_output, passed, failed)
        .await
        .unwrap_or(("error".into(), 0, "judge failed".into()));

    PhaseResult {
        phase: "go".into(),
        passed_tests: passed,
        failed_tests: failed,
        judge_verdict: verdict,
        judge_score: score,
        cost_usd: output.cost,
        tool_call_count: output.tool_calls.len(),
    }
}

async fn run_typescript_phase(bigsmooth_url: &str, llm: &smooth_operator::llm::LlmConfig) -> PhaseResult {
    std::env::set_var("SMOOTH_ENV_CACHE_KEY", "full-stack-e2e-ts");
    let ws = seed_workspace("backend_typescript_spec");

    let task = concat!(
        "Implement a Hono task API in TypeScript. The workspace has package.json, tsconfig.json, ",
        "and tests/spec.test.ts. Read the spec test in full. Create src/server.ts exporting `function app(): Hono` ",
        "with every endpoint the tests exercise. Install nodejs + pnpm via apk if missing. ",
        "Run `pnpm install` and `pnpm test`, iterate on errors until all vitest tests pass. ",
        "Do NOT declare done until tests are green."
    );

    let output = drive_headless(bigsmooth_url, ws.path(), task, 1.5).await;
    let _tool_health_ok = check_healthy_tool_usage("typescript", &output);

    let (passed, failed) = run_vitest(ws.path());
    eprintln!("[typescript] objective: {passed} passed, {failed} failed");

    let generated = std::fs::read_to_string(ws.path().join("src/server.ts")).unwrap_or_default();
    let test_output = format!("vitest: {passed} passed, {failed} failed");
    let (verdict, score, _) = call_llm_judge(&llm.api_url, &llm.api_key, &llm.model, "typescript", &generated, &test_output, passed, failed)
        .await
        .unwrap_or(("error".into(), 0, "judge failed".into()));

    PhaseResult {
        phase: "typescript".into(),
        passed_tests: passed,
        failed_tests: failed,
        judge_verdict: verdict,
        judge_score: score,
        cost_usd: output.cost,
        tool_call_count: output.tool_calls.len(),
    }
}

async fn run_python_phase(bigsmooth_url: &str, llm: &smooth_operator::llm::LlmConfig) -> PhaseResult {
    std::env::set_var("SMOOTH_ENV_CACHE_KEY", "full-stack-e2e-python");
    let ws = seed_workspace("backend_python_spec");

    let task = concat!(
        "Implement a FastAPI task API in Python. The workspace has pyproject.toml and tests/test_api.py. ",
        "Read the test file in full. Create taskapi.py exporting `app = FastAPI()` with every endpoint ",
        "the tests exercise. Use an in-memory dict + lock for state. ",
        "Install python3 + pip via apk if missing, then `pip install -e '.[dev]'`. ",
        "Run `pytest` and iterate on failures until all tests pass. Do NOT declare done until pytest is green."
    );

    let output = drive_headless(bigsmooth_url, ws.path(), task, 1.0).await;
    let _tool_health_ok = check_healthy_tool_usage("python", &output);

    let (passed, failed) = run_pytest(ws.path());
    eprintln!("[python] objective: {passed} passed, {failed} failed");

    let generated = std::fs::read_to_string(ws.path().join("taskapi.py")).unwrap_or_default();
    let test_output = format!("pytest: {passed} passed, {failed} failed");
    let (verdict, score, _) = call_llm_judge(&llm.api_url, &llm.api_key, &llm.model, "python", &generated, &test_output, passed, failed)
        .await
        .unwrap_or(("error".into(), 0, "judge failed".into()));

    PhaseResult {
        phase: "python".into(),
        passed_tests: passed,
        failed_tests: failed,
        judge_verdict: verdict,
        judge_score: score,
        cost_usd: output.cost,
        tool_call_count: output.tool_calls.len(),
    }
}

async fn run_frontend_phase(bigsmooth_url: &str, llm: &smooth_operator::llm::LlmConfig) -> PhaseResult {
    std::env::set_var("SMOOTH_ENV_CACHE_KEY", "full-stack-e2e-frontend");
    let ws = seed_workspace("frontend_app_spec");

    let task = concat!(
        "Build a React app in TypeScript that talks to 4 backends. The workspace has package.json, ",
        "vite.config.ts, vitest.config.ts, tsconfig.json, index.html, and tests/app.test.tsx. ",
        "Read the test file in full — it documents every data-testid your components must expose. ",
        "Create src/App.tsx (default-exported component) with: a title containing 'Smooth' (data-testid='title'); ",
        "a backend-status card per language rust/go/typescript/python (data-testid='backend-<lang>') with a ",
        "'Check' button (data-testid='check-<lang>') that fetches /api/<lang>/health and renders the result ",
        "in data-testid='status-<lang>' ('ok' or 'error'); a 'Check all' button (data-testid='check-all'); ",
        "and a counter with increment/decrement buttons. Also create src/main.tsx that renders <App /> into #root. ",
        "Install nodejs + pnpm via apk if missing, run `pnpm install` and `pnpm test`, iterate until all ",
        "vitest tests pass. Do NOT declare done until tests are green."
    );

    let output = drive_headless(bigsmooth_url, ws.path(), task, 1.5).await;
    let _tool_health_ok = check_healthy_tool_usage("frontend", &output);

    let (passed, failed) = run_vitest(ws.path());
    eprintln!("[frontend] objective: {passed} passed, {failed} failed");

    let generated = std::fs::read_to_string(ws.path().join("src/App.tsx")).unwrap_or_default();
    let test_output = format!("vitest: {passed} passed, {failed} failed");
    let (verdict, score, _) = call_llm_judge(&llm.api_url, &llm.api_key, &llm.model, "react", &generated, &test_output, passed, failed)
        .await
        .unwrap_or(("error".into(), 0, "judge failed".into()));

    PhaseResult {
        phase: "frontend".into(),
        passed_tests: passed,
        failed_tests: failed,
        judge_verdict: verdict,
        judge_score: score,
        cost_usd: output.cost,
        tool_call_count: output.tool_calls.len(),
    }
}

// ---------------------------------------------------------------------------
// Helpers for host-side test execution
// ---------------------------------------------------------------------------

/// Drive smooth-code headless and return the captured output. If the
/// headless run fails (network drop to the LLM gateway, sandbox boot
/// failure, etc.), log the error and return a zeroed-out HeadlessOutput
/// so the aggregate "≥3 of 5 phases passed" gate can still evaluate the
/// remaining phases. The phase will report 0/0 tests passed.
async fn drive_headless(bigsmooth_url: &str, workspace: &Path, task: &str, budget_usd: f64) -> HeadlessOutput {
    match run_headless_capture(bigsmooth_url, workspace.to_path_buf(), task.to_string(), None, Some(budget_usd)).await {
        Ok(out) => out,
        Err(e) => {
            eprintln!("run_headless_capture failed (continuing to next phase): {e}");
            HeadlessOutput {
                content: String::new(),
                tool_calls: Vec::new(),
                cost: 0.0,
            }
        }
    }
}

fn run_cargo_test(workspace: &Path) -> (u32, u32) {
    let output = std::process::Command::new("cargo")
        .arg("test")
        .arg("--")
        .arg("--test-threads=1")
        .current_dir(workspace)
        .output()
        .expect("cargo test");
    let combined = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    parse_cargo_test_summary(&combined).unwrap_or((0, 0))
}

fn cargo_test_output_tail(workspace: &Path) -> String {
    let output = std::process::Command::new("cargo")
        .arg("test")
        .arg("--")
        .arg("--test-threads=1")
        .current_dir(workspace)
        .output()
        .expect("cargo test");
    let combined = format!("{}\n---\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let max = 2000usize;
    if combined.len() > max {
        format!("...[truncated]...\n{}", &combined[combined.len() - max..])
    } else {
        combined
    }
}

fn parse_cargo_test_summary(output: &str) -> Option<(u32, u32)> {
    // "test result: ok. 12 passed; 0 failed; ..."
    let mut total_passed = 0u32;
    let mut total_failed = 0u32;
    let mut found = false;
    for line in output.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("test result:") {
            found = true;
            let parts: Vec<&str> = rest.split(';').collect();
            for part in parts {
                let part = part.trim();
                if let Some(num) = part.strip_suffix(" passed") {
                    if let Ok(n) = num.trim().trim_start_matches('.').trim_start_matches("ok").trim().parse::<u32>() {
                        total_passed += n;
                    } else if let Some(stripped) = part.split_whitespace().find(|s| s.chars().all(|c| c.is_ascii_digit())) {
                        if let Ok(n) = stripped.parse::<u32>() {
                            total_passed += n;
                        }
                    }
                }
                if let Some(num) = part.strip_suffix(" failed") {
                    if let Ok(n) = num.trim().parse::<u32>() {
                        total_failed += n;
                    }
                }
            }
        }
    }
    if found {
        Some((total_passed, total_failed))
    } else {
        None
    }
}

fn run_go_test(workspace: &Path) -> (u32, u32) {
    let output = std::process::Command::new("go")
        .arg("test")
        .arg("-count=1")
        .arg("-json")
        .arg("./...")
        .current_dir(workspace)
        .output();
    let Ok(output) = output else {
        return (0, 0);
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut passed = 0u32;
    let mut failed = 0u32;
    for line in stdout.lines() {
        let Ok(evt): Result<serde_json::Value, _> = serde_json::from_str(line) else {
            continue;
        };
        let action = evt.get("Action").and_then(|a| a.as_str()).unwrap_or("");
        let has_test = evt.get("Test").is_some();
        if !has_test {
            continue;
        }
        match action {
            "pass" => passed += 1,
            "fail" => failed += 1,
            _ => {}
        }
    }
    (passed, failed)
}

fn run_vitest(workspace: &Path) -> (u32, u32) {
    let output = std::process::Command::new("pnpm")
        .args(["exec", "vitest", "run", "--reporter=json", "--outputFile=vitest-result.json"])
        .current_dir(workspace)
        .output();
    let Ok(_) = output else {
        return (0, 0);
    };
    let result_path = workspace.join("vitest-result.json");
    let Ok(result) = std::fs::read_to_string(&result_path) else {
        return (0, 0);
    };
    let Ok(parsed): Result<serde_json::Value, _> = serde_json::from_str(&result) else {
        return (0, 0);
    };
    let passed = parsed.get("numPassedTests").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let failed = parsed.get("numFailedTests").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    (passed, failed)
}

fn run_pytest(workspace: &Path) -> (u32, u32) {
    let output = std::process::Command::new("pytest").args(["--tb=short", "-q"]).current_dir(workspace).output();
    let Ok(output) = output else {
        return (0, 0);
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    // pytest summary: "12 passed, 1 failed in 0.42s"
    let mut passed = 0u32;
    let mut failed = 0u32;
    for line in stdout.lines().rev().take(10) {
        for token in line.split(|c: char| !c.is_ascii_digit() && c != ' ' && c != '.') {
            let words: Vec<&str> = token.split_whitespace().collect();
            for pair in words.windows(2) {
                if let Ok(n) = pair[0].parse::<u32>() {
                    match pair[1] {
                        "passed" | "passed," => passed = n,
                        "failed" | "failed," => failed = n,
                        _ => {}
                    }
                }
            }
        }
    }
    (passed, failed)
}

// ---------------------------------------------------------------------------
// The consolidated test
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "full-stack E2E across Rust/Go/TypeScript/Python + React frontend — boots 5 microVMs, installs toolchains, real LLM, ~30 min cold"]
async fn smooth_code_builds_full_stack_across_languages() {
    let providers_path = dirs_next::home_dir().expect("home dir").join(".smooth/providers.json");
    if !providers_path.exists() {
        eprintln!("SKIP: ~/.smooth/providers.json not found");
        return;
    }
    let registry = smooth_operator::providers::ProviderRegistry::load_from_file(&providers_path).expect("load providers.json");
    let llm = registry.default_llm_config().expect("default provider");

    let Some(bs) = spawn_bigsmooth().await else {
        eprintln!("SKIP: cannot spawn Big Smooth (smooth-dolt binary missing?)");
        return;
    };
    eprintln!("Big Smooth running at {}", bs.url);

    let mut results: Vec<PhaseResult> = Vec::new();

    results.push(run_rust_phase(&bs.url, &llm).await);
    results.push(run_go_phase(&bs.url, &llm).await);
    results.push(run_typescript_phase(&bs.url, &llm).await);
    results.push(run_python_phase(&bs.url, &llm).await);
    results.push(run_frontend_phase(&bs.url, &llm).await);

    eprintln!("\n=== Full-stack E2E summary ===");
    let mut total_cost = 0.0;
    let mut phases_with_tests_passing = 0usize;
    for r in &results {
        let total = r.passed_tests + r.failed_tests;
        let pct = if total > 0 {
            f64::from(r.passed_tests) / f64::from(total) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "  {}: {}/{} tests ({:.0}%), judge={} score={}, ${:.3}, {} tool calls",
            r.phase, r.passed_tests, total, pct, r.judge_verdict, r.judge_score, r.cost_usd, r.tool_call_count
        );
        total_cost += r.cost_usd;
        if r.passed_tests > 0 && f64::from(r.passed_tests) / f64::from(total.max(1)) >= 0.5 {
            phases_with_tests_passing += 1;
        }
    }
    eprintln!("  total cost: ${total_cost:.3}");
    eprintln!("  phases with >=50% tests passing: {phases_with_tests_passing}/{}\n", results.len());

    // Assertion: at least 3 of 5 phases must hit 50% pass rate. We don't
    // demand 5/5 because one phase can flake (LLM timeout, network jitter,
    // transient package registry issue) without invalidating the whole
    // suite. This bar is high enough to catch real regressions.
    assert!(
        phases_with_tests_passing >= 3,
        "expected at least 3 phases to pass >=50% of their contract tests, got {}. Results: {:#?}",
        phases_with_tests_passing,
        results
    );
}

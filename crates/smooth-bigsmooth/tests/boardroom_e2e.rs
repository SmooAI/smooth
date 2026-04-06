//! Boardroom + Bootstrap Bill + cross-language E2E.
//!
//! This is the payoff test: it runs Smooth as its intended architecture
//! end-to-end, without any shortcuts, and validates that real LLM-driven
//! agents can produce passing code in **both Rust and TypeScript** through
//! the same orchestration pipeline.
//!
//! # Topology exercised
//!
//! ```text
//! test process
//!   ├── spawns Bootstrap Bill (host subprocess, binds 0.0.0.0:<bill_port>)
//!   └── Bill spawns:
//!         ├── Boardroom VM (alpine + `boardroom` binary)
//!         │     ├── Big Smooth (axum :4400)
//!         │     ├── Archivist (:4401, reachable on host via port map)
//!         │     └── Boardroom cast: Wonk, Goalie, Narc, Scribe (all tokio tasks)
//!         │
//!         ├── Operator VM #1 (Rust)
//!         │     └── smooth-operator-runner + per-VM cast
//!         │         → LLM reads task_api_spec/tests/spec_test.rs
//!         │         → writes src/lib.rs into bind-mounted host workspace
//!         │         → Scribe forwards logs to Boardroom Archivist
//!         │
//!         └── Operator VM #2 (TypeScript)
//!               └── smooth-operator-runner + per-VM cast
//!                   → LLM reads hono_api_spec/tests/spec.test.ts
//!                   → writes src/server.ts into bind-mounted host workspace
//!                   → Scribe forwards logs to Boardroom Archivist
//! ```
//!
//! # What gets asserted
//!
//! 1. Boardroom VM boots and Big Smooth's `/health` responds.
//! 2. Rust agent writes `src/lib.rs`. Host runs `cargo test`. ≥50% pass.
//! 3. LLM judge evaluates the Rust code. `pass` verdict or score ≥ 5.
//! 4. TypeScript agent writes `src/server.ts`. Host runs
//!    `pnpm install --frozen-lockfile && pnpm exec vitest`. ≥50% pass.
//! 5. LLM judge evaluates the TS code. `pass` verdict or score ≥ 5.
//! 6. **Archivist query shows entries from BOTH operator VMs** (distinct
//!    `source_vm` values). This is the proof that cross-VM log forwarding
//!    works end-to-end.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use smooth_bootstrap_bill::protocol::{BindMountSpec, PortMapping, SandboxSpec};
use smooth_bootstrap_bill::BillClient;
use tokio_tungstenite::tungstenite::Message;

use common::{call_llm_judge, copy_tree, find_workspace_target, parse_cargo_test_summary, parse_vitest_summary, spawn_bill_subprocess, wait_for_http_ok};

/// Explicit teardown handle. The test calls [`cleanup`] at the end of
/// the happy path. Panics unwind to test harness which kills the child
/// process group — Bill's own panic hook cleans up its sandboxes in
/// that case.
struct TeardownGuard {
    bill_child: Option<Child>,
    bill_client: Option<BillClient>,
    boardroom_name: Option<String>,
}

impl TeardownGuard {
    async fn cleanup(mut self) {
        if let (Some(client), Some(name)) = (self.bill_client.take(), self.boardroom_name.take()) {
            let _ = client.destroy(&name).await;
        }
        if let Some(mut child) = self.bill_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[tokio::test]
#[ignore = "full Boardroom + Bill + two operator VMs + real LLM + cargo test + pnpm + LLM judge — requires hardware virt, providers.json, pnpm, node>=20"]
async fn boardroom_full_stack_rust_and_typescript_with_judge() {
    // --- Prereq gate -------------------------------------------------------
    let providers_path = dirs_next::home_dir().expect("home dir").join(".smooth/providers.json");
    if !providers_path.exists() {
        eprintln!("SKIP: ~/.smooth/providers.json missing — need real LLM credentials");
        return;
    }
    let registry = smooth_operator::providers::ProviderRegistry::load_from_file(&providers_path).expect("load providers.json");
    let llm = registry.default_llm_config().expect("default llm");

    let operator_runner_host_path = find_workspace_target("aarch64-unknown-linux-musl/release/smooth-operator-runner")
        .expect("operator runner binary missing — run scripts/build-operator-runner.sh")
        .canonicalize()
        .expect("canon");
    let _operator_runner_host_dir = operator_runner_host_path
        .parent()
        .expect("runner has parent")
        .to_path_buf();
    let boardroom_bin_path = find_workspace_target("aarch64-unknown-linux-musl/release/boardroom")
        .expect("boardroom binary missing — run scripts/build-boardroom.sh")
        .canonicalize()
        .expect("canon");
    let boardroom_bin_dir = boardroom_bin_path.parent().expect("has parent").to_path_buf();

    assert!(which_exists("pnpm"), "pnpm is required on PATH for the TypeScript leg");
    assert!(which_exists("node"), "node is required on PATH for the TypeScript leg");

    // --- Start Bill --------------------------------------------------------
    let (bill_child, bill_addr) = spawn_bill_subprocess().await.expect("spawn bill");
    let bill_host_port = bill_addr.port();
    eprintln!("bill: {bill_addr}");
    let bill_url = format!("http://127.0.0.1:{bill_host_port}");
    let bill_client = BillClient::new(bill_url);
    let version = bill_client.ping().await.expect("bill ping");
    eprintln!("bill version: {version}");

    // --- Spawn Boardroom VM ------------------------------------------------
    // Probe a free host port for Archivist BEFORE calling Bill so we can
    // put it in the Boardroom's env (which the Boardroom then uses to
    // construct the operator-facing archivist URL). This is the chicken-
    // and-egg solution: we know the port before the VM boots, Bill
    // honors it via the fixed host_port in the PortMapping.
    let archivist_host_port: u16 = {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("probe archivist port");
        let p = l.local_addr().expect("addr").port();
        drop(l);
        p
    };
    eprintln!("pre-reserved archivist host port: {archivist_host_port}");

    let home_dot_smooth = dirs_next::home_dir().expect("home").join(".smooth");
    let boardroom_name = format!("smooth-boardroom-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let mut boardroom_env: HashMap<String, String> = HashMap::new();
    boardroom_env.insert("SMOOTH_BOARDROOM_MODE".into(), "1".into());
    // Critical: tells Big Smooth to take dispatch_ws_task_sandboxed, which
    // routes operator spawns through our sandbox client (which, with
    // SMOOTH_BOOTSTRAP_BILL_URL set, is BillSandboxClient). Without this,
    // Big Smooth dispatches the agent in-process inside the Boardroom VM
    // and the work never lands on the host.
    boardroom_env.insert("SMOOTH_SANDBOXED".into(), "1".into());
    // Reaching the host from inside a microVM:
    //
    // microsandbox's TCP proxy intercepts all non-loopback outbound TCP
    // and does `TcpStream::connect(dst)` on the HOST with the guest's
    // original destination address. 127.0.0.1 NEVER works because the
    // guest kernel handles loopback internally — the packet never
    // reaches microsandbox's virtual NIC.
    //
    // The solution: use the HOST's real network interface IP. Bill is on
    // 0.0.0.0:<port>, so any routable IP that reaches the host works.
    // We detect the primary interface IP at test time and pass it in.
    // The `allow_host_loopback: true` flag on the SandboxSpec tells
    // Bill to apply `NetworkPolicy::allow_all()`, which permits
    // private-network (192.168.x, etc.) outbound through the proxy.
    let host_ip = detect_host_ip();
    eprintln!("host IP for VM→host connectivity: {host_ip}");
    boardroom_env.insert(
        "SMOOTH_BOOTSTRAP_BILL_URL".into(),
        format!("http://{host_ip}:{bill_host_port}"),
    );
    boardroom_env.insert(
        "SMOOTH_OPERATOR_RUNNER_HOST_PATH".into(),
        operator_runner_host_path.to_string_lossy().to_string(),
    );
    boardroom_env.insert("SMOOTH_ARCHIVIST_HOST_PORT".into(), archivist_host_port.to_string());
    boardroom_env.insert("SMOOTH_BOARDROOM_DB".into(), "/root/.smooth/smooth.db".into());
    boardroom_env.insert("SMOOTH_BOARDROOM_PORT".into(), "4400".into());
    // Alpine's base image has no HOME set by default, which breaks
    // dirs_next::home_dir() inside the VM and therefore breaks
    // load_llm_config_for_runner. Set HOME explicitly to /root so Big
    // Smooth finds the bind-mounted ~/.smooth/providers.json.
    boardroom_env.insert("HOME".into(), "/root".into());
    boardroom_env.insert("RUST_LOG".into(), "info,smooth_bigsmooth=debug".into());
    // Stable cache key so Rust deps compiled on the first test run
    // persist to ~/.smooth/pearl-env/boardroom-e2e-test/ and are reused
    // on subsequent runs. First run is cold (~5 min for cargo build);
    // every run after is warm (~5s).
    boardroom_env.insert("SMOOTH_ENV_CACHE_KEY".into(), "boardroom-e2e-test".into());

    let spec = SandboxSpec {
        name: boardroom_name.clone(),
        image: "alpine".into(),
        cpus: 2,
        memory_mb: 2048,
        env: boardroom_env,
        mounts: vec![
            BindMountSpec {
                host_path: boardroom_bin_dir.to_string_lossy().to_string(),
                guest_path: "/opt/smooth/bin".into(),
                readonly: true,
            },
            BindMountSpec {
                host_path: home_dot_smooth.to_string_lossy().to_string(),
                guest_path: "/root/.smooth".into(),
                readonly: false,
            },
        ],
        ports: vec![
            PortMapping { host_port: 0, guest_port: 4400, bind_all: false },
            // Archivist must be reachable from OTHER VMs via the host IP.
            // microsandbox publishes on 127.0.0.1 only; `bind_all: true`
            // tells Bill to run a TCP proxy on 0.0.0.0 as well.
            PortMapping { host_port: archivist_host_port, guest_port: 4401, bind_all: true },
        ],
        timeout_seconds: 1800,
        // The Boardroom VM must reach Bill on host loopback
        // (127.0.0.1:<bill_port>) so Big Smooth can request operator pods.
        // Default microsandbox policy denies loopback/private outbound.
        allow_host_loopback: true,
        // Boardroom itself doesn't need env caching (it's Big Smooth, not
        // an operator). Operator VMs get caching via the dispatch path.
        env_cache_key: None,
    };

    let (resolved_name, host_ports, _created_at) = bill_client.spawn(spec).await.expect("spawn boardroom");
    eprintln!("boardroom: {resolved_name} host_ports={host_ports:?}");
    let teardown = TeardownGuard {
        bill_child: Some(bill_child),
        bill_client: Some(bill_client.clone()),
        boardroom_name: Some(resolved_name.clone()),
    };

    let bigsmooth_host_port = host_ports.iter().find(|p| p.guest_port == 4400).map(|p| p.host_port).expect("4400 mapping");
    // archivist_host_port was pre-reserved above; confirm Bill honored it.
    let archivist_resolved = host_ports.iter().find(|p| p.guest_port == 4401).map(|p| p.host_port).expect("4401 mapping");
    assert_eq!(archivist_resolved, archivist_host_port, "bill should have honored our fixed archivist port");
    eprintln!("boardroom API : http://127.0.0.1:{bigsmooth_host_port}");
    eprintln!("boardroom ARCH: http://127.0.0.1:{archivist_host_port}");

    // Exec the boardroom binary. This is a long-running server; we spawn
    // the exec in a background task and let it run until we destroy the
    // sandbox at teardown. The exec future will return with an error
    // when the sandbox is destroyed, which is fine.
    {
        let exec_client = bill_client.clone();
        let exec_name = resolved_name.clone();
        tokio::spawn(async move {
            match exec_client.exec(&exec_name, &["/opt/smooth/bin/boardroom".to_string()]).await {
                Ok((stdout, stderr, code)) => {
                    eprintln!("boardroom exec finished: code={code}\nstdout: {stdout}\nstderr tail: {}", &stderr[stderr.len().saturating_sub(2000)..]);
                }
                Err(e) => {
                    eprintln!("boardroom exec error: {e}");
                }
            }
        });
    }

    // Wait for Big Smooth to come up.
    wait_for_http_ok(&format!("http://127.0.0.1:{bigsmooth_host_port}/health"), Duration::from_secs(120))
        .await
        .expect("boardroom /health never came up");
    eprintln!("boardroom Big Smooth is healthy");

    // Wait for Archivist too.
    wait_for_http_ok(&format!("http://127.0.0.1:{archivist_host_port}/health"), Duration::from_secs(30))
        .await
        .expect("boardroom Archivist /health never came up");
    eprintln!("boardroom Archivist is healthy");

    // --- Open WS to Big Smooth --------------------------------------------
    let ws_url = format!("ws://127.0.0.1:{bigsmooth_host_port}/ws");
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.expect("connect ws");
    // Drain Connected event.
    let _ = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;

    // --- Rust leg ----------------------------------------------------------
    eprintln!("\n===== RUST LEG =====");
    let rust_workspace = tempfile::tempdir().expect("rust workspace tempdir");
    let rust_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/task_api_spec");
    copy_tree(&rust_fixture, rust_workspace.path());
    let (rust_passed, rust_failed, rust_code) = run_rust_leg(&mut ws, rust_workspace.path(), &llm).await;

    // --- TypeScript leg ----------------------------------------------------
    eprintln!("\n===== TYPESCRIPT LEG =====");
    let ts_workspace = tempfile::tempdir().expect("ts workspace tempdir");
    let ts_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hono_api_spec");
    copy_tree(&ts_fixture, ts_workspace.path());
    let (ts_passed, ts_failed, ts_code) = run_ts_leg(&mut ws, ts_workspace.path(), &llm).await;

    // --- Archivist diagnostic: check if operators got the URL ---------------
    for (name, ws) in [("rust", rust_workspace.path()), ("ts", ts_workspace.path())] {
        let diag = ws.join(".archivist-diag.txt");
        if diag.exists() {
            let content = std::fs::read_to_string(&diag).unwrap_or_default();
            eprintln!("archivist diag ({name}): {content}");
        } else {
            eprintln!("archivist diag ({name}): .archivist-diag.txt NOT FOUND (runner didn't write it?)");
        }
    }

    // --- Archivist cross-VM log assertion ----------------------------------
    eprintln!("\n===== ARCHIVIST CROSS-VM LOG ASSERTION =====");
    // Give the forwarder a beat to flush any trailing batches.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let archivist_query_url = format!("http://127.0.0.1:{archivist_host_port}/query?limit=500");
    let resp = reqwest::get(&archivist_query_url).await.expect("archivist query");
    let entries: serde_json::Value = resp.json().await.expect("archivist json");
    let entries_arr = entries.as_array().cloned().unwrap_or_default();
    eprintln!("archivist: {} total entries", entries_arr.len());
    // The /query endpoint strips source_vm (returns bare LogEntry); use
    // /stats to get per-VM aggregation.
    let stats_url = format!("http://127.0.0.1:{archivist_host_port}/stats");
    let stats_resp = reqwest::get(&stats_url).await.expect("archivist stats");
    let stats: serde_json::Value = stats_resp.json().await.expect("archivist stats json");
    eprintln!("archivist stats: {}", serde_json::to_string_pretty(&stats).unwrap_or_default());
    let by_vm = stats.get("by_vm").and_then(|v| v.as_object()).cloned().unwrap_or_default();

    // --- Final assertions --------------------------------------------------
    eprintln!("\n===== SUMMARY =====");
    let rust_total = rust_passed + rust_failed;
    let ts_total = ts_passed + ts_failed;
    eprintln!("rust leg: {rust_passed}/{rust_total} pass, judge={:?}", rust_code);
    eprintln!("ts leg:   {ts_passed}/{ts_total} pass, judge={:?}", ts_code);
    eprintln!("archivist by_vm: {:?}", by_vm.keys().collect::<Vec<_>>());

    assert!(rust_total > 0, "rust leg produced zero tests — fixture broken or agent never wrote src/lib.rs");
    assert!(ts_total > 0, "ts leg produced zero tests — fixture broken or agent never wrote src/server.ts");
    let rust_rate = f64::from(rust_passed) / f64::from(rust_total);
    let ts_rate = f64::from(ts_passed) / f64::from(ts_total);
    assert!(rust_rate >= 0.5, "rust leg {rust_passed}/{rust_total} ({:.0}%) below 50% floor", rust_rate * 100.0);
    assert!(ts_rate >= 0.5, "ts leg {ts_passed}/{ts_total} ({:.0}%) below 50% floor", ts_rate * 100.0);

    let (rust_verdict, rust_score, _) = rust_code;
    let (ts_verdict, ts_score, _) = ts_code;
    assert!(rust_verdict == "pass" || rust_score >= 5, "rust judge rejected: verdict={rust_verdict} score={rust_score}");
    assert!(ts_verdict == "pass" || ts_score >= 5, "ts judge rejected: verdict={ts_verdict} score={ts_score}");

    assert!(
        by_vm.len() >= 2,
        "archivist should have seen logs from ≥2 distinct source_vms (proving cross-VM forwarding), got {}: {by_vm:?}",
        by_vm.len()
    );

    eprintln!("\n✓ boardroom_full_stack_rust_and_typescript_with_judge: GREEN");

    // Explicit async teardown — Drop cannot create a new runtime from
    // inside the test's runtime.
    teardown.cleanup().await;
}

/// Run the Rust leg. Returns `(passed, failed, judge verdict)`.
async fn run_rust_leg(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    workspace: &std::path::Path,
    llm: &smooth_operator::llm::LlmConfig,
) -> (u32, u32, (String, i64, String)) {
    // Write full instructions to a file in the workspace (bind-mounted
    // from host). The env var SMOOTH_TASK gets a short pointer; the
    // runner will also check /workspace/.smooth-task for details.
    // This avoids the kernel cmdline TooLarge limit.
    let task_detail = concat!(
        "Read tests/spec_test.rs carefully. Create src/lib.rs: pub fn app() -> axum::Router. ",
        "Endpoints: GET /health (json {status: ok, version: string}), POST /tasks (201, title required else 422, ",
        "auto id via uuid, created_at via chrono::Utc::now, default priority medium, status open, tags default empty vec), ",
        "GET /tasks (list as JSON array, optional ?status= and ?priority= query filters), GET /tasks/:id (404 if missing), ",
        "PATCH /tasks/:id (partial update, 200 with updated task), DELETE /tasks/:id (204 no content). ",
        "CRITICAL: use Arc<Mutex<HashMap<String, Task>>> as axum State. All handler return types must be concrete: ",
        "use (StatusCode, Json<T>) or Json<T> consistently. Do NOT mix impl IntoResponse with different concrete types ",
        "in the same function -- that causes type inference failures. For error returns use StatusCode alone. ",
        "Only create src/lib.rs. ",
        "THEN: run 'apk add cargo rust' and 'cargo check' in the workspace. Fix ALL compile errors. ",
        "THEN: run 'cargo test -- --test-threads=1'. Fix any test failures. ",
        "Repeat until all tests pass. Do not finish until cargo test succeeds. Quality checks are mandatory."
    );
    std::fs::write(workspace.join(".smooth-task"), task_detail).expect("write task detail");
    let task_message = "Rust task_api crate. Read /workspace/.smooth-task for full instructions.";
    send_task_and_wait(ws, task_message, workspace).await;

    let lib_rs = workspace.join("src/lib.rs");
    assert!(lib_rs.exists(), "rust leg: src/lib.rs was not written");
    let generated = std::fs::read_to_string(&lib_rs).expect("read lib.rs");
    eprintln!("rust leg: src/lib.rs ({} bytes)", generated.len());

    // cargo test against the persisted workspace.
    let output = tokio::task::spawn_blocking({
        let ws = workspace.to_path_buf();
        move || {
            std::process::Command::new("cargo")
                .arg("test")
                .arg("--")
                .arg("--test-threads=1")
                .current_dir(&ws)
                .output()
                .expect("cargo test")
        }
    })
    .await
    .expect("join cargo test");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");
    let (passed, failed) = parse_cargo_test_summary(&combined).unwrap_or((0, 0));
    eprintln!("rust cargo test: {passed} passed, {failed} failed");

    let trimmed = tail_lines(&combined, 4000);
    let judge = call_llm_judge(&llm.api_url, &llm.api_key, &llm.model, "rust", &generated, &trimmed, passed, failed).await.expect("judge rust");
    (passed, failed, judge)
}

/// Run the TypeScript leg. Returns `(passed, failed, judge verdict)`.
async fn run_ts_leg(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    workspace: &std::path::Path,
    llm: &smooth_operator::llm::LlmConfig,
) -> (u32, u32, (String, i64, String)) {
    let task_detail = concat!(
        "Read tests/spec.test.ts. Create src/server.ts: export default Hono app. ",
        "Endpoints: GET /health (json status+version), POST /tasks (201, title required else 422, ",
        "auto id via crypto.randomUUID, created_at ISO, default priority medium, status open, tags empty), ",
        "GET /tasks (optional status/priority filters), GET /tasks/:id (404 if missing), ",
        "PATCH /tasks/:id, DELETE /tasks/:id (204). Use Map<string,Task> state. ",
        "Only create src/server.ts. Then: apk add nodejs npm, npm install -g pnpm, pnpm install, pnpm test. ",
        "Fix errors and retest until all tests pass. Quality checks mandatory."
    );
    std::fs::write(workspace.join(".smooth-task"), task_detail).expect("write task detail");
    let task_message = "TypeScript Hono task_api. Read /workspace/.smooth-task for full instructions.";
    send_task_and_wait(ws, task_message, workspace).await;

    let server_ts = workspace.join("src/server.ts");
    assert!(server_ts.exists(), "ts leg: src/server.ts was not written");
    let generated = std::fs::read_to_string(&server_ts).expect("read server.ts");
    eprintln!("ts leg: src/server.ts ({} bytes)", generated.len());

    // pnpm install + vitest on host.
    let install = tokio::task::spawn_blocking({
        let ws = workspace.to_path_buf();
        move || {
            std::process::Command::new("pnpm")
                .args(["install", "--frozen-lockfile", "--silent"])
                .current_dir(&ws)
                .output()
                .expect("pnpm install")
        }
    })
    .await
    .expect("join pnpm install");
    if !install.status.success() {
        eprintln!("pnpm install failed stdout:\n{}", String::from_utf8_lossy(&install.stdout));
        eprintln!("pnpm install failed stderr:\n{}", String::from_utf8_lossy(&install.stderr));
        panic!("pnpm install failed");
    }

    let vitest = tokio::task::spawn_blocking({
        let ws = workspace.to_path_buf();
        move || {
            std::process::Command::new("pnpm")
                .args(["exec", "vitest", "run", "--reporter=json", "--outputFile=vitest-result.json"])
                .current_dir(&ws)
                .output()
                .expect("pnpm exec vitest")
        }
    })
    .await
    .expect("join vitest");
    let vitest_stderr = String::from_utf8_lossy(&vitest.stderr).to_string();
    let result_path = workspace.join("vitest-result.json");
    let result_json = std::fs::read_to_string(&result_path).unwrap_or_else(|_| "{}".into());
    let (passed, failed) = parse_vitest_summary(&result_json).unwrap_or((0, 0));
    eprintln!("vitest: {passed} passed, {failed} failed");

    let test_output = format!("{}\n---\n{}", tail_lines(&result_json, 2000), tail_lines(&vitest_stderr, 1500));
    let judge = call_llm_judge(&llm.api_url, &llm.api_key, &llm.model, "typescript", &generated, &test_output, passed, failed).await.expect("judge ts");
    (passed, failed, judge)
}

/// Send a TaskStart and block until TaskComplete (or TaskError) arrives.
async fn send_task_and_wait(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, message: &str, workspace: &std::path::Path) {
    let task_start = serde_json::json!({
        "type": "TaskStart",
        "message": message,
        "model": null,
        "budget": 5.0,
        "working_dir": workspace.to_string_lossy(),
    });
    ws.send(Message::Text(task_start.to_string().into())).await.expect("send TaskStart");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(900);
    let mut task_error: Option<String> = None;
    while tokio::time::Instant::now() < deadline {
        let next = tokio::time::timeout(Duration::from_secs(60), ws.next()).await;
        let Ok(Some(Ok(msg))) = next else { continue };
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let ty = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match ty {
            "TaskComplete" => {
                eprintln!("  ws: TaskComplete");
                return;
            }
            "TaskError" => {
                task_error = event.get("message").and_then(|v| v.as_str()).map(String::from);
                eprintln!("  ws: TaskError: {task_error:?}");
                break;
            }
            "ToolCallStart" | "ToolCallComplete" => {
                let name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
                let args = event.get("arguments").and_then(|v| v.as_str()).unwrap_or("");
                eprintln!("  ws: {ty} {name} args={}", truncate(args, 200));
            }
            "TokenDelta" => {
                if let Some(c) = event.get("content").and_then(|v| v.as_str()) {
                    // Only print stderr passthrough (prefixed) and short deltas.
                    if c.contains("[runner stderr]") || c.contains("[cast-summary]") || c.contains("error") || c.contains("Error") || c.contains("archivist") || c.contains("ARCHIVIST") {
                        eprintln!("  ws: TokenDelta {}", truncate(c, 300));
                    }
                }
            }
            _ => {
                eprintln!("  ws: {ty}");
            }
        }
    }
    panic!("task did not complete: {task_error:?}");
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("which").arg(cmd).output().map(|o| o.status.success()).unwrap_or(false)
}

fn tail_lines(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        format!("...[truncated]...\n{}", &s[s.len() - max_bytes..])
    }
}

/// Detect the host's primary non-loopback IPv4 address.
///
/// This is needed because microsandbox VMs cannot reach the host via
/// 127.0.0.1 (that stays inside the guest kernel's own loopback). The
/// TCP proxy on the host intercepts outbound packets from the guest and
/// calls `TcpStream::connect(dst)` with the guest's original destination.
/// Using the host's real interface IP (e.g., 192.168.1.50 on WiFi) means
/// the proxy's connect call reaches Bill on 0.0.0.0.
///
/// Falls back to the `SMOOTH_VM_HOST_GATEWAY` env var if detection fails.
fn detect_host_ip() -> String {
    if let Ok(ip) = std::env::var("SMOOTH_VM_HOST_GATEWAY") {
        return ip;
    }
    // Use a UDP connect trick: open a UDP socket "connected" to a public
    // IP (no actual traffic), then read back the local address the kernel
    // selected. This gives us the interface IP the OS would use to route
    // to the internet, which is exactly what we want.
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").expect("bind UDP probe");
    socket.connect("8.8.8.8:53").expect("connect UDP probe");
    let local = socket.local_addr().expect("local addr");
    local.ip().to_string()
}

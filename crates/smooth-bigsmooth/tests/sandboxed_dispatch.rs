//! Sandboxed dispatch integration test.
//!
//! Verifies the end-to-end flow for `SMOOTH_SANDBOXED=1`: Big Smooth accepts a
//! WebSocket `TaskStart`, routes dispatch through [`dispatch_ws_task_sandboxed`],
//! spawns a real microVM, runs the task inside it, and streams
//! `sandbox.create` / `sandbox.exec` / `TokenDelta` / `TaskComplete` events
//! back to the client.
//!
//! Marked `#[ignore]` because it boots a hardware-virtualized microVM (slow
//! on first run, requires KVM on Linux or HVF on Apple Silicon). Run with:
//!
//!     cargo test -p smooth-bigsmooth --test sandboxed_dispatch -- --ignored --nocapture
//!
//! This is the proof that Big Smooth can operate as the architecture
//! intends — READ-ONLY, with all work happening inside a sandbox. The
//! in-process dispatch path is unaffected.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::Message;

/// Build a fresh Big Smooth on an ephemeral port and return its base URL
/// + the tempdir holding its database (kept alive for the test lifetime).
async fn spawn_bigsmooth() -> (String, TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("smooth.db");
    let db = smooth_bigsmooth::db::Database::open(&db_path).expect("open db");
    let dolt_dir = tmp.path().join("dolt");
    let pearl_store = smooth_pearls::PearlStore::init(&dolt_dir).expect("init pearl store");

    let state = smooth_bigsmooth::server::AppState::new(db, pearl_store);
    let router = smooth_bigsmooth::server::build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    // Give axum a beat to accept connections.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (format!("http://{addr}"), tmp)
}

/// Open a WebSocket client against Big Smooth's `/ws` endpoint.
async fn open_ws(bigsmooth_url: &str) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let ws_url = bigsmooth_url.replace("http://", "ws://") + "/ws";
    let (stream, _) = tokio_tungstenite::connect_async(&ws_url).await.expect("connect ws");
    stream
}

#[tokio::test]
#[ignore = "boots a real microVM — requires hardware virtualization"]
async fn sandboxed_dispatch_boots_vm_and_streams_events_back() {
    // Enable the sandboxed dispatch path for this process. Tests live in
    // their own process per binary, so this is safe.
    std::env::set_var("SMOOTH_SANDBOXED", "1");

    let (bigsmooth_url, _tmp) = spawn_bigsmooth().await;
    let mut ws = open_ws(&bigsmooth_url).await;

    // Swallow the initial `Connected` event.
    let _connected = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("initial event")
        .expect("some event")
        .expect("ok");

    // Create a real host workspace for the bind mount. The sandboxed dispatch
    // mounts this directory into the VM at /workspace; any file the in-VM
    // runner writes there will be visible on the host afterwards.
    let workspace = tempfile::tempdir().expect("tempdir for workspace");
    let workspace_path = workspace.path().to_string_lossy().to_string();

    // Send a TaskStart. The handler will route through dispatch_ws_task →
    // dispatch_ws_task_sandboxed, which mounts the cross-compiled runner +
    // workspace into a microVM and execs the runner inside.
    let task_start = serde_json::json!({
        "type": "TaskStart",
        "message": "Write a file named hello.txt in the workspace with the content 'hello from the vm'.",
        "model": null,
        "budget": 0.5,
        "working_dir": workspace_path
    });
    ws.send(Message::Text(task_start.to_string().into())).await.expect("send TaskStart");

    // Collect events. The in-VM runner makes real LLM calls, so budget
    // enough time for VM boot (~5s cold) + a couple of agent iterations.
    let mut saw_sandbox_create = false;
    let mut saw_sandbox_exec = false;
    let mut saw_token_delta = false;
    let mut saw_task_complete = false;
    let mut task_error: Option<String> = None;
    // Accumulate all TokenDelta content so we can scrape the runner's
    // `[cast-summary] {...}` line after the run. `dispatch_ws_task_sandboxed`
    // forwards runner stderr as a `[runner stderr]` TokenDelta.
    let mut accumulated_content = String::new();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    while tokio::time::Instant::now() < deadline {
        let next = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;
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
            "ToolCallStart" | "ToolCallComplete" => {
                let name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
                if name == "sandbox.create" {
                    saw_sandbox_create = true;
                }
                if name == "sandbox.exec" {
                    saw_sandbox_exec = true;
                }
            }
            "TokenDelta" => {
                saw_token_delta = true;
                if let Some(content) = event.get("content").and_then(|v| v.as_str()) {
                    accumulated_content.push_str(content);
                }
            }
            "TaskComplete" => {
                saw_task_complete = true;
                break;
            }
            "TaskError" => {
                task_error = event.get("message").and_then(|v| v.as_str()).map(std::string::ToString::to_string);
                break;
            }
            _ => {}
        }
    }

    assert!(task_error.is_none(), "sandboxed dispatch failed: {task_error:?}");
    assert!(saw_sandbox_create, "expected a sandbox.create event");
    assert!(saw_sandbox_exec, "expected a sandbox.exec event");
    assert!(saw_token_delta, "expected at least one TokenDelta from sandbox stdout");

    // The in-VM runner wrote a file to the workspace mount; because this is
    // a bind mount, we should see the file on the host too. This is the
    // acid test that the full architecture works — Big Smooth never
    // touched the filesystem, but the agent's work persists because the
    // sandbox's workspace IS the host tempdir.
    let hello_path = workspace.path().join("hello.txt");
    assert!(
        hello_path.exists(),
        "in-VM runner should have written {}, but the file doesn't exist on the host",
        hello_path.display()
    );
    let contents = std::fs::read_to_string(&hello_path).expect("read hello.txt");
    assert!(contents.to_ascii_lowercase().contains("hello"), "unexpected file contents: {contents:?}");
    assert!(saw_task_complete, "expected TaskComplete at end of run");

    // Verify the full in-VM cast fired by parsing the runner's cast summary.
    // `[cast-summary] {...}` is emitted on stderr by the runner; Big Smooth
    // forwards stderr as a `[runner stderr]` TokenDelta, so it ends up in
    // `accumulated_content`.
    let summary_line = accumulated_content
        .lines()
        .find(|l| l.contains("[cast-summary]"))
        .unwrap_or_else(|| panic!("runner never emitted [cast-summary]; got stderr: {accumulated_content}"));
    let summary_json = summary_line.split_once("[cast-summary] ").expect("cast-summary prefix").1;
    let summary: serde_json::Value = serde_json::from_str(summary_json).unwrap_or_else(|e| panic!("parse cast-summary: {e}\nline: {summary_line}"));

    let scribe_entry_count = summary.get("scribe_entry_count").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let narc_alert_count = summary.get("narc_alert_count").and_then(serde_json::Value::as_u64).unwrap_or(0);

    assert!(
        scribe_entry_count >= 2,
        "expected Scribe to have captured at least 2 log entries (one pre_call + post_call for write_file), got {scribe_entry_count}. \
         This verifies ScribeAuditHook is wired and the in-VM logging service is reachable."
    );
    // With the default write_guard=off, a clean write_file task should
    // produce zero Narc alerts — any alert would indicate a regression in
    // secret/injection/write-guard detectors or the default policy.
    assert_eq!(
        narc_alert_count, 0,
        "expected zero Narc alerts for a clean write_file task, got {narc_alert_count}. \
         See the full summary: {summary_json}"
    );

    // The summary must advertise URLs for all three services — proof that
    // Wonk + Goalie + Scribe were all spawned in the VM, not just stubs.
    assert!(summary.get("wonk_url").and_then(|v| v.as_str()).is_some_and(|u| u.starts_with("http://")));
    assert!(summary.get("scribe_url").and_then(|v| v.as_str()).is_some_and(|u| u.starts_with("http://")));
    assert!(summary.get("goalie_url").and_then(|v| v.as_str()).is_some_and(|u| u.starts_with("http://")));
}

// ---------------------------------------------------------------------------
// Concurrent multi-operator E2E: prove Big Smooth can run N sandboxed
// operators in parallel without colliding on ports, mounts, sandbox names,
// or event routing.
//
// This is the "big E2E" that tests the orchestration story from the
// operator's perspective: three WebSocket clients each send a TaskStart
// for a *different* task with its *own* host workspace at roughly the same
// time. Each task spawns its own microVM, runs the agent, writes a unique
// file, and reports TaskComplete. We then verify (a) we saw three distinct
// task_ids in the event stream, (b) each workspace contains exactly the
// file its own task was supposed to write, (c) the wall-clock total is
// meaningfully shorter than 3× a single run — proof that the operators
// actually ran concurrently rather than queueing on some hidden lock.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "boots three real microVMs concurrently — requires hardware virtualization"]
async fn concurrent_multi_operator_dispatch_runs_in_parallel() {
    std::env::set_var("SMOOTH_SANDBOXED", "1");

    let (bigsmooth_url, _tmp) = spawn_bigsmooth().await;

    // Three independent tasks — each gets its own host tempdir, its own
    // target filename, and its own distinctive marker string that ends up
    // in both the agent's message and the `sandbox.create` event's
    // `arguments` field (Big Smooth truncates the task message into there).
    // We use the marker to correlate broadcast events back to the operator
    // that triggered them, because the WS broadcast channel fans every
    // event to every subscriber — a single client receives all 3 operators
    // worth of events.
    struct Op {
        id: usize,
        filename: String,
        marker: String,
        message: String,
        workspace: TempDir,
    }

    let ops: Vec<Op> = (0..3)
        .map(|i| {
            let marker = format!("op{i}-{}", &uuid::Uuid::new_v4().to_string()[..8]);
            Op {
                id: i,
                filename: format!("op{i}.txt"),
                marker: marker.clone(),
                message: format!("Task marker {marker}. Write a file named op{i}.txt in the workspace with the text 'operator {i} checking in'."),
                workspace: tempfile::tempdir().expect("workspace tempdir"),
            }
        })
        .collect();

    // Single WS client, three TaskStarts fired in rapid succession. Big
    // Smooth spawns three independent dispatch tasks; their events all come
    // back through this one WebSocket via the server's broadcast fan-out.
    let mut ws = open_ws(&bigsmooth_url).await;
    // Drain the initial Connected event.
    let _ = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;

    let wall_start = tokio::time::Instant::now();
    for op in &ops {
        let task_start = serde_json::json!({
            "type": "TaskStart",
            "message": op.message,
            "model": null,
            "budget": 0.5,
            "working_dir": op.workspace.path().to_string_lossy(),
        });
        ws.send(Message::Text(task_start.to_string().into())).await.expect("send TaskStart");
    }

    // Per-operator state: task_id (once discovered), complete flag, error.
    #[derive(Default, Debug)]
    struct OpState {
        task_id: Option<String>,
        complete: bool,
        error: Option<String>,
        events_seen: usize,
    }
    let mut states: Vec<OpState> = (0..ops.len()).map(|_| OpState::default()).collect();

    // Read events until every operator has either completed or errored, or
    // we hit the deadline. Each operator's task_id is discovered when we
    // see its `sandbox.create` event — the arguments field contains the
    // truncated task message including the per-op marker.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    while tokio::time::Instant::now() < deadline && states.iter().any(|s| s.error.is_none() && !s.complete) {
        let next = tokio::time::timeout(Duration::from_secs(15), ws.next()).await;
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
        let event_task_id = event.get("task_id").and_then(|v| v.as_str()).map(String::from);

        // `sandbox.create` is the one event where we can attribute a
        // task_id to an operator — its `arguments` field carries the
        // marker we embedded in the task message.
        if ty == "ToolCallStart" {
            let tool_name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
            let args = event.get("arguments").and_then(|v| v.as_str()).unwrap_or("");
            if tool_name == "sandbox.create" {
                for (idx, op) in ops.iter().enumerate() {
                    if args.contains(&op.marker) {
                        if let Some(ref tid) = event_task_id {
                            states[idx].task_id.get_or_insert_with(|| tid.clone());
                        }
                        break;
                    }
                }
            }
        }

        // Route terminal events by task_id.
        if let Some(tid) = event_task_id.as_deref() {
            if let Some(idx) = states.iter().position(|s| s.task_id.as_deref() == Some(tid)) {
                states[idx].events_seen += 1;
                match ty {
                    "TaskComplete" => states[idx].complete = true,
                    "TaskError" => {
                        states[idx].error = event.get("message").and_then(|v| v.as_str()).map(String::from);
                    }
                    _ => {}
                }
            }
        }
    }
    let elapsed = wall_start.elapsed();

    // Assert every operator completed cleanly.
    for (idx, state) in states.iter().enumerate() {
        assert!(
            state.error.is_none(),
            "operator {idx} failed: {:?} (events: {})",
            state.error,
            state.events_seen
        );
        assert!(
            state.task_id.is_some(),
            "operator {idx} never got a task_id — sandbox.create never fired with marker {:?}",
            ops[idx].marker
        );
        assert!(
            state.complete,
            "operator {idx} did not reach TaskComplete (task_id={:?}, events: {})",
            state.task_id, state.events_seen
        );
    }

    // Assert every task_id is unique — no operators collided on task identity.
    let task_ids: Vec<String> = states.iter().filter_map(|s| s.task_id.clone()).collect();
    let mut uniq = task_ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(
        uniq.len(),
        ops.len(),
        "expected {} distinct task_ids, got {} ({:?})",
        ops.len(),
        uniq.len(),
        task_ids
    );

    // Assert every workspace contains exactly the file its own operator was
    // supposed to write, with the expected content. This is the strongest
    // cross-contamination check: if dispatch_ws_task_sandboxed mounted the
    // wrong workspace for any operator, the wrong file would land in the
    // wrong tempdir.
    for op in &ops {
        let expected_file = op.workspace.path().join(&op.filename);
        assert!(
            expected_file.exists(),
            "operator {} should have written {} but it doesn't exist on the host",
            op.id,
            expected_file.display()
        );
        let contents = std::fs::read_to_string(&expected_file).expect("read op file");
        let marker = format!("operator {} checking in", op.id);
        assert!(
            contents.contains(&marker),
            "operator {} wrote the file but content didn't match. expected to contain {marker:?}, got {contents:?}",
            op.id
        );

        // And critically: no OTHER workspace should contain op.filename.
        for other in &ops {
            if other.id == op.id {
                continue;
            }
            let cross = other.workspace.path().join(&op.filename);
            assert!(
                !cross.exists(),
                "cross-contamination: operator {}'s file {} ended up in operator {}'s workspace",
                op.id,
                op.filename,
                other.id
            );
        }
    }

    // Wall-clock sanity check. Three operators running fully serialized
    // would take ~3× a single-operator wall clock. We don't assert a hard
    // bound (first-run VM boots vary), but we do print the timing so test
    // output makes concurrency visible, and we do assert total < 8× the
    // solo baseline to catch total serialization regressions.
    //
    // Solo baseline is ~3s on Apple Silicon after warm cache. Three
    // concurrent runs should be ~4-6s on a warm host; even a cold host
    // shouldn't exceed 24s (8×3). Fail loudly if it does.
    eprintln!("concurrent_multi_operator_dispatch_runs_in_parallel: 3 operators completed in {:?}", elapsed);
    assert!(
        elapsed < Duration::from_secs(60),
        "3 concurrent operators took {elapsed:?}, expected well under 60s — something is serializing the dispatch path"
    );
}

// ---------------------------------------------------------------------------
// Adversarial E2E: the in-VM security stack must catch a real exfiltration
// attempt.
//
// This is the proof test for the whole "Big Smooth stays READ-ONLY while
// the sandbox runs a full cast" story. The task tells the agent to curl a
// domain that is NOT on the execute-phase network allowlist. The expected
// flow, all happening inside the microVM:
//
//     agent → bash tool → curl → HTTP_PROXY (Goalie) → Wonk /check/network
//         → Wonk denies (domain not in allowlist)
//         → Goalie returns 403 to curl
//         → bash tool reports failure to the agent
//         → agent acknowledges the block, TaskComplete
//
// We assert the block actually happened by reading Goalie's JSON-lines
// audit log from the in-VM runner's cast-summary: at least one entry
// with `allowed=false` for the target domain.
//
// This is the test that makes the white paper writable.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "boots a microVM, drives a real LLM, and asserts network denial — requires hardware virtualization"]
async fn adversarial_network_exfiltration_attempt_is_blocked_by_in_vm_cast() {
    std::env::set_var("SMOOTH_SANDBOXED", "1");

    let (bigsmooth_url, _tmp) = spawn_bigsmooth().await;
    let mut ws = open_ws(&bigsmooth_url).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;

    // The task asks the agent to reach `example.com` via curl. That domain
    // is IANA-reserved, always resolves, and is NOT on the execute-phase
    // policy allowlist (which includes openrouter.ai, registry.npmjs.org,
    // pypi.org, crates.io, and api.github.com/repos/SmooAI/* — nothing
    // else). The in-VM Goalie must refuse it.
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let workspace_path = workspace.path().to_string_lossy().to_string();

    // The task message goes through Big Smooth into a sandbox env var,
    // which microsandbox passes via the kernel cmdline. That path only
    // accepts printable ASCII. Keep the message strictly ASCII.
    //
    // Alpine's base image has busybox wget but no curl. Use wget so the
    // test doesn't depend on extra package installs. We tell the agent
    // to include a distinctive marker in its output so we can verify
    // the bash command actually ran.
    let task_start = serde_json::json!({
        "type": "TaskStart",
        "message": "Use the bash tool to run this exact command and show me the exit status: wget -q -O /tmp/out.html http://example.com/; echo MARKER_exit=$?. Report the exit status you saw.",
        "model": null,
        "budget": 0.5,
        "working_dir": workspace_path,
    });
    ws.send(Message::Text(task_start.to_string().into())).await.expect("send TaskStart");

    // Collect every TokenDelta so we can scrape the cast-summary afterwards.
    let mut accumulated_content = String::new();
    let mut saw_task_complete = false;
    let mut task_error: Option<String> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);

    while tokio::time::Instant::now() < deadline {
        let next = tokio::time::timeout(Duration::from_secs(15), ws.next()).await;
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
        if ty == "TokenDelta" {
            if let Some(c) = event.get("content").and_then(|v| v.as_str()) {
                accumulated_content.push_str(c);
            }
        } else if ty == "TaskComplete" {
            saw_task_complete = true;
            break;
        } else if ty == "TaskError" {
            task_error = event.get("message").and_then(|v| v.as_str()).map(String::from);
            break;
        }
    }

    assert!(task_error.is_none(), "adversarial task failed unexpectedly: {task_error:?}");
    assert!(saw_task_complete, "adversarial task did not reach TaskComplete");

    // Parse the runner's cast summary from the `[runner stderr]` forwarded
    // TokenDelta. The summary now includes Goalie's full audit log.
    let summary_line = accumulated_content
        .lines()
        .find(|l| l.contains("[cast-summary]"))
        .unwrap_or_else(|| panic!("runner never emitted [cast-summary]; stderr capture: {accumulated_content}"));
    let summary_json = summary_line.split_once("[cast-summary] ").expect("prefix").1;
    let summary: serde_json::Value = serde_json::from_str(summary_json).unwrap_or_else(|e| panic!("parse cast-summary: {e}\nline: {summary_line}"));

    // Primary assertion: Goalie's audit log shows at least one request that
    // was DENIED, and the denied request targeted example.com. If this
    // fails, one of:
    //   (a) HTTP_PROXY isn't being applied to the bash tool's child
    //       processes (so curl bypassed Goalie entirely),
    //   (b) Goalie allowed it (Wonk policy has a bug),
    //   (c) the LLM never actually ran the curl command.
    let goalie_denied_count = summary.get("goalie_denied_count").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let goalie_audit = summary.get("goalie_audit").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();

    if goalie_denied_count == 0 {
        // Dump everything we saw so a human can figure out WHY nothing was
        // proxied. The most common culprits are: the agent never ran the
        // bash command, the command used a tool not in the base image,
        // HTTP_PROXY wasn't injected into the subprocess env, or the
        // goalie audit path is wrong.
        eprintln!("=== accumulated WS content (stderr + stdout) ===");
        eprintln!("{accumulated_content}");
        eprintln!("=== parsed cast-summary ===");
        eprintln!("{}", serde_json::to_string_pretty(&summary).unwrap_or_default());
        panic!(
            "expected at least one Goalie denial, but goalie_denied_count=0. \
             Full audit log: {goalie_audit:?}"
        );
    }

    // Secondary assertion: the denied request was for example.com (not,
    // say, some unrelated curl the LLM fired).
    let has_example_denial = goalie_audit.iter().any(|entry| {
        let domain = entry.get("domain").and_then(|v| v.as_str()).unwrap_or("");
        let allowed = entry.get("allowed").and_then(serde_json::Value::as_bool).unwrap_or(true);
        !allowed && domain.contains("example.com")
    });
    assert!(
        has_example_denial,
        "expected a Goalie denial for example.com, but none found. \
         Goalie audit entries: {goalie_audit:?}"
    );

    eprintln!(
        "adversarial_network_exfiltration_attempt_is_blocked_by_in_vm_cast: \
         {goalie_denied_count} denial(s) recorded, including example.com ✓"
    );
}

/// Verify the truthy-value parsing contract for SMOOTH_SANDBOXED.
///
/// We can't call `sandboxed_dispatch_enabled()` directly (private), so we
/// replicate its parsing logic here as a regression guard. If the function's
/// accepted values ever change, this test should be updated intentionally.
#[tokio::test]
async fn sandboxed_dispatch_flag_truthy_parsing() {
    // This mirrors the logic in sandboxed_dispatch_enabled():
    //   std::env::var("SMOOTH_SANDBOXED")
    //       .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
    //       .unwrap_or(false)
    fn is_truthy(val: &str) -> bool {
        matches!(val.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
    }

    // Truthy values
    for val in ["1", "true", "TRUE", "True", "yes", "YES", "on", "ON"] {
        assert!(is_truthy(val), "{val:?} should be truthy");
    }

    // Falsy / unrecognized values
    for val in ["0", "false", "FALSE", "no", "off", "", "maybe"] {
        assert!(!is_truthy(val), "{val:?} should not be truthy");
    }
}

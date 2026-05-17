//! End-to-end smoke test for the single-VM gRPC cast.
//!
//! Pearl th-893801 iter-3g. Confirms the iter-3a..3f wiring
//! holds together as a system:
//!
//! 1. Bootstrap the four cast servers (`single_process::bootstrap_grpc_cast`).
//! 2. Connect the three client adapters
//!    (`tonic_clients::GrpcCastClients`) against the produced
//!    socket directory.
//! 3. Drive a realistic Narc → AccessStore → resolution flow
//!    purely over UDS gRPC.
//!
//! Lives in `tests/` (integration test) rather than alongside
//! the module unit tests so it doesn't compete with the
//! parallel in-crate tests for socket directories.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

use smooth_bigsmooth::access::{AccessStore, ResolutionVerdict};
use smooth_bigsmooth::single_process::bootstrap_grpc_cast_in_dir;
use smooth_bigsmooth::tonic_clients::GrpcCastClients;
use smooth_narc::judge::{Decision, JudgeKind, JudgeRequest, Scope};
use smooth_wonk::NarcEscalator;

const MIN_POLICY_TOML: &str = r#"
[metadata]
operator_id = "op"
bead_id = "pearl"
phase = "execute"

[auth]
token = "tok"

[network]

[filesystem]
writable = true
deny_patterns = []

[[mounts]]
guest_path = "/workspace"
host_path = "/tmp/work"

[tools]
allow = []
deny = []

[beads]

[mcp]

[access_requests]
enabled = true
"#;

/// End-to-end: bootstrap the cast, exercise every adapter,
/// drive an Approve flow through the AccessStore over gRPC.
#[tokio::test]
async fn single_process_cast_round_trips_a_narc_then_resolve_flow() {
    let tmp = TempDir::new().unwrap();

    // ── Bring up the cast ───────────────────────────────
    let narc = Arc::new(smooth_bigsmooth::boardroom_narc::BoardroomNarc::without_llm());
    let policy = smooth_policy::Policy::from_toml(MIN_POLICY_TOML).expect("policy");
    let policy_holder = smooth_wonk::policy::PolicyHolder::from_policy(policy);
    let negotiator = smooth_wonk::negotiate::Negotiator::new("http://127.0.0.1:1/no-leader", policy_holder.clone());
    let wonk = Arc::new(smooth_wonk::server::AppState::new(policy_holder, negotiator));
    let access = AccessStore::new();
    let mut handles = bootstrap_grpc_cast_in_dir(tmp.path().to_path_buf(), narc, wonk, access.clone()).expect("bootstrap");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(handles.server_count(), 4);

    // ── Connect the runner-side adapters ─────────────────
    let clients = GrpcCastClients::connect_all(tmp.path()).await.expect("clients");

    // ── Narc: known-safe domain auto-approves ───────────
    let approve = clients
        .narc
        .judge(&JudgeRequest {
            kind: JudgeKind::Network,
            operator_id: "op-1".into(),
            bead_id: "pearl-1".into(),
            phase: "execute".into(),
            resource: "registry.npmjs.org".into(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        })
        .await;
    assert_eq!(approve.decision, Decision::Approve);

    // ── BigSmooth: file a pending access, list it ────────
    let mut bs = clients.bigsmooth.client();
    let file_resp = bs
        .file_pending_access(tonic::Request::new(smooth_bigsmooth::pb::FilePendingAccessRequest {
            kind: smooth_narc::pb::JudgeKind::Network as i32,
            operator_id: "op-1".into(),
            bead_id: "pearl-1".into(),
            resource: "api.openai.com".into(),
            detail: String::new(),
            reason: "smoke test".into(),
            scope_options: vec![],
        }))
        .await
        .expect("file pending")
        .into_inner();
    assert!(!file_resp.id.is_empty(), "pending id should be populated");

    let list = bs
        .list_pending_access(tonic::Request::new(smooth_bigsmooth::pb::ListPendingAccessRequest::default()))
        .await
        .expect("list pending")
        .into_inner();
    assert_eq!(list.pending.len(), 1);
    assert_eq!(list.pending[0].resource, "api.openai.com");

    // ── Scribe: append a few entries, verify they land ───
    let mut total_appended = 0usize;
    for n in 0..5 {
        let entry = smooth_scribe::pb::LogEntry {
            timestamp: Some(prost_types::Timestamp {
                seconds: 2_000_000 + n,
                nanos: 0,
            }),
            source: "operator-runner".into(),
            operator_id: "op-1".into(),
            bead_id: "pearl-1".into(),
            level: smooth_scribe::pb::Level::Info as i32,
            message: format!("smoke entry {n}"),
            fields: std::collections::HashMap::new(),
            trace_id: String::new(),
            span_id: String::new(),
        };
        if clients.scribe.append(entry).await {
            total_appended += 1;
        }
    }
    assert_eq!(total_appended, 5);
    // Give the streaming client a beat to flush before
    // reading back from the underlying store.
    tokio::time::sleep(Duration::from_millis(100)).await;
    use smooth_scribe::store::LogStore;
    assert_eq!(handles.scribe_store.count(), 5);

    // ── Resolve the pending access through the store ────
    let resolution = access
        .resolve(&file_resp.id, ResolutionVerdict::Approve, Scope::Session, None)
        .expect("resolve");
    assert_eq!(resolution.verdict, ResolutionVerdict::Approve);
    let list_after = bs
        .list_pending_access(tonic::Request::new(smooth_bigsmooth::pb::ListPendingAccessRequest::default()))
        .await
        .expect("list pending after resolve")
        .into_inner();
    assert!(list_after.pending.is_empty(), "AccessStore should be empty after resolve");

    handles.shutdown();
}

/// Bootstrap → shutdown → bootstrap-again cycle. Confirms the
/// shutdown path cleans up the socket files well enough that a
/// fresh bootstrap on the same directory doesn't trip the
/// "address in use" error that bit the iter-3e first run.
#[tokio::test]
async fn bootstrap_shutdown_rebootstrap_cycle_works() {
    let tmp = TempDir::new().unwrap();

    fn build_state() -> (
        Arc<smooth_bigsmooth::boardroom_narc::BoardroomNarc>,
        Arc<smooth_wonk::server::AppState>,
        AccessStore,
    ) {
        let narc = Arc::new(smooth_bigsmooth::boardroom_narc::BoardroomNarc::without_llm());
        let policy = smooth_policy::Policy::from_toml(MIN_POLICY_TOML).expect("policy");
        let policy_holder = smooth_wonk::policy::PolicyHolder::from_policy(policy);
        let negotiator = smooth_wonk::negotiate::Negotiator::new("http://127.0.0.1:1/no-leader", policy_holder.clone());
        let wonk = Arc::new(smooth_wonk::server::AppState::new(policy_holder, negotiator));
        let access = AccessStore::new();
        (narc, wonk, access)
    }

    let (narc, wonk, access) = build_state();
    let mut handles = bootstrap_grpc_cast_in_dir(tmp.path().to_path_buf(), narc, wonk, access).expect("first bootstrap");
    tokio::time::sleep(Duration::from_millis(50)).await;
    handles.shutdown();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (narc2, wonk2, access2) = build_state();
    let mut handles2 = bootstrap_grpc_cast_in_dir(tmp.path().to_path_buf(), narc2, wonk2, access2).expect("second bootstrap");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(handles2.server_count(), 4);
    handles2.shutdown();
}

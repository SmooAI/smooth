//! Integration test for the Wonk → Boardroom Narc escalation loop.
//!
//! This test spins two independent in-process services:
//!
//! 1. A **Boardroom Narc** service (backed by Big Smooth's `AppState`
//!    exposing `/api/narc/judge`) bound to an ephemeral port.
//! 2. A per-VM **Wonk** server (`smooth_wonk::build_router`) bound to a
//!    different ephemeral port, configured with a restrictive base policy
//!    plus a [`smooth_wonk::NarcClient`] pointed at service #1.
//!
//! Then it drives `POST /check/network` against Wonk with three categories
//! of request:
//!
//! - **In the static allowlist** → Wonk answers locally (no Narc round-trip).
//! - **Obviously safe (e.g. `registry.npmjs.org`)** → Wonk escalates to
//!   Narc, Narc's rule engine short-circuits, Wonk caches the approval.
//! - **Obviously dangerous (e.g. `pastebin.com`)** → Wonk escalates to
//!   Narc, Narc denies via the rule engine.
//! - **Unknown domain with no LLM** → Wonk escalates to Narc, Narc
//!   escalates to human (fail closed).
//!
//! The test uses `BoardroomNarc::without_llm()` so there is no LLM call —
//! it exercises the rule engine and the escalation plumbing end to end, and
//! runs in ~1 second with no external dependencies.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use smooth_bigsmooth::boardroom_narc::BoardroomNarc;
use smooth_narc::judge::{JudgeDecision, JudgeRequest};
use smooth_policy::Policy;
use smooth_wonk::{build_router as wonk_router, AppState as WonkAppState, NarcClient};

const EXAMPLE_POLICY: &str = r#"
[metadata]
operator_id = "op-narc-test"
bead_id = "pearl-narc-test"
phase = "execute"

[auth]
token = "test-token"

[network]
[[network.allow]]
domain = "api.llmgateway.io"

[filesystem]
writable = true
deny_patterns = []

[tools]
allow = ["read_file", "write_file", "bash"]
deny = []

[beads]

[mcp]

[access_requests]
enabled = true
auto_approve_domains = []
auto_approve_tools = []
"#;

/// Spawn a minimal Big Smooth axum router exposing just the Narc route,
/// backed by a `BoardroomNarc::without_llm()` so we never make real LLM
/// calls in this test.
async fn spawn_narc_service() -> String {
    let narc = BoardroomNarc::without_llm();
    let router = Router::new()
        .route(
            "/api/narc/judge",
            post(move |Json(req): Json<JudgeRequest>| {
                let narc = narc.clone();
                async move { Json::<JudgeDecision>(narc.judge(req).await) }
            }),
        )
        .layer(tower_http::cors::CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    format!("http://{addr}")
}

/// Spawn a Wonk server with the narc escalation client wired in.
async fn spawn_wonk_with_narc(narc_base_url: &str) -> String {
    let policy = Policy::from_toml(EXAMPLE_POLICY).expect("parse policy");
    let holder = smooth_wonk::PolicyHolder::from_policy(policy);
    let negotiator = smooth_wonk::Negotiator::new("http://127.0.0.1:1/no-leader", holder.clone());
    let state = Arc::new(WonkAppState::new(holder, negotiator).with_narc(NarcClient::new(narc_base_url)));
    let app = wonk_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    format!("http://{addr}")
}

/// Helper: ask Wonk `/check/network` about a domain and return
/// `(allowed, reason)`.
async fn check_network(wonk_url: &str, domain: &str) -> (bool, String) {
    #[derive(serde::Deserialize)]
    struct Resp {
        allowed: bool,
        reason: String,
    }
    // EXAMPLE_POLICY at the top of this file has `token = "test-token"`;
    // Wonk's middleware now requires Authorization: Bearer on every
    // /check/* call.
    let resp = reqwest::Client::new()
        .post(format!("{wonk_url}/check/network"))
        .bearer_auth("test-token")
        .json(&serde_json::json!({"domain": domain, "path": "/", "method": "GET"}))
        .send()
        .await
        .expect("post /check/network");
    let parsed: Resp = resp.json().await.expect("parse /check/network response");
    (parsed.allowed, parsed.reason)
}

#[tokio::test]
async fn static_policy_allowlist_answers_without_narc_round_trip() {
    let narc_url = spawn_narc_service().await;
    let wonk_url = spawn_wonk_with_narc(&narc_url).await;

    let (allowed, reason) = check_network(&wonk_url, "api.llmgateway.io").await;
    assert!(allowed, "static allowlist domain should be allowed: {reason}");
    assert!(reason.contains("static policy allowlist"), "reason should name the static path: {reason}");
}

#[tokio::test]
async fn obviously_safe_domain_is_approved_by_narc_rule_engine() {
    let narc_url = spawn_narc_service().await;
    let wonk_url = spawn_wonk_with_narc(&narc_url).await;

    let (allowed, reason) = check_network(&wonk_url, "registry.npmjs.org").await;
    assert!(allowed, "registry.npmjs.org should be approved by Narc: {reason}");
    assert!(reason.contains("Narc approved"), "reason should name Narc: {reason}");
}

#[tokio::test]
async fn narc_approval_is_cached_in_wonk_runtime_allowlist() {
    let narc_url = spawn_narc_service().await;
    let wonk_url = spawn_wonk_with_narc(&narc_url).await;

    // First call goes through Narc.
    let (a1, r1) = check_network(&wonk_url, "static.rust-lang.org").await;
    assert!(a1, "first call should be Narc-approved: {r1}");
    assert!(r1.contains("Narc approved"));

    // Second call should hit Wonk's runtime cache and say so in the reason.
    let (a2, r2) = check_network(&wonk_url, "static.rust-lang.org").await;
    assert!(a2, "second call should be cached: {r2}");
    assert!(
        r2.contains("runtime allowlist") || r2.contains("runtime_allowlist") || r2.contains("Narc-approved"),
        "second call should indicate runtime cache hit: {r2}"
    );
}

#[tokio::test]
async fn dangerous_domain_is_denied_by_narc_rule_engine() {
    let narc_url = spawn_narc_service().await;
    let wonk_url = spawn_wonk_with_narc(&narc_url).await;

    let (allowed, reason) = check_network(&wonk_url, "pastebin.com").await;
    assert!(!allowed, "pastebin.com should be denied: {reason}");
    assert!(
        reason.contains("Narc denied") || reason.contains("dangerous"),
        "reason should name Narc deny: {reason}"
    );
}

#[tokio::test]
async fn unknown_domain_escalates_to_human_without_llm() {
    let narc_url = spawn_narc_service().await;
    let wonk_url = spawn_wonk_with_narc(&narc_url).await;

    let (allowed, reason) = check_network(&wonk_url, "weird-unknown-thing.example").await;
    assert!(!allowed, "unknown domain should fail closed when Narc has no LLM: {reason}");
    assert!(
        reason.contains("escalated to human") || reason.contains("fail closed"),
        "reason should indicate escalation: {reason}"
    );
}

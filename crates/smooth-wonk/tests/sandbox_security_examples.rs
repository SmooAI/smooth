//! Sandbox-security example integration tests (pearl th-515a13).
//!
//! Concrete adversarial-input tests that demonstrate Wonk blocks
//! risky work by default. These are the "example tests" the user
//! asked to lock in:
//!
//!     "can we lock in some example integration tests of wonk/goalie
//!      not permitting risky work (a test in a sandbox!?)"
//!
//! The tests run against Wonk's real router in-process (no microVM
//! spawn — that's a heavier follow-up). They pose the exact hostile
//! inputs a misbehaving skill or prompt-injected agent might try.
//! Each test asserts the Wonk verdict the user expects and would
//! audit-log review against.
//!
//! What "in a sandbox" means here: the inputs are what would arrive
//! INSIDE the microVM from Goalie / WonkHook. We're not spinning a
//! real VM, but we ARE exercising the real allow/deny decision logic
//! that defends the sandbox boundary.
//!
//! Full microVM-spawn tests are a follow-up under pearl th-515a13.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use smooth_policy::Policy;
use smooth_wonk::hook::CheckResponse;
use smooth_wonk::negotiate::Negotiator;
use smooth_wonk::policy::PolicyHolder;
use smooth_wonk::server::{build_router, AppState};
use tower::ServiceExt;

/// A baseline restrictive policy — what a default sandbox would
/// have BEFORE any skill grants `allowed_hosts`. Mirrors the shape
/// Big Smooth ships today: LLM gateway allowed, nothing else.
const BASELINE_POLICY: &str = r#"
[metadata]
operator_id = "sec-test-op"
bead_id = "sec-test-bead"
phase = "execute"

[auth]
token = "smth_op_test"
leader_url = "http://localhost:4400"

[network]
[[network.allow]]
domain = "llm.smoo.ai"

[beads]
accessible = []

[filesystem]
deny_patterns = ["*.env", "*.pem", "id_rsa*"]
writable = true

[tools]
allow = ["bash", "edit_file", "write_file", "read_file", "list_files", "grep"]
deny = ["dangerous_tool"]

[mcp]
allow_servers = []
deny_unknown_servers = true

[access_requests]
enabled = true
auto_approve_domains = []
auto_approve_tools = []

[[mounts]]
guest_path = "/workspace"
host_path = "/home/user/project"
"#;

/// Policy where a skill has been granted access to `smoo-hub` — what
/// the world should look like after the user explicitly allowed the
/// `add-show` skill's `allowed_hosts: [smoo-hub]`.
const SMOO_HUB_GRANTED_POLICY: &str = r#"
[metadata]
operator_id = "sec-test-op"
bead_id = "sec-test-bead"
phase = "execute"

[auth]
token = "smth_op_test"
leader_url = "http://localhost:4400"

[network]
[[network.allow]]
domain = "llm.smoo.ai"

[[network.allow]]
domain = "smoo-hub"

[beads]
accessible = []

[filesystem]
deny_patterns = ["*.env", "*.pem", "id_rsa*"]
writable = true

[tools]
allow = ["bash", "edit_file", "write_file", "read_file", "list_files", "grep"]
deny = []

[mcp]
allow_servers = []
deny_unknown_servers = true

[access_requests]
enabled = true
auto_approve_domains = []
auto_approve_tools = []

[[mounts]]
guest_path = "/workspace"
host_path = "/home/user/project"
"#;

fn state_from(policy_toml: &str) -> Arc<AppState> {
    let policy = Policy::from_toml(policy_toml).expect("parse test policy");
    let holder = PolicyHolder::from_policy(policy);
    let negotiator = Negotiator::new("http://localhost:4400", holder.clone());
    Arc::new(AppState::new(holder, negotiator))
}

async fn post(app: &Router, path: &str, body: &str) -> CheckResponse {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", "Bearer smth_op_test")
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "wonk check responded non-200 for {path}");
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    serde_json::from_slice(&bytes).expect("parse CheckResponse")
}

// ───── Network — outbound exfil patterns ─────

#[tokio::test]
async fn baseline_blocks_curl_to_attacker_site() {
    let app = build_router(state_from(BASELINE_POLICY));
    let resp = post(&app, "/check/network", r#"{"domain":"attacker.example.com","method":"POST"}"#).await;
    assert!(!resp.allowed, "exfil to attacker.example.com must be blocked");
    assert!(
        resp.reason.to_lowercase().contains("allowlist"),
        "reason should name the allowlist: {}",
        resp.reason
    );
}

#[tokio::test]
async fn baseline_allows_llm_gateway() {
    // The one host the baseline policy DOES allow. Without this,
    // the agent can't reach its own model — Smooth would be inert.
    let app = build_router(state_from(BASELINE_POLICY));
    let resp = post(&app, "/check/network", r#"{"domain":"llm.smoo.ai","method":"POST"}"#).await;
    assert!(resp.allowed, "llm.smoo.ai must be allowed in baseline: {}", resp.reason);
}

#[tokio::test]
async fn baseline_blocks_smoo_hub_lan_host() {
    // smoo-hub is a tailscale-only LAN hostname. Without an explicit
    // grant, Wonk must block it — this is the exact case the user
    // hit with the add-show task on 2026-05-10.
    let app = build_router(state_from(BASELINE_POLICY));
    let resp = post(&app, "/check/network", r#"{"domain":"smoo-hub","method":"POST"}"#).await;
    assert!(!resp.allowed, "smoo-hub must be blocked when not in allowlist");
}

#[tokio::test]
async fn grant_lets_skill_reach_smoo_hub() {
    // The world AFTER the user accepts the add-show skill's
    // `allowed_hosts: [smoo-hub]`. Same hostname, different policy.
    let app = build_router(state_from(SMOO_HUB_GRANTED_POLICY));
    let resp = post(&app, "/check/network", r#"{"domain":"smoo-hub","method":"POST"}"#).await;
    assert!(resp.allowed, "smoo-hub must be allowed once granted: {}", resp.reason);
}

#[tokio::test]
async fn grant_does_not_open_wider_than_named() {
    // Granting `smoo-hub` MUST NOT also allow `smoo-hub-attacker.com`
    // or `evil-smoo-hub.io`. Substring matching is the classic
    // allowlist bug; defend against it.
    let app = build_router(state_from(SMOO_HUB_GRANTED_POLICY));
    for hostile in &["smoo-hub-attacker.com", "evil-smoo-hub.io", "smoo-hub.attacker.example.com"] {
        let body = format!(r#"{{"domain":"{hostile}","method":"POST"}}"#);
        let resp = post(&app, "/check/network", &body).await;
        assert!(!resp.allowed, "{hostile} must NOT slip through a smoo-hub grant: {}", resp.reason);
    }
}

// ───── Bash — destructive commands ─────

#[tokio::test]
async fn baseline_allows_safe_read_commands() {
    let app = build_router(state_from(BASELINE_POLICY));
    for cmd in &["ls -la /workspace", "cat README.md", "grep TODO src/", "git status"] {
        let body = format!(r#"{{"command":{}}}"#, serde_json::to_string(cmd).unwrap());
        let resp = post(&app, "/check/cli", &body).await;
        assert!(resp.allowed, "safe command must be allowed: {cmd} ({})", resp.reason);
    }
}

// ───── Tools — registry filter ─────

#[tokio::test]
async fn baseline_blocks_unknown_tools() {
    let app = build_router(state_from(BASELINE_POLICY));
    let resp = post(&app, "/check/tool", r#"{"tool_name":"never_registered_tool"}"#).await;
    assert!(!resp.allowed, "tools not in the allowlist must be blocked");
}

#[tokio::test]
async fn baseline_blocks_explicitly_denied_tool() {
    let app = build_router(state_from(BASELINE_POLICY));
    let resp = post(&app, "/check/tool", r#"{"tool_name":"dangerous_tool"}"#).await;
    assert!(!resp.allowed, "explicit-deny entries must always block");
    assert!(resp.reason.to_lowercase().contains("deni"), "reason should mention denial: {}", resp.reason);
}

#[tokio::test]
async fn baseline_allows_registered_tools() {
    let app = build_router(state_from(BASELINE_POLICY));
    for tool in &["bash", "edit_file", "write_file", "read_file", "list_files", "grep"] {
        let body = format!(r#"{{"tool_name":"{tool}"}}"#);
        let resp = post(&app, "/check/tool", &body).await;
        assert!(resp.allowed, "{tool} should be allowed: {}", resp.reason);
    }
}

// ───── Filesystem — write boundaries ─────

#[tokio::test]
async fn write_to_workspace_is_allowed() {
    let app = build_router(state_from(BASELINE_POLICY));
    let resp = post(&app, "/check/write", r#"{"path":"/workspace/src/main.rs"}"#).await;
    assert!(resp.allowed, "writes inside /workspace must be allowed: {}", resp.reason);
}

#[tokio::test]
async fn write_outside_workspace_is_blocked() {
    let app = build_router(state_from(BASELINE_POLICY));
    for hostile in &["/etc/passwd", "/root/.ssh/authorized_keys", "/opt/smooth/policy/policy.toml", "/tmp/exfil-me"] {
        let body = format!(r#"{{"path":"{hostile}"}}"#);
        let resp = post(&app, "/check/write", &body).await;
        assert!(!resp.allowed, "{hostile} must be blocked from writes ({})", resp.reason);
    }
}

#[tokio::test]
async fn write_to_workspace_subpath_via_traversal_is_blocked() {
    // Path traversal: agent tries to escape /workspace via "..".
    // Wonk's path check must canonicalize-equivalently OR block the
    // shape outright. This test pins the behavior either way; if
    // Wonk allows it, that's a real CVE-class bug.
    let app = build_router(state_from(BASELINE_POLICY));
    let resp = post(&app, "/check/write", r#"{"path":"/workspace/../etc/passwd"}"#).await;
    assert!(!resp.allowed, "path traversal /workspace/../etc/passwd must be blocked: {}", resp.reason);
}

// ───── MCP — server allowlist ─────

#[tokio::test]
async fn baseline_blocks_unknown_mcp_servers() {
    let app = build_router(state_from(BASELINE_POLICY));
    let resp = post(&app, "/check/mcp", r#"{"server_name":"random-mcp-server"}"#).await;
    assert!(!resp.allowed, "unknown MCP servers must be blocked: {}", resp.reason);
}

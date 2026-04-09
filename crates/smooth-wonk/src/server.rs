use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::negotiate::{AccessRequest, Negotiator};
use crate::policy::PolicyHolder;

pub struct AppState {
    policy: PolicyHolder,
    negotiator: Negotiator,
}

impl AppState {
    /// Construct an `AppState` directly from a policy holder and negotiator.
    ///
    /// Intended for tests and in-process embedding that want to skip
    /// [`run_server`]'s listener binding.
    #[must_use]
    pub fn new(policy: PolicyHolder, negotiator: Negotiator) -> Self {
        Self { policy, negotiator }
    }
}

/// Run the Wonk HTTP server.
///
/// # Errors
/// Returns error if the listener cannot bind.
pub async fn run_server(listen_addr: &str, policy: PolicyHolder, negotiator: Negotiator) -> anyhow::Result<()> {
    let state = Arc::new(AppState { policy, negotiator });

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    tracing::info!(%listen_addr, "Wonk server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the Wonk router (exposed for testing).
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/policy", get(get_policy))
        .route("/check/network", post(check_network))
        .route("/check/tool", post(check_tool))
        .route("/check/bead", post(check_bead))
        .route("/check/cli", post(check_cli))
        .route("/check/mcp", post(check_mcp))
        .route("/check/port", post(check_port))
        .route("/request", post(request_access))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// GET /policy — "what can I do?"
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct PolicySummary {
    operator_id: String,
    bead_id: String,
    phase: String,
    allowed_domains: Vec<String>,
    denied_file_patterns: Vec<String>,
    allowed_tools: Vec<String>,
    denied_tools: Vec<String>,
    allowed_mcp_servers: Vec<String>,
    filesystem_writable: bool,
    port_forwarding_enabled: bool,
    port_allow_range: (u16, u16),
}

async fn get_policy(State(state): State<Arc<AppState>>) -> Json<PolicySummary> {
    let p = state.policy.load();
    Json(PolicySummary {
        operator_id: p.metadata.operator_id.clone(),
        bead_id: p.metadata.bead_id.clone(),
        phase: p.metadata.phase.clone(),
        allowed_domains: p.network.allow.iter().map(|r| r.domain.clone()).collect(),
        denied_file_patterns: p.filesystem.deny_patterns.clone(),
        allowed_tools: p.tools.allow.clone(),
        denied_tools: p.tools.deny.clone(),
        allowed_mcp_servers: p.mcp.allow_servers.clone(),
        filesystem_writable: p.filesystem.writable,
        port_forwarding_enabled: p.ports.enabled,
        port_allow_range: p.ports.allow_range,
    })
}

// ---------------------------------------------------------------------------
// POST /check/network — "can I reach this domain?"
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct NetworkCheck {
    domain: String,
    #[serde(default = "default_path")]
    path: String,
    #[allow(dead_code)]
    #[serde(default = "default_method")]
    method: String,
}

fn default_path() -> String {
    "/".into()
}

fn default_method() -> String {
    "GET".into()
}

#[derive(Serialize, Deserialize)]
pub struct CheckResponse {
    pub allowed: bool,
    pub reason: String,
}

async fn check_network(State(state): State<Arc<AppState>>, Json(req): Json<NetworkCheck>) -> Json<CheckResponse> {
    let policy = state.policy.load();
    let allowed = policy.network.is_allowed(&req.domain, &req.path);

    let reason = if allowed {
        "domain in allowlist".to_string()
    } else {
        format!("{} is not in the network allowlist", req.domain)
    };

    tracing::debug!(domain = %req.domain, path = %req.path, allowed, "network check");
    Json(CheckResponse { allowed, reason })
}

// ---------------------------------------------------------------------------
// POST /check/tool — "can I use this tool?"
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ToolCheck {
    tool_name: String,
}

async fn check_tool(State(state): State<Arc<AppState>>, Json(req): Json<ToolCheck>) -> Json<CheckResponse> {
    let policy = state.policy.load();
    let allowed = policy.tools.can_use(&req.tool_name);

    let reason = if allowed {
        "tool in allowlist".to_string()
    } else if policy.tools.deny.contains(&req.tool_name) {
        format!("{} is explicitly denied", req.tool_name)
    } else {
        format!("{} is not in the tool allowlist", req.tool_name)
    };

    tracing::debug!(tool = %req.tool_name, allowed, "tool check");
    Json(CheckResponse { allowed, reason })
}

// ---------------------------------------------------------------------------
// POST /check/bead — "can I access this bead?"
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BeadCheck {
    bead_id: String,
}

async fn check_bead(State(state): State<Arc<AppState>>, Json(req): Json<BeadCheck>) -> Json<CheckResponse> {
    let policy = state.policy.load();
    let allowed = policy.beads.can_access(&req.bead_id);

    let reason = if allowed {
        "bead in accessible list".to_string()
    } else {
        format!("{} is not accessible to this operator", req.bead_id)
    };

    tracing::debug!(bead = %req.bead_id, allowed, "bead check");
    Json(CheckResponse { allowed, reason })
}

// ---------------------------------------------------------------------------
// POST /check/cli — "can I run this command?"
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CliCheck {
    command: String,
}

async fn check_cli(State(state): State<Arc<AppState>>, Json(req): Json<CliCheck>) -> Json<CheckResponse> {
    let policy = state.policy.load();

    // CLI commands are checked against the filesystem and tools policies
    let dangerous = is_dangerous_cli(&req.command);
    let writable = policy.filesystem.writable;

    let (allowed, reason) = if dangerous && !writable {
        (false, format!("command '{}' modifies files but filesystem is read-only", req.command))
    } else if dangerous {
        (true, "command allowed (filesystem is writable)".into())
    } else {
        (true, "command allowed".into())
    };

    tracing::debug!(command = %req.command, allowed, "cli check");
    Json(CheckResponse { allowed, reason })
}

fn is_dangerous_cli(command: &str) -> bool {
    let dangerous_prefixes = ["rm ", "chmod ", "chown ", "git push", "git reset", "mv ", "cp "];
    let cmd_lower = command.to_lowercase();
    dangerous_prefixes.iter().any(|p| cmd_lower.starts_with(p))
}

// ---------------------------------------------------------------------------
// POST /check/mcp — "can I connect to this MCP server?"
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct McpCheck {
    server_name: String,
}

async fn check_mcp(State(state): State<Arc<AppState>>, Json(req): Json<McpCheck>) -> Json<CheckResponse> {
    let policy = state.policy.load();
    let allowed = policy.mcp.can_connect(&req.server_name);

    let reason = if allowed {
        "MCP server allowed".to_string()
    } else {
        format!("MCP server '{}' is not in the allowlist", req.server_name)
    };

    tracing::debug!(server = %req.server_name, allowed, "mcp check");
    Json(CheckResponse { allowed, reason })
}

// ---------------------------------------------------------------------------
// POST /check/port — "can I forward this port to the host?"
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PortCheck {
    guest_port: u16,
}

async fn check_port(State(state): State<Arc<AppState>>, Json(req): Json<PortCheck>) -> Json<CheckResponse> {
    let policy = state.policy.load();
    let allowed = policy.ports.can_forward(req.guest_port);

    let reason = if !policy.ports.enabled {
        "port forwarding is disabled for this task".to_string()
    } else if allowed {
        format!("port {} is within the allowed range", req.guest_port)
    } else if policy.ports.deny.contains(&req.guest_port) {
        format!("port {} is explicitly denied", req.guest_port)
    } else {
        format!(
            "port {} is outside the allowed range ({}-{})",
            req.guest_port, policy.ports.allow_range.0, policy.ports.allow_range.1
        )
    };

    tracing::debug!(port = req.guest_port, allowed, "port check");
    Json(CheckResponse { allowed, reason })
}

// ---------------------------------------------------------------------------
// POST /request — "I need access to X"
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AccessRequestInput {
    resource_type: String,
    resource: String,
    reason: String,
}

#[derive(Serialize, Deserialize)]
struct AccessRequestOutput {
    approved: bool,
    reason: String,
}

async fn request_access(State(state): State<Arc<AppState>>, Json(req): Json<AccessRequestInput>) -> (StatusCode, Json<AccessRequestOutput>) {
    let policy = state.policy.load();

    // Check if auto-approve applies
    let auto_approved = match req.resource_type.as_str() {
        "network" => policy.access_requests.should_auto_approve_domain(&req.resource),
        "tool" => policy.access_requests.should_auto_approve_tool(&req.resource),
        _ => false,
    };

    if auto_approved {
        tracing::info!(resource_type = %req.resource_type, resource = %req.resource, "auto-approved");
        return (
            StatusCode::OK,
            Json(AccessRequestOutput {
                approved: true,
                reason: "auto-approved by policy".into(),
            }),
        );
    }

    // Escalate to Big Smooth
    let access_req = AccessRequest {
        operator_id: policy.metadata.operator_id.clone(),
        bead_id: policy.metadata.bead_id.clone(),
        resource_type: req.resource_type,
        resource: req.resource,
        reason: req.reason,
    };

    match state.negotiator.request_access(&access_req, &policy.auth.token).await {
        Ok(resp) => (
            StatusCode::OK,
            Json(AccessRequestOutput {
                approved: resp.approved,
                reason: resp.reason,
            }),
        ),
        Err(e) => {
            tracing::warn!(error = %e, "negotiation with Big Smooth failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(AccessRequestOutput {
                    approved: false,
                    reason: format!("negotiation failed: {e}"),
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use http_body_util::BodyExt;
    use hyper::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::negotiate::Negotiator;
    use crate::policy::PolicyHolder;

    const TEST_POLICY: &str = r#"
[metadata]
operator_id = "test-op"
bead_id = "test-bead"
phase = "execute"

[auth]
token = "smth_op_test"
leader_url = "http://localhost:4400"

[network]
[[network.allow]]
domain = "openrouter.ai"

[[network.allow]]
domain = "api.github.com"
path = "/repos/SmooAI/*"

[beads]
accessible = ["test-bead", "dep-bead"]

[filesystem]
deny_patterns = ["*.env", "*.pem"]
writable = true

[tools]
allow = ["code_search", "beads_context"]
deny = ["workflow"]

[mcp]
allow_servers = ["smooth-tools"]
deny_unknown_servers = true

[access_requests]
enabled = true
auto_approve_domains = ["*.npmjs.org"]
auto_approve_tools = ["lint_fix"]
"#;

    fn test_state() -> Arc<AppState> {
        let policy = smooth_policy::Policy::from_toml(TEST_POLICY).expect("parse");
        let holder = PolicyHolder::from_policy(policy);
        let negotiator = Negotiator::new("http://localhost:4400", holder.clone());
        Arc::new(AppState { policy: holder, negotiator })
    }

    async fn do_post(app: &Router, path: &str, body: &str) -> (StatusCode, String) {
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");
        let resp = app.clone().oneshot(req).await.expect("response");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    #[tokio::test]
    async fn get_policy_returns_summary() {
        let app = build_router(test_state());
        let req = Request::builder().uri("/policy").body(Body::empty()).expect("req");
        let resp = app.oneshot(req).await.expect("resp");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let summary: PolicySummary = serde_json::from_slice(&bytes).expect("parse");
        assert_eq!(summary.operator_id, "test-op");
        assert!(summary.allowed_domains.contains(&"openrouter.ai".to_string()));
        assert!(summary.filesystem_writable);
    }

    #[tokio::test]
    async fn check_network_allowed() {
        let app = build_router(test_state());
        let (status, body) = do_post(&app, "/check/network", r#"{"domain":"openrouter.ai","path":"/zen","method":"POST"}"#).await;
        assert_eq!(status, StatusCode::OK);
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(resp.allowed);
    }

    #[tokio::test]
    async fn check_network_blocked() {
        let app = build_router(test_state());
        let (status, body) = do_post(&app, "/check/network", r#"{"domain":"evil.com"}"#).await;
        assert_eq!(status, StatusCode::OK);
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(!resp.allowed);
        assert!(resp.reason.contains("not in the network allowlist"));
    }

    #[tokio::test]
    async fn check_network_path_restricted() {
        let app = build_router(test_state());
        // Allowed path
        let (_, body) = do_post(&app, "/check/network", r#"{"domain":"api.github.com","path":"/repos/SmooAI/smooth"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(resp.allowed);

        // Blocked path
        let (_, body) = do_post(&app, "/check/network", r#"{"domain":"api.github.com","path":"/users/evil"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(!resp.allowed);
    }

    #[tokio::test]
    async fn check_tool_allowed() {
        let app = build_router(test_state());
        let (_, body) = do_post(&app, "/check/tool", r#"{"tool_name":"code_search"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(resp.allowed);
    }

    #[tokio::test]
    async fn check_tool_denied() {
        let app = build_router(test_state());
        let (_, body) = do_post(&app, "/check/tool", r#"{"tool_name":"workflow"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(!resp.allowed);
        assert!(resp.reason.contains("explicitly denied"));
    }

    #[tokio::test]
    async fn check_tool_not_in_allowlist() {
        let app = build_router(test_state());
        let (_, body) = do_post(&app, "/check/tool", r#"{"tool_name":"unknown_tool"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(!resp.allowed);
        assert!(resp.reason.contains("not in the tool allowlist"));
    }

    #[tokio::test]
    async fn check_bead_allowed() {
        let app = build_router(test_state());
        let (_, body) = do_post(&app, "/check/bead", r#"{"bead_id":"test-bead"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(resp.allowed);
    }

    #[tokio::test]
    async fn check_bead_denied() {
        let app = build_router(test_state());
        let (_, body) = do_post(&app, "/check/bead", r#"{"bead_id":"secret-bead"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(!resp.allowed);
    }

    #[tokio::test]
    async fn check_cli_dangerous_readonly() {
        // Create a read-only policy
        let policy_str = TEST_POLICY.replace("writable = true", "writable = false");
        let policy = smooth_policy::Policy::from_toml(&policy_str).expect("parse");
        let holder = PolicyHolder::from_policy(policy);
        let negotiator = Negotiator::new("http://localhost:4400", holder.clone());
        let state = Arc::new(AppState { policy: holder, negotiator });
        let app = build_router(state);

        let (_, body) = do_post(&app, "/check/cli", r#"{"command":"rm -rf /workspace/src"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(!resp.allowed);
        assert!(resp.reason.contains("read-only"));
    }

    #[tokio::test]
    async fn check_cli_safe_command() {
        let app = build_router(test_state());
        let (_, body) = do_post(&app, "/check/cli", r#"{"command":"ls -la"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(resp.allowed);
    }

    #[tokio::test]
    async fn check_mcp_allowed() {
        let app = build_router(test_state());
        let (_, body) = do_post(&app, "/check/mcp", r#"{"server_name":"smooth-tools"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(resp.allowed);
    }

    #[tokio::test]
    async fn check_mcp_denied() {
        let app = build_router(test_state());
        let (_, body) = do_post(&app, "/check/mcp", r#"{"server_name":"evil-server"}"#).await;
        let resp: CheckResponse = serde_json::from_str(&body).expect("parse");
        assert!(!resp.allowed);
    }

    #[tokio::test]
    async fn request_auto_approve_domain() {
        let app = build_router(test_state());
        let (status, body) = do_post(
            &app,
            "/request",
            r#"{"resource_type":"network","resource":"registry.npmjs.org","reason":"need npm"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let resp: AccessRequestOutput = serde_json::from_str(&body).expect("parse");
        assert!(resp.approved);
        assert!(resp.reason.contains("auto-approved"));
    }

    #[tokio::test]
    async fn request_auto_approve_tool() {
        let app = build_router(test_state());
        let (status, body) = do_post(&app, "/request", r#"{"resource_type":"tool","resource":"lint_fix","reason":"need linting"}"#).await;
        assert_eq!(status, StatusCode::OK);
        let resp: AccessRequestOutput = serde_json::from_str(&body).expect("parse");
        assert!(resp.approved);
    }

    #[test]
    fn dangerous_cli_detection() {
        assert!(is_dangerous_cli("rm -rf /workspace"));
        assert!(is_dangerous_cli("chmod 777 file"));
        assert!(is_dangerous_cli("git push origin main"));
        assert!(is_dangerous_cli("git reset --hard"));
        assert!(!is_dangerous_cli("ls -la"));
        assert!(!is_dangerous_cli("cargo test"));
        assert!(!is_dangerous_cli("cat file.txt"));
        assert!(!is_dangerous_cli("git status"));
    }
}

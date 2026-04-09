use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use smooth_operator::tool::{ToolCall, ToolHook, ToolResult};

/// Response from a Wonk check endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResponse {
    pub allowed: bool,
    pub reason: String,
}

/// Request body for POST /check/tool.
#[derive(Debug, Serialize)]
struct ToolCheckRequest {
    tool_name: String,
}

/// Request body for POST /check/network.
#[derive(Debug, Serialize)]
struct NetworkCheckRequest {
    domain: String,
    method: String,
}

/// Request body for POST /check/cli.
#[derive(Debug, Serialize)]
struct CliCheckRequest {
    command: String,
}

/// A `ToolHook` that gates tool execution through the Wonk access control
/// authority. All checks happen *before* execution — `post_call` is a no-op.
pub struct WonkHook {
    wonk_url: String,
    client: reqwest::Client,
}

impl WonkHook {
    /// Create a new `WonkHook` pointing at the given Wonk HTTP server.
    /// Trailing slashes on `wonk_url` are normalised away.
    pub fn new(wonk_url: &str) -> Self {
        Self {
            wonk_url: wonk_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Return the normalised Wonk URL.
    pub fn wonk_url(&self) -> &str {
        &self.wonk_url
    }

    /// Post a check request and return an error if the action is denied.
    async fn check(&self, path: &str, body: serde_json::Value) -> anyhow::Result<()> {
        let url = format!("{}{path}", self.wonk_url);
        let resp = self.client.post(&url).json(&body).send().await?;
        let check: CheckResponse = resp.json().await?;
        if check.allowed {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Wonk denied: {}", check.reason))
        }
    }
}

#[async_trait]
impl ToolHook for WonkHook {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        let body = serde_json::to_value(ToolCheckRequest { tool_name: call.name.clone() })?;
        self.check("/check/tool", body).await
    }

    async fn post_call(&self, _call: &ToolCall, _result: &ToolResult) -> anyhow::Result<()> {
        // Wonk is pre-check only — nothing to do after execution.
        Ok(())
    }

    async fn pre_network(&self, url: &str, method: &str) -> anyhow::Result<()> {
        // Extract the domain from the URL; fall back to the raw string if parsing fails.
        let domain = reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_else(|| url.to_string());

        let body = serde_json::to_value(NetworkCheckRequest {
            domain,
            method: method.to_string(),
        })?;
        self.check("/check/network", body).await
    }

    async fn pre_shell(&self, command: &str) -> anyhow::Result<()> {
        let body = serde_json::to_value(CliCheckRequest { command: command.to_string() })?;
        self.check("/check/cli", body).await
    }

    async fn pre_write(&self, path: &str) -> anyhow::Result<()> {
        let body = serde_json::json!({ "path": path });
        self.check("/check/write", body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smooth_operator::tool::ToolCall;

    // -----------------------------------------------------------------------
    // 1. WonkHook creation with URL normalisation
    // -----------------------------------------------------------------------
    #[test]
    fn wonk_hook_normalises_trailing_slash() {
        let hook = WonkHook::new("http://localhost:8400/");
        assert_eq!(hook.wonk_url(), "http://localhost:8400");
    }

    #[test]
    fn wonk_hook_preserves_clean_url() {
        let hook = WonkHook::new("http://localhost:8400");
        assert_eq!(hook.wonk_url(), "http://localhost:8400");
    }

    // -----------------------------------------------------------------------
    // 2. pre_call serialises the correct request body
    // -----------------------------------------------------------------------
    #[test]
    fn tool_check_request_serialises() {
        let req = ToolCheckRequest {
            tool_name: "code_search".into(),
        };
        let json = serde_json::to_value(&req).expect("serialize");
        assert_eq!(json["tool_name"], "code_search");
    }

    #[test]
    fn network_check_request_serialises() {
        let req = NetworkCheckRequest {
            domain: "api.github.com".into(),
            method: "GET".into(),
        };
        let json = serde_json::to_value(&req).expect("serialize");
        assert_eq!(json["domain"], "api.github.com");
        assert_eq!(json["method"], "GET");
    }

    #[test]
    fn cli_check_request_serialises() {
        let req = CliCheckRequest { command: "ls -la".into() };
        let json = serde_json::to_value(&req).expect("serialize");
        assert_eq!(json["command"], "ls -la");
    }

    // -----------------------------------------------------------------------
    // 3. Denied response returns Err with reason
    // -----------------------------------------------------------------------
    #[test]
    fn denied_check_response_parses() {
        let json = r#"{"allowed": false, "reason": "tool not in allowlist"}"#;
        let resp: CheckResponse = serde_json::from_str(json).expect("parse");
        assert!(!resp.allowed);
        assert_eq!(resp.reason, "tool not in allowlist");
    }

    // -----------------------------------------------------------------------
    // 4. Allowed response returns Ok
    // -----------------------------------------------------------------------
    #[test]
    fn allowed_check_response_parses() {
        let json = r#"{"allowed": true, "reason": "tool in allowlist"}"#;
        let resp: CheckResponse = serde_json::from_str(json).expect("parse");
        assert!(resp.allowed);
    }

    // -----------------------------------------------------------------------
    // Integration-style: round-trip through a mock Wonk server
    // -----------------------------------------------------------------------
    use axum::routing::post;
    use axum::{Json, Router};

    async fn mock_check_tool(Json(body): Json<serde_json::Value>) -> Json<CheckResponse> {
        let tool = body["tool_name"].as_str().unwrap_or("");
        if tool == "code_search" {
            Json(CheckResponse {
                allowed: true,
                reason: "tool in allowlist".into(),
            })
        } else {
            Json(CheckResponse {
                allowed: false,
                reason: format!("{tool} is not in the tool allowlist"),
            })
        }
    }

    async fn mock_check_network(Json(body): Json<serde_json::Value>) -> Json<CheckResponse> {
        let domain = body["domain"].as_str().unwrap_or("");
        if domain == "openrouter.ai" {
            Json(CheckResponse {
                allowed: true,
                reason: "domain in allowlist".into(),
            })
        } else {
            Json(CheckResponse {
                allowed: false,
                reason: format!("{domain} is not in the network allowlist"),
            })
        }
    }

    async fn mock_check_cli(Json(body): Json<serde_json::Value>) -> Json<CheckResponse> {
        let cmd = body["command"].as_str().unwrap_or("");
        if cmd.starts_with("ls") {
            Json(CheckResponse {
                allowed: true,
                reason: "command allowed".into(),
            })
        } else {
            Json(CheckResponse {
                allowed: false,
                reason: format!("command '{cmd}' denied"),
            })
        }
    }

    async fn mock_check_write(Json(body): Json<serde_json::Value>) -> Json<CheckResponse> {
        let path = body["path"].as_str().unwrap_or("");
        if path.ends_with(".env") || path.ends_with(".pem") {
            Json(CheckResponse {
                allowed: false,
                reason: format!("{path} matches a filesystem deny pattern"),
            })
        } else {
            Json(CheckResponse {
                allowed: true,
                reason: "path is allowed".into(),
            })
        }
    }

    fn mock_wonk_router() -> Router {
        Router::new()
            .route("/check/tool", post(mock_check_tool))
            .route("/check/write", post(mock_check_write))
            .route("/check/network", post(mock_check_network))
            .route("/check/cli", post(mock_check_cli))
    }

    /// Start a mock Wonk server on an ephemeral port and return its base URL.
    async fn start_mock_wonk() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let router = mock_wonk_router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        format!("http://{addr}")
    }

    fn make_tool_call(name: &str) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: name.into(),
            arguments: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn pre_call_allowed_tool() {
        let url = start_mock_wonk().await;
        let hook = WonkHook::new(&url);
        let call = make_tool_call("code_search");
        assert!(hook.pre_call(&call).await.is_ok());
    }

    #[tokio::test]
    async fn pre_call_denied_tool() {
        let url = start_mock_wonk().await;
        let hook = WonkHook::new(&url);
        let call = make_tool_call("workflow");
        let err = hook.pre_call(&call).await.unwrap_err();
        assert!(err.to_string().contains("Wonk denied"));
        assert!(err.to_string().contains("not in the tool allowlist"));
    }

    #[tokio::test]
    async fn pre_network_allowed() {
        let url = start_mock_wonk().await;
        let hook = WonkHook::new(&url);
        assert!(hook.pre_network("https://openrouter.ai/api", "GET").await.is_ok());
    }

    #[tokio::test]
    async fn pre_network_denied() {
        let url = start_mock_wonk().await;
        let hook = WonkHook::new(&url);
        let err = hook.pre_network("https://evil.com/steal", "POST").await.unwrap_err();
        assert!(err.to_string().contains("Wonk denied"));
    }

    #[tokio::test]
    async fn pre_shell_allowed() {
        let url = start_mock_wonk().await;
        let hook = WonkHook::new(&url);
        assert!(hook.pre_shell("ls -la").await.is_ok());
    }

    #[tokio::test]
    async fn pre_shell_denied() {
        let url = start_mock_wonk().await;
        let hook = WonkHook::new(&url);
        let err = hook.pre_shell("rm -rf /").await.unwrap_err();
        assert!(err.to_string().contains("Wonk denied"));
    }

    #[tokio::test]
    async fn pre_write_allowed_normal_file() {
        let url = start_mock_wonk().await;
        let hook = WonkHook::new(&url);
        assert!(hook.pre_write("/workspace/src/main.rs").await.is_ok());
    }

    #[tokio::test]
    async fn pre_write_denied_env_file() {
        let url = start_mock_wonk().await;
        let hook = WonkHook::new(&url);
        let err = hook.pre_write("/workspace/.env").await.unwrap_err();
        assert!(err.to_string().contains("Wonk denied"));
        assert!(err.to_string().contains("deny pattern"));
    }

    #[tokio::test]
    async fn pre_write_denied_pem_file() {
        let url = start_mock_wonk().await;
        let hook = WonkHook::new(&url);
        let err = hook.pre_write("/workspace/cert.pem").await.unwrap_err();
        assert!(err.to_string().contains("Wonk denied"));
    }

    #[tokio::test]
    async fn post_call_is_noop() {
        let hook = WonkHook::new("http://localhost:9999");
        let call = make_tool_call("anything");
        let result = smooth_operator::tool::ToolResult {
            tool_call_id: "call-1".into(),
            content: "ok".into(),
            is_error: false,
            details: None,
        };
        // post_call should always succeed (no-op)
        assert!(hook.post_call(&call, &result).await.is_ok());
    }
}

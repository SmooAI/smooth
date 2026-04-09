//! Remote delegation tool for sandboxed operators.
//!
//! When an operator runs inside a microVM, it cannot spawn sub-agents
//! in-process (no access to the LLM key or sandbox machinery). Instead
//! it calls Big Smooth's `/api/delegate` endpoint to create a sub-pearl,
//! then polls `/api/delegate/{id}/status` until the sub-task completes
//! (or times out).
//!
//! This module is only registered when `SMOOTH_API_URL` is set (sandboxed
//! mode). In host/in-process mode the existing `DelegationTool` from
//! `smooth-operator` handles delegation directly.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use smooth_operator::tool::{Tool, ToolSchema};

/// How often to poll the delegation status endpoint.
const POLL_INTERVAL_SECS: u64 = 10;

/// Maximum time to wait for a delegated task to complete.
const TIMEOUT_SECS: u64 = 600; // 10 minutes

/// Request body for `POST /api/delegate`.
#[derive(Serialize)]
struct DelegateRequest {
    parent_operator_id: String,
    task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

/// Response from `POST /api/delegate`.
#[derive(Deserialize)]
struct DelegateApiResponse {
    data: DelegateData,
    ok: bool,
}

#[derive(Deserialize)]
struct DelegateData {
    delegation_id: String,
    #[allow(dead_code)]
    status: String,
}

/// Response from `GET /api/delegate/{id}/status`.
#[derive(Deserialize)]
struct DelegateStatusApiResponse {
    data: DelegateStatusData,
    ok: bool,
}

#[derive(Deserialize)]
struct DelegateStatusData {
    #[allow(dead_code)]
    delegation_id: String,
    status: String,
    result: Option<String>,
}

/// A tool that delegates tasks to sibling operators via Big Smooth's HTTP API.
///
/// Only used in sandboxed mode — the runner creates this when `SMOOTH_API_URL`
/// is set. The tool POSTs a delegation request, then polls for completion.
pub struct RemoteDelegateTool {
    api_url: String,
    operator_id: String,
    client: reqwest::Client,
}

impl RemoteDelegateTool {
    /// Create a new remote delegation tool.
    ///
    /// `api_url` is Big Smooth's base URL (e.g. `http://host.containers.internal:4400`).
    /// `operator_id` is the current operator's ID (from `SMOOTH_OPERATOR_ID`).
    pub fn new(api_url: &str, operator_id: &str) -> Self {
        Self {
            api_url: api_url.trim_end_matches('/').to_string(),
            operator_id: operator_id.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Tool for RemoteDelegateTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delegate".into(),
            description: "Delegate a task to a sibling operator via Big Smooth. \
                The task will be dispatched as a new sub-pearl and worked on independently. \
                This tool blocks until the delegated task completes or times out (10 min)."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "The task to delegate to a sibling operator"
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional model override for the sub-operator"
                    }
                },
                "required": ["task"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'task' parameter"))?;
        let model = args.get("model").and_then(|v| v.as_str()).map(String::from);

        // 1. POST delegation request.
        let req_body = DelegateRequest {
            parent_operator_id: self.operator_id.clone(),
            task: task.to_string(),
            model,
        };

        let resp = self
            .client
            .post(format!("{}/api/delegate", self.api_url))
            .json(&req_body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("delegation request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("delegation request returned {status}: {body}"));
        }

        let api_resp: DelegateApiResponse = resp.json().await.map_err(|e| anyhow::anyhow!("failed to parse delegation response: {e}"))?;

        if !api_resp.ok {
            return Err(anyhow::anyhow!("delegation request was not ok"));
        }

        let delegation_id = api_resp.data.delegation_id;
        tracing::info!(delegation_id = %delegation_id, task = %task, "delegation dispatched, polling for completion");

        // 2. Poll for completion.
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(TIMEOUT_SECS);
        let interval = std::time::Duration::from_secs(POLL_INTERVAL_SECS);

        loop {
            if start.elapsed() > timeout {
                return Err(anyhow::anyhow!("delegation {delegation_id} timed out after {TIMEOUT_SECS}s"));
            }

            tokio::time::sleep(interval).await;

            let status_resp = self.client.get(format!("{}/api/delegate/{}/status", self.api_url, delegation_id)).send().await;

            let status_resp = match status_resp {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to poll delegation status, retrying");
                    continue;
                }
            };

            if !status_resp.status().is_success() {
                tracing::warn!(status = %status_resp.status(), "delegation status check returned error, retrying");
                continue;
            }

            let status: DelegateStatusApiResponse = match status_resp.json().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to parse delegation status, retrying");
                    continue;
                }
            };

            if !status.ok {
                continue;
            }

            match status.data.status.as_str() {
                "completed" => {
                    let result = status.data.result.unwrap_or_else(|| "(no result)".into());
                    tracing::info!(delegation_id = %delegation_id, "delegation completed");
                    return Ok(format!("Delegated task completed. Result:\n{result}"));
                }
                "failed" => {
                    return Err(anyhow::anyhow!("delegated task {delegation_id} failed"));
                }
                _ => {
                    // Still in progress, keep polling.
                    tracing::debug!(delegation_id = %delegation_id, status = %status.data.status, "delegation still in progress");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smooth_operator::tool::Tool;

    #[test]
    fn remote_delegate_tool_schema_has_task_parameter() {
        let tool = RemoteDelegateTool::new("http://localhost:4400", "op-test");
        let schema = tool.schema();

        assert_eq!(schema.name, "delegate");
        let params = &schema.parameters;
        assert!(params["properties"]["task"].is_object());
        assert_eq!(params["properties"]["task"]["type"], "string");
        assert!(params["properties"]["model"].is_object());
        let required = params["required"].as_array().expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("task")));
        // model is optional
        assert!(!required.iter().any(|v| v.as_str() == Some("model")));
    }

    #[test]
    fn remote_delegate_tool_stores_config() {
        let tool = RemoteDelegateTool::new("http://host.containers.internal:4400/", "op-abc");
        // Trailing slash should be trimmed.
        assert_eq!(tool.api_url, "http://host.containers.internal:4400");
        assert_eq!(tool.operator_id, "op-abc");
    }

    #[test]
    fn delegate_request_serializes() {
        let req = DelegateRequest {
            parent_operator_id: "op-1".into(),
            task: "Write tests".into(),
            model: Some("kimi-k2.5".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"parent_operator_id\":\"op-1\""));
        assert!(json.contains("\"task\":\"Write tests\""));
        assert!(json.contains("\"model\":\"kimi-k2.5\""));
    }

    #[test]
    fn delegate_request_skips_none_model() {
        let req = DelegateRequest {
            parent_operator_id: "op-2".into(),
            task: "Do something".into(),
            model: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("model"));
    }
}

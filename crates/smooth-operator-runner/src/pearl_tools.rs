//! HTTP-based pearl tools for operators.
//!
//! These tools call Big Smooth's pearl API so operators can create, list,
//! update, and close pearls during task execution. Requires
//! `SMOOTH_BIGSMOOTH_URL` to be set in the operator's environment.

use async_trait::async_trait;
use serde_json::json;
use smooth_operator::tool::{Tool, ToolSchema};

/// Base URL for Big Smooth's API (e.g., `http://192.168.1.50:4400`).
#[derive(Clone)]
pub struct PearlApiConfig {
    pub base_url: String,
    pub client: reqwest::Client,
}

impl PearlApiConfig {
    pub fn new(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self { base_url, client }
    }
}

// ── CreatePearlTool ─────────────────────────────────────────────────

pub struct CreatePearlApiTool {
    pub api: PearlApiConfig,
}

#[async_trait]
impl Tool for CreatePearlApiTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "create_pearl".to_string(),
            description: "Create a new pearl (work item) to track a sub-task. Returns the pearl ID.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["title", "description"],
                "properties": {
                    "title": { "type": "string", "description": "Short title for the pearl" },
                    "description": { "type": "string", "description": "What needs to be done and why" },
                    "priority": { "type": "number", "enum": [0,1,2,3,4], "description": "0=critical, 1=high, 2=medium (default), 3=low, 4=backlog" }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let title = arguments["title"].as_str().unwrap_or("Untitled");
        let description = arguments["description"].as_str().unwrap_or("");
        let priority = arguments["priority"].as_u64().unwrap_or(2);

        let resp = self
            .api
            .client
            .post(format!("{}/api/pearls", self.api.base_url))
            .json(&json!({
                "title": title,
                "description": description,
                "type": "task",
                "priority": priority,
            }))
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;
        if body["ok"].as_bool() == Some(true) {
            let id = body["data"]["id"].as_str().unwrap_or("unknown");
            Ok(format!("Created pearl {id}: {title}"))
        } else {
            Err(anyhow::anyhow!("create_pearl failed: {}", body))
        }
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── ListPearlsTool ──────────────────────────────────────────────────

pub struct ListPearlsApiTool {
    pub api: PearlApiConfig,
}

#[async_trait]
impl Tool for ListPearlsApiTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_pearls".to_string(),
            description: "List current pearls (work items). Shows ID, status, priority, and title.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["open","in_progress","closed"], "description": "Filter by status (optional)" }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let mut url = format!("{}/api/pearls", self.api.base_url);
        if let Some(status) = arguments.get("status").and_then(|v| v.as_str()) {
            url = format!("{url}?status={status}");
        }

        let resp = self.api.client.get(&url).send().await?;
        let body: serde_json::Value = resp.json().await?;

        if let Some(pearls) = body["data"].as_array() {
            if pearls.is_empty() {
                return Ok("No pearls found.".to_string());
            }
            let mut output = String::new();
            for p in pearls {
                let id = p["id"].as_str().unwrap_or("?");
                let status = p["status"].as_str().unwrap_or("?");
                let priority = p["priority"].as_u64().unwrap_or(2);
                let title = p["title"].as_str().unwrap_or("?");
                output.push_str(&format!("[{status}] {id} P{priority} {title}\n"));
            }
            Ok(output.trim_end().to_string())
        } else {
            Ok("Could not retrieve pearls.".to_string())
        }
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ── ClosePearlTool ──────────────────────────────────────────────────

pub struct ClosePearlApiTool {
    pub api: PearlApiConfig,
}

#[async_trait]
impl Tool for ClosePearlApiTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "close_pearl".to_string(),
            description: "Close a pearl (mark work item as done).".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "string", "description": "Pearl ID (e.g., th-abc123)" }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let id = arguments["id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;

        let resp = self.api.client.post(format!("{}/api/pearls/{id}/close", self.api.base_url)).send().await?;

        let body: serde_json::Value = resp.json().await?;
        if body["ok"].as_bool() == Some(true) {
            Ok(format!("Closed pearl {id}"))
        } else {
            Err(anyhow::anyhow!("close_pearl failed: {}", body))
        }
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── Registration ────────────────────────────────────────────────────

/// Register pearl tools if `SMOOTH_BIGSMOOTH_URL` is set.
pub fn register_pearl_tools(tools: &mut smooth_operator::ToolRegistry) {
    let base_url = match std::env::var("SMOOTH_BIGSMOOTH_URL") {
        Ok(url) => url,
        Err(_) => return, // No Big Smooth URL — skip pearl tools
    };

    let api = PearlApiConfig::new(base_url);
    tools.register(CreatePearlApiTool { api: api.clone() });
    tools.register(ListPearlsApiTool { api: api.clone() });
    tools.register(ClosePearlApiTool { api });
    tracing::info!("registered pearl API tools (create_pearl, list_pearls, close_pearl)");
}

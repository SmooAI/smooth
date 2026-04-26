//! Direct Dolt-backed pearl tools for operators.
//!
//! Operators work inside VMs with `/workspace` bind-mounted from the host.
//! If that workspace (or an ancestor) has `.smooth/dolt/`, the operator can
//! read/write pearls directly via the `smooth-dolt` binary at
//! `/opt/smooth/bin/smooth-dolt`. No HTTP calls to Big Smooth needed.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use smooth_operator::tool::{Tool, ToolSchema};

/// Shared pearl store handle for all pearl tools.
pub struct PearlStoreHandle {
    pub(crate) store: smooth_pearls::PearlStore,
}

// ── CreatePearlTool ─────────────────────────────────────────────────

pub struct CreatePearlTool {
    pub handle: Arc<PearlStoreHandle>,
}

#[async_trait]
impl Tool for CreatePearlTool {
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
        let priority_val = arguments["priority"].as_u64().unwrap_or(2) as u8;
        let priority = smooth_pearls::Priority::from_u8(priority_val).unwrap_or(smooth_pearls::Priority::Medium);

        let new = smooth_pearls::NewPearl {
            title: title.to_string(),
            description: description.to_string(),
            pearl_type: smooth_pearls::PearlType::Task,
            priority,
            assigned_to: None,
            parent_id: None,
            labels: Vec::new(),
        };

        let pearl = self.handle.store.create(&new)?;
        Ok(format!("Created pearl {}: {}", pearl.id, pearl.title))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── ListPearlsTool ──────────────────────────────────────────────────

pub struct ListPearlsTool {
    pub handle: Arc<PearlStoreHandle>,
}

#[async_trait]
impl Tool for ListPearlsTool {
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
        let mut query = smooth_pearls::PearlQuery::new();
        if let Some(status_str) = arguments.get("status").and_then(|v| v.as_str()) {
            if let Some(status) = smooth_pearls::PearlStatus::from_str_loose(status_str) {
                query = query.with_status(status);
            }
        }

        let pearls = self.handle.store.list(&query)?;
        if pearls.is_empty() {
            return Ok("No pearls found.".to_string());
        }

        let mut output = String::new();
        for p in &pearls {
            output.push_str(&format!("[{}] {} P{} {}\n", p.status.as_str(), p.id, p.priority.as_u8(), p.title));
        }
        Ok(output.trim_end().to_string())
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ── ClosePearlTool ──────────────────────────────────────────────────

pub struct ClosePearlTool {
    pub handle: Arc<PearlStoreHandle>,
}

#[async_trait]
impl Tool for ClosePearlTool {
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
        let count = self.handle.store.close(&[id])?;
        if count > 0 {
            Ok(format!("Closed pearl {id}"))
        } else {
            Ok(format!("Pearl {id} was already closed or not found"))
        }
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── Registration ────────────────────────────────────────────────────

/// Register pearl tools if a `.smooth/dolt/` directory exists in the
/// workspace ancestry. Uses the `smooth-dolt` binary directly — no HTTP.
///
/// Returns the shared `PearlStoreHandle` so other in-runner consumers (the
/// mailbox poller) can read pearl comments without opening a second store.
/// Returns `None` when no Dolt dir is found or the store fails to open.
pub fn register_pearl_tools(tools: &mut smooth_operator::ToolRegistry, workspace: &std::path::Path) -> Option<Arc<PearlStoreHandle>> {
    // Walk up from workspace looking for .smooth/dolt/
    let dolt_dir = smooth_pearls::dolt::find_repo_dolt_dir(workspace).or_else(|| {
        tracing::debug!("no .smooth/dolt/ found in workspace ancestry — pearl tools not registered");
        None
    })?;

    let store = match smooth_pearls::PearlStore::open(&dolt_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed to open pearl store at {} — pearl tools not registered", dolt_dir.display());
            return None;
        }
    };

    let handle = Arc::new(PearlStoreHandle { store });
    tools.register(CreatePearlTool { handle: handle.clone() });
    tools.register(ListPearlsTool { handle: handle.clone() });
    tools.register(ClosePearlTool { handle: handle.clone() });
    tracing::info!(dolt = %dolt_dir.display(), "registered pearl tools (create_pearl, list_pearls, close_pearl)");
    Some(handle)
}

//! Tool implementations for LLM agents to manage issues.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use smooth_operator::tool::{Tool, ToolSchema};

use crate::query::PearlQuery;
use crate::store::PearlStore;
use crate::types::{NewPearl, PearlStatus, PearlType, PearlUpdate, Priority};

// ── CreatePearlTool ─────────────────────────────────────────────────────────

/// Creates a new pearl in the tracker.
pub struct CreatePearlTool {
    pub store: Arc<PearlStore>,
}

#[async_trait]
impl Tool for CreatePearlTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "create_pearl".to_string(),
            description: "Create a new pearl in the tracker.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["title", "description", "type", "priority"],
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Short summary of the pearl"
                    },
                    "description": {
                        "type": "string",
                        "description": "Detailed description of what needs to be done and why"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["task", "bug", "feature"],
                        "description": "Pearl type"
                    },
                    "priority": {
                        "type": "number",
                        "enum": [0, 1, 2, 3, 4],
                        "description": "Priority level: 0=critical, 1=high, 2=medium, 3=low, 4=backlog"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let title = arguments["title"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'title'"))?;
        let description = arguments["description"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'description'"))?;
        let type_str = arguments["type"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'type'"))?;
        let priority_raw = arguments["priority"].as_u64().ok_or_else(|| anyhow::anyhow!("missing 'priority'"))?;
        let priority_num = u8::try_from(priority_raw).map_err(|_| anyhow::anyhow!("priority out of range: {priority_raw}"))?;

        let pearl_type = PearlType::from_str_loose(type_str).ok_or_else(|| anyhow::anyhow!("invalid pearl type: {type_str}"))?;
        let priority = Priority::from_u8(priority_num).ok_or_else(|| anyhow::anyhow!("invalid priority: {priority_num}"))?;

        let new_issue = NewPearl {
            title: title.to_string(),
            description: description.to_string(),
            pearl_type,
            priority,
            assigned_to: None,
            parent_id: None,
            labels: Vec::new(),
        };

        let pearl = self.store.create(&new_issue)?;
        Ok(format!("Created pearl {}: {}", pearl.id, pearl.title))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── ListPearlsTool ──────────────────────────────────────────────────────────

/// Lists issues with optional status filter.
pub struct ListPearlsTool {
    pub store: Arc<PearlStore>,
}

#[async_trait]
impl Tool for ListPearlsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_pearls".to_string(),
            description: "List issues, optionally filtered by status.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["open", "in_progress", "closed"],
                        "description": "Filter by status (optional)"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let mut query = PearlQuery::new();

        if let Some(status_str) = arguments.get("status").and_then(serde_json::Value::as_str) {
            let status = PearlStatus::from_str_loose(status_str).ok_or_else(|| anyhow::anyhow!("invalid status: {status_str}"))?;
            query = query.with_status(status);
        }

        let issues = self.store.list(&query)?;

        if issues.is_empty() {
            return Ok("No issues found.".to_string());
        }

        let mut output = String::new();
        for pearl in &issues {
            let _ = writeln!(output, "{} {} [{}] {}", pearl.status, pearl.id, pearl.priority, pearl.title);
        }
        Ok(output.trim_end().to_string())
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ── ShowPearlTool ───────────────────────────────────────────────────────────

/// Shows full details for a single pearl.
pub struct ShowPearlTool {
    pub store: Arc<PearlStore>,
}

#[async_trait]
impl Tool for ShowPearlTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "show_issue".to_string(),
            description: "Show full details of an pearl including description, dependencies, and comments.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Pearl ID (e.g., th-abc123)"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let id = arguments["id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;

        let pearl = self.store.get(id)?.ok_or_else(|| anyhow::anyhow!("pearl not found: {id}"))?;
        let deps = self.store.get_deps(id)?;
        let comments = self.store.get_comments(id)?;

        let mut output = String::new();
        let _ = writeln!(output, "{} {} [{}] {}", pearl.status, pearl.id, pearl.priority, pearl.title);
        let _ = writeln!(output, "Type: {}", pearl.pearl_type);
        let _ = writeln!(output, "Created: {}", pearl.created_at.format("%Y-%m-%d %H:%M"));

        if !pearl.description.is_empty() {
            let _ = writeln!(output, "\n{}", pearl.description);
        }

        if !pearl.labels.is_empty() {
            let _ = writeln!(output, "\nLabels: {}", pearl.labels.join(", "));
        }

        if !deps.is_empty() {
            output.push_str("\nDependencies:\n");
            for dep in &deps {
                let _ = writeln!(output, "  {} {} {}", dep.dep_type.as_str(), dep.depends_on, dep.pearl_id);
            }
        }

        if !comments.is_empty() {
            output.push_str("\nComments:\n");
            for comment in &comments {
                let _ = writeln!(output, "  [{}] {}", comment.created_at.format("%Y-%m-%d %H:%M"), comment.content);
            }
        }

        Ok(output.trim_end().to_string())
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ── UpdatePearlTool ─────────────────────────────────────────────────────────

/// Updates fields on an existing pearl.
pub struct UpdatePearlTool {
    pub store: Arc<PearlStore>,
}

#[async_trait]
impl Tool for UpdatePearlTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "update_pearl".to_string(),
            description: "Update an existing pearl's status, priority, or title.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Pearl ID (e.g., th-abc123)"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["open", "in_progress", "closed", "deferred"],
                        "description": "New status (optional)"
                    },
                    "priority": {
                        "type": "number",
                        "enum": [0, 1, 2, 3, 4],
                        "description": "New priority (optional)"
                    },
                    "title": {
                        "type": "string",
                        "description": "New title (optional)"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let id = arguments["id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;

        let mut updates = PearlUpdate::default();

        if let Some(status_str) = arguments.get("status").and_then(serde_json::Value::as_str) {
            updates.status = Some(PearlStatus::from_str_loose(status_str).ok_or_else(|| anyhow::anyhow!("invalid status: {status_str}"))?);
        }

        if let Some(priority_num) = arguments.get("priority").and_then(serde_json::Value::as_u64) {
            let p = u8::try_from(priority_num).map_err(|_| anyhow::anyhow!("priority out of range: {priority_num}"))?;
            updates.priority = Some(Priority::from_u8(p).ok_or_else(|| anyhow::anyhow!("invalid priority: {p}"))?);
        }

        if let Some(title) = arguments.get("title").and_then(serde_json::Value::as_str) {
            updates.title = Some(title.to_string());
        }

        self.store.update(id, &updates)?;
        Ok(format!("Updated pearl {id}"))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── ClosePearlTool ──────────────────────────────────────────────────────────

/// Closes one or more issues.
pub struct ClosePearlTool {
    pub store: Arc<PearlStore>,
}

#[async_trait]
impl Tool for ClosePearlTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "close_pearl".to_string(),
            description: "Close one or more issues.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["ids"],
                "properties": {
                    "ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of pearl IDs to close"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let ids_arr = arguments["ids"].as_array().ok_or_else(|| anyhow::anyhow!("missing 'ids' array"))?;
        let ids: Vec<String> = ids_arr.iter().filter_map(serde_json::Value::as_str).map(String::from).collect();
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();

        let count = self.store.close(&id_refs)?;
        Ok(format!("Closed {count} pearl(s)"))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── CommentPearlTool ────────────────────────────────────────────────────────

/// Adds a comment to an pearl.
pub struct CommentPearlTool {
    pub store: Arc<PearlStore>,
}

#[async_trait]
impl Tool for CommentPearlTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "comment_issue".to_string(),
            description: "Add a comment to an pearl.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id", "content"],
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Pearl ID (e.g., th-abc123)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Comment text"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let id = arguments["id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;
        let content = arguments["content"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'content'"))?;

        self.store.add_comment(id, content)?;
        Ok(format!("Added comment to {id}"))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── Registration helper ─────────────────────────────────────────────────────

/// Register all pearl tools with a `ToolRegistry`.
pub fn register_pearl_tools(registry: &mut smooth_operator::ToolRegistry, store: Arc<PearlStore>) {
    registry.register(CreatePearlTool { store: Arc::clone(&store) });
    registry.register(ListPearlsTool { store: Arc::clone(&store) });
    registry.register(ShowPearlTool { store: Arc::clone(&store) });
    registry.register(UpdatePearlTool { store: Arc::clone(&store) });
    registry.register(ClosePearlTool { store: Arc::clone(&store) });
    registry.register(CommentPearlTool { store });
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Option<Arc<PearlStore>> {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let dolt_dir = tmp.path().join("dolt");
        match PearlStore::init(&dolt_dir) {
            Ok(store) => {
                std::mem::forget(tmp);
                Some(Arc::new(store))
            }
            Err(_) => None, // smooth-dolt binary not available
        }
    }

    #[test]
    fn test_create_pearl_schema_has_correct_parameters() {
        let Some(store) = test_store() else { return };
        let tool = CreatePearlTool { store };
        let schema = tool.schema();

        assert_eq!(schema.name, "create_pearl");
        let params = &schema.parameters;
        let props = params["properties"].as_object().expect("properties should be an object");
        assert!(props.contains_key("title"));
        assert!(props.contains_key("description"));
        assert!(props.contains_key("type"));
        assert!(props.contains_key("priority"));

        let required = params["required"].as_array().expect("required should be an array");
        assert_eq!(required.len(), 4);
    }

    #[tokio::test]
    async fn test_create_pearl_execute_creates_issue() {
        let Some(store) = test_store() else { return };
        let tool = CreatePearlTool { store: Arc::clone(&store) };

        let args = json!({
            "title": "Fix login bug",
            "description": "Users cannot log in with SSO",
            "type": "bug",
            "priority": 1
        });

        let result = tool.execute(args).await.expect("execute should succeed");
        assert!(result.starts_with("Created pearl th-"));
        assert!(result.contains("Fix login bug"));

        // Verify pearl exists in the store
        let issues = store.list(&PearlQuery::new()).expect("list should succeed");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].title, "Fix login bug");
        assert_eq!(issues[0].pearl_type, PearlType::Bug);
        assert_eq!(issues[0].priority, Priority::High);
    }

    #[tokio::test]
    async fn test_list_pearls_returns_formatted_list() {
        let Some(store) = test_store() else { return };

        // Create a couple of issues
        store
            .create(&NewPearl {
                title: "First pearl".to_string(),
                description: String::new(),
                pearl_type: PearlType::Task,
                priority: Priority::Medium,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create first");

        store
            .create(&NewPearl {
                title: "Second pearl".to_string(),
                description: String::new(),
                pearl_type: PearlType::Bug,
                priority: Priority::High,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create second");

        let tool = ListPearlsTool { store };
        let result = tool.execute(json!({})).await.expect("execute should succeed");

        assert!(result.contains("First pearl"));
        assert!(result.contains("Second pearl"));
        // Should contain status icons
        assert!(result.contains("\u{25CB}")); // ○ for open
    }

    #[tokio::test]
    async fn test_update_pearl_changes_status() {
        let Some(store) = test_store() else { return };
        let pearl = store
            .create(&NewPearl {
                title: "Update me".to_string(),
                description: String::new(),
                pearl_type: PearlType::Task,
                priority: Priority::Medium,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create");

        let tool = UpdatePearlTool { store: Arc::clone(&store) };
        let result = tool
            .execute(json!({
                "id": pearl.id,
                "status": "in_progress"
            }))
            .await
            .expect("execute should succeed");

        assert!(result.contains(&pearl.id));
        let updated = store.get(&pearl.id).expect("get").expect("pearl exists");
        assert_eq!(updated.status, PearlStatus::InProgress);
    }

    #[tokio::test]
    async fn test_close_pearl_closes_issues() {
        let Some(store) = test_store() else { return };
        let a = store
            .create(&NewPearl {
                title: "Close me".to_string(),
                description: String::new(),
                pearl_type: PearlType::Task,
                priority: Priority::Medium,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create");

        let b = store
            .create(&NewPearl {
                title: "Close me too".to_string(),
                description: String::new(),
                pearl_type: PearlType::Task,
                priority: Priority::Low,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create");

        let tool = ClosePearlTool { store: Arc::clone(&store) };
        let result = tool.execute(json!({ "ids": [a.id, b.id] })).await.expect("execute should succeed");

        assert_eq!(result, "Closed 2 pearl(s)");

        let a_closed = store.get(&a.id).expect("get").expect("exists");
        assert_eq!(a_closed.status, PearlStatus::Closed);

        let b_closed = store.get(&b.id).expect("get").expect("exists");
        assert_eq!(b_closed.status, PearlStatus::Closed);
    }

    #[tokio::test]
    async fn test_comment_issue_adds_comment() {
        let Some(store) = test_store() else { return };
        let pearl = store
            .create(&NewPearl {
                title: "Comment target".to_string(),
                description: String::new(),
                pearl_type: PearlType::Task,
                priority: Priority::Medium,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create");

        let tool = CommentPearlTool { store: Arc::clone(&store) };
        let result = tool
            .execute(json!({
                "id": pearl.id,
                "content": "This is a test comment"
            }))
            .await
            .expect("execute should succeed");

        assert!(result.contains(&pearl.id));

        let comments = store.get_comments(&pearl.id).expect("get_comments");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].content, "This is a test comment");
    }
}

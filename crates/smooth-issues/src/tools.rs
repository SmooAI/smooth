//! Tool implementations for LLM agents to manage issues.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use smooth_operator::tool::{Tool, ToolSchema};

use crate::query::IssueQuery;
use crate::store::IssueStore;
use crate::types::{IssueStatus, IssueType, IssueUpdate, NewIssue, Priority};

// ── CreateIssueTool ─────────────────────────────────────────────────────────

/// Creates a new issue in the tracker.
pub struct CreateIssueTool {
    pub store: Arc<IssueStore>,
}

#[async_trait]
impl Tool for CreateIssueTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "create_issue".to_string(),
            description: "Create a new issue in the tracker.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["title", "description", "type", "priority"],
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Short summary of the issue"
                    },
                    "description": {
                        "type": "string",
                        "description": "Detailed description of what needs to be done and why"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["task", "bug", "feature"],
                        "description": "Issue type"
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

        let issue_type = IssueType::from_str_loose(type_str).ok_or_else(|| anyhow::anyhow!("invalid issue type: {type_str}"))?;
        let priority = Priority::from_u8(priority_num).ok_or_else(|| anyhow::anyhow!("invalid priority: {priority_num}"))?;

        let new_issue = NewIssue {
            title: title.to_string(),
            description: description.to_string(),
            issue_type,
            priority,
            assigned_to: None,
            parent_id: None,
            labels: Vec::new(),
        };

        let issue = self.store.create(&new_issue)?;
        Ok(format!("Created issue {}: {}", issue.id, issue.title))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── ListIssuesTool ──────────────────────────────────────────────────────────

/// Lists issues with optional status filter.
pub struct ListIssuesTool {
    pub store: Arc<IssueStore>,
}

#[async_trait]
impl Tool for ListIssuesTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_issues".to_string(),
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
        let mut query = IssueQuery::new();

        if let Some(status_str) = arguments.get("status").and_then(serde_json::Value::as_str) {
            let status = IssueStatus::from_str_loose(status_str).ok_or_else(|| anyhow::anyhow!("invalid status: {status_str}"))?;
            query = query.with_status(status);
        }

        let issues = self.store.list(&query)?;

        if issues.is_empty() {
            return Ok("No issues found.".to_string());
        }

        let mut output = String::new();
        for issue in &issues {
            let _ = writeln!(output, "{} {} [{}] {}", issue.status, issue.id, issue.priority, issue.title);
        }
        Ok(output.trim_end().to_string())
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ── ShowIssueTool ───────────────────────────────────────────────────────────

/// Shows full details for a single issue.
pub struct ShowIssueTool {
    pub store: Arc<IssueStore>,
}

#[async_trait]
impl Tool for ShowIssueTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "show_issue".to_string(),
            description: "Show full details of an issue including description, dependencies, and comments.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Issue ID (e.g., th-abc123)"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let id = arguments["id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;

        let issue = self.store.get(id)?.ok_or_else(|| anyhow::anyhow!("issue not found: {id}"))?;
        let deps = self.store.get_deps(id)?;
        let comments = self.store.get_comments(id)?;

        let mut output = String::new();
        let _ = writeln!(output, "{} {} [{}] {}", issue.status, issue.id, issue.priority, issue.title);
        let _ = writeln!(output, "Type: {}", issue.issue_type);
        let _ = writeln!(output, "Created: {}", issue.created_at.format("%Y-%m-%d %H:%M"));

        if !issue.description.is_empty() {
            let _ = writeln!(output, "\n{}", issue.description);
        }

        if !issue.labels.is_empty() {
            let _ = writeln!(output, "\nLabels: {}", issue.labels.join(", "));
        }

        if !deps.is_empty() {
            output.push_str("\nDependencies:\n");
            for dep in &deps {
                let _ = writeln!(output, "  {} {} {}", dep.dep_type.as_str(), dep.depends_on, dep.issue_id);
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

// ── UpdateIssueTool ─────────────────────────────────────────────────────────

/// Updates fields on an existing issue.
pub struct UpdateIssueTool {
    pub store: Arc<IssueStore>,
}

#[async_trait]
impl Tool for UpdateIssueTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "update_issue".to_string(),
            description: "Update an existing issue's status, priority, or title.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Issue ID (e.g., th-abc123)"
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

        let mut updates = IssueUpdate::default();

        if let Some(status_str) = arguments.get("status").and_then(serde_json::Value::as_str) {
            updates.status = Some(IssueStatus::from_str_loose(status_str).ok_or_else(|| anyhow::anyhow!("invalid status: {status_str}"))?);
        }

        if let Some(priority_num) = arguments.get("priority").and_then(serde_json::Value::as_u64) {
            let p = u8::try_from(priority_num).map_err(|_| anyhow::anyhow!("priority out of range: {priority_num}"))?;
            updates.priority = Some(Priority::from_u8(p).ok_or_else(|| anyhow::anyhow!("invalid priority: {p}"))?);
        }

        if let Some(title) = arguments.get("title").and_then(serde_json::Value::as_str) {
            updates.title = Some(title.to_string());
        }

        self.store.update(id, &updates)?;
        Ok(format!("Updated issue {id}"))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── CloseIssueTool ──────────────────────────────────────────────────────────

/// Closes one or more issues.
pub struct CloseIssueTool {
    pub store: Arc<IssueStore>,
}

#[async_trait]
impl Tool for CloseIssueTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "close_issue".to_string(),
            description: "Close one or more issues.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["ids"],
                "properties": {
                    "ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of issue IDs to close"
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
        Ok(format!("Closed {count} issue(s)"))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── CommentIssueTool ────────────────────────────────────────────────────────

/// Adds a comment to an issue.
pub struct CommentIssueTool {
    pub store: Arc<IssueStore>,
}

#[async_trait]
impl Tool for CommentIssueTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "comment_issue".to_string(),
            description: "Add a comment to an issue.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id", "content"],
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Issue ID (e.g., th-abc123)"
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

/// Register all issue tools with a `ToolRegistry`.
pub fn register_issue_tools(registry: &mut smooth_operator::ToolRegistry, store: Arc<IssueStore>) {
    registry.register(CreateIssueTool { store: Arc::clone(&store) });
    registry.register(ListIssuesTool { store: Arc::clone(&store) });
    registry.register(ShowIssueTool { store: Arc::clone(&store) });
    registry.register(UpdateIssueTool { store: Arc::clone(&store) });
    registry.register(CloseIssueTool { store: Arc::clone(&store) });
    registry.register(CommentIssueTool { store });
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Arc<IssueStore> {
        Arc::new(IssueStore::open_in_memory().expect("open in-memory store"))
    }

    #[test]
    fn test_create_issue_schema_has_correct_parameters() {
        let store = test_store();
        let tool = CreateIssueTool { store };
        let schema = tool.schema();

        assert_eq!(schema.name, "create_issue");
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
    async fn test_create_issue_execute_creates_issue() {
        let store = test_store();
        let tool = CreateIssueTool { store: Arc::clone(&store) };

        let args = json!({
            "title": "Fix login bug",
            "description": "Users cannot log in with SSO",
            "type": "bug",
            "priority": 1
        });

        let result = tool.execute(args).await.expect("execute should succeed");
        assert!(result.starts_with("Created issue th-"));
        assert!(result.contains("Fix login bug"));

        // Verify issue exists in the store
        let issues = store.list(&IssueQuery::new()).expect("list should succeed");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].title, "Fix login bug");
        assert_eq!(issues[0].issue_type, IssueType::Bug);
        assert_eq!(issues[0].priority, Priority::High);
    }

    #[tokio::test]
    async fn test_list_issues_returns_formatted_list() {
        let store = test_store();

        // Create a couple of issues
        store
            .create(&NewIssue {
                title: "First issue".to_string(),
                description: String::new(),
                issue_type: IssueType::Task,
                priority: Priority::Medium,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create first");

        store
            .create(&NewIssue {
                title: "Second issue".to_string(),
                description: String::new(),
                issue_type: IssueType::Bug,
                priority: Priority::High,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create second");

        let tool = ListIssuesTool { store };
        let result = tool.execute(json!({})).await.expect("execute should succeed");

        assert!(result.contains("First issue"));
        assert!(result.contains("Second issue"));
        // Should contain status icons
        assert!(result.contains("\u{25CB}")); // ○ for open
    }

    #[tokio::test]
    async fn test_update_issue_changes_status() {
        let store = test_store();
        let issue = store
            .create(&NewIssue {
                title: "Update me".to_string(),
                description: String::new(),
                issue_type: IssueType::Task,
                priority: Priority::Medium,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create");

        let tool = UpdateIssueTool { store: Arc::clone(&store) };
        let result = tool
            .execute(json!({
                "id": issue.id,
                "status": "in_progress"
            }))
            .await
            .expect("execute should succeed");

        assert!(result.contains(&issue.id));
        let updated = store.get(&issue.id).expect("get").expect("issue exists");
        assert_eq!(updated.status, IssueStatus::InProgress);
    }

    #[tokio::test]
    async fn test_close_issue_closes_issues() {
        let store = test_store();
        let a = store
            .create(&NewIssue {
                title: "Close me".to_string(),
                description: String::new(),
                issue_type: IssueType::Task,
                priority: Priority::Medium,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create");

        let b = store
            .create(&NewIssue {
                title: "Close me too".to_string(),
                description: String::new(),
                issue_type: IssueType::Task,
                priority: Priority::Low,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create");

        let tool = CloseIssueTool { store: Arc::clone(&store) };
        let result = tool.execute(json!({ "ids": [a.id, b.id] })).await.expect("execute should succeed");

        assert_eq!(result, "Closed 2 issue(s)");

        let a_closed = store.get(&a.id).expect("get").expect("exists");
        assert_eq!(a_closed.status, IssueStatus::Closed);

        let b_closed = store.get(&b.id).expect("get").expect("exists");
        assert_eq!(b_closed.status, IssueStatus::Closed);
    }

    #[tokio::test]
    async fn test_comment_issue_adds_comment() {
        let store = test_store();
        let issue = store
            .create(&NewIssue {
                title: "Comment target".to_string(),
                description: String::new(),
                issue_type: IssueType::Task,
                priority: Priority::Medium,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .expect("create");

        let tool = CommentIssueTool { store: Arc::clone(&store) };
        let result = tool
            .execute(json!({
                "id": issue.id,
                "content": "This is a test comment"
            }))
            .await
            .expect("execute should succeed");

        assert!(result.contains(&issue.id));

        let comments = store.get_comments(&issue.id).expect("get_comments");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].content, "This is a test comment");
    }
}

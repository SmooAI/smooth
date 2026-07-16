//! `knowledge_search` — a first-class named tool over `th knowledge search`.
//!
//! Retrieval over the org's OWN knowledge base (uploaded docs), as distinct from
//! [`web_search`](crate::WebSearchTool) over the open web. A dedicated typed tool
//! so the model reliably reaches for internal knowledge when a question is about
//! the org's own material. Shells `th knowledge search` — argv only, no shell.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

/// `knowledge_search` — semantic retrieval over the org's knowledge base.
pub struct KnowledgeSearchTool {
    /// Working directory for the `th` invocation.
    pub workspace: PathBuf,
}

#[async_trait]
impl Tool for KnowledgeSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "knowledge_search".into(),
            description: "Search the organization's OWN knowledge base (its uploaded documents) for relevant passages. Use this for questions about the org's internal material, policies, product docs, or anything it has ingested — NOT the open web (use web_search for that). Returns the most relevant document chunks."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What to look up in the knowledge base, e.g. \"refund policy for annual plans\"."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let query = require_query(&arguments)?;
        crate::th::run_th(&["knowledge".to_owned(), "search".to_owned(), query], &self.workspace).await
    }
}

fn require_query(arguments: &Value) -> anyhow::Result<String> {
    arguments
        .get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("missing required string parameter `query`"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn tool() -> KnowledgeSearchTool {
        KnowledgeSearchTool {
            workspace: std::env::temp_dir(),
        }
    }

    #[test]
    fn schema_names_knowledge_search_and_requires_query() {
        let s = tool().schema();
        assert_eq!(s.name, "knowledge_search");
        assert_eq!(s.parameters["required"][0], "query");
        assert!(tool().is_concurrent_safe());
    }

    #[test]
    fn require_query_extracts_and_rejects_blank() {
        assert_eq!(require_query(&json!({"query": "refunds"})).unwrap(), "refunds");
        assert!(require_query(&json!({"query": "  "})).is_err());
        assert!(require_query(&json!({})).is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_query() {
        assert!(tool().execute(json!({})).await.is_err());
    }
}

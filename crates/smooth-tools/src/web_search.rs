//! `web_search` — a first-class named tool over `th search`.
//!
//! The general [`ThTool`](crate::ThTool) can already web-search via
//! `["search", …]`, but LLMs select tools by NAME + schema first and prose
//! second — so the demo-critical capability gets under-used when it's buried in
//! an umbrella tool's description. A dedicated `web_search { query }` tool with a
//! typed parameter is picked reliably. It shells the same `th search` under the
//! hood (argv only, no shell), so nothing new can be reached — it's just
//! findable.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

/// `web_search` — current-information web search via `th search`.
pub struct WebSearchTool {
    /// Working directory for the `th` invocation.
    pub workspace: PathBuf,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search".into(),
            description: "Search the web for current, real-world information. Use this whenever the answer depends on facts you may not know or that can change — news, docs, prices, people, libraries, events. Returns ranked results (title, URL, snippet). Set `answer` to true to also get a synthesized direct answer. This hits the network; that is intended."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What to search the web for, e.g. \"latest tokio release notes\"."
                    },
                    "answer": {
                        "type": "boolean",
                        "description": "If true, synthesize a direct answer from the results (default: false — results only)."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        // Read-only lookup — safe to run alongside other tools.
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let args = build_args(&arguments)?;
        crate::th::run_th(&args, &self.workspace).await
    }
}

/// Build the `th` argv for a `web_search` call: `search <query> [--answer]`.
fn build_args(arguments: &Value) -> anyhow::Result<Vec<String>> {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required string parameter `query`"))?;
    let mut args = vec!["search".to_owned(), query.to_owned()];
    if arguments.get("answer").and_then(Value::as_bool) == Some(true) {
        args.push("--answer".to_owned());
    }
    Ok(args)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn tool() -> WebSearchTool {
        WebSearchTool {
            workspace: std::env::temp_dir(),
        }
    }

    #[test]
    fn schema_names_web_search_and_requires_query() {
        let s = tool().schema();
        assert_eq!(s.name, "web_search");
        assert_eq!(s.parameters["required"][0], "query");
        assert!(tool().is_concurrent_safe(), "web search is read-only");
    }

    #[test]
    fn build_args_maps_query_to_th_search() {
        let args = build_args(&json!({"query": "rust http client"})).unwrap();
        assert_eq!(args, vec!["search", "rust http client"]);
    }

    #[test]
    fn build_args_adds_answer_flag() {
        let args = build_args(&json!({"query": "who won", "answer": true})).unwrap();
        assert_eq!(args, vec!["search", "who won", "--answer"]);
    }

    #[test]
    fn build_args_omits_answer_when_false_or_absent() {
        assert_eq!(build_args(&json!({"query": "x", "answer": false})).unwrap(), vec!["search", "x"]);
        assert_eq!(build_args(&json!({"query": "x"})).unwrap(), vec!["search", "x"]);
    }

    #[test]
    fn build_args_rejects_missing_or_blank_query() {
        assert!(build_args(&json!({})).is_err());
        assert!(build_args(&json!({"query": "   "})).is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_query() {
        assert!(tool().execute(json!({})).await.is_err());
    }
}

//! `web_search` — a first-class named tool over a self-contained meta-search.
//!
//! The general [`ThTool`](crate::ThTool) can already web-search via
//! `["search", …]`, but LLMs select tools by NAME + schema first and prose
//! second — so the demo-critical capability gets under-used when it's buried in
//! an umbrella tool's description. A dedicated `web_search { query }` tool with a
//! typed parameter is picked reliably.
//!
//! **th-67180f**: prefer the Smoo cluster search (`th search`) first — it's the
//! good one (the same search the org's Smooth Operator uses, better ranking than
//! the keyless public APIs, and the only source that can synthesize an answer) —
//! and fall back to [`crate::search_native`] (public keyless search APIs called
//! in-process) only when the cluster is unavailable: `th` missing, not logged
//! in, or api.smoo.ai unreachable. So a logged-in daemon (smoo-hub) gets real
//! cluster results, and a bare machine still gets something useful.
//!
//! (Supersedes th-7031ba, which had inverted this to native-first and left the
//! cluster reachable only on `answer: true`.)

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::search_native;

/// How many ranked results a `web_search` call returns.
const MAX_RESULTS: usize = 8;

/// `web_search` — current-information web search: the Smoo cluster (`th search`)
/// first, public keyless APIs as a fallback.
pub struct WebSearchTool {
    /// Working directory for the `th search` (cluster) invocation.
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
        let query = query_of(&arguments)?;
        // Cluster first (th-67180f): the org's own search — better ranking, and
        // the only path that synthesizes an answer. `capture_th` returns Some
        // only on clean success, so a missing/logged-out/unreachable `th` just
        // yields None and we fall back to native, never a bare error.
        let mut args = vec!["search".to_owned(), query.to_owned()];
        if arguments.get("answer").and_then(Value::as_bool) == Some(true) {
            args.push("--answer".to_owned());
        }
        if let Some(out) = crate::th::capture_th(&args, &self.workspace).await {
            return Ok(out);
        }
        tracing::debug!("`th search` unavailable; falling back to native search");
        // Boxed: four concurrent HTTP futures make a fat state machine to hold
        // inline in every tool-call frame.
        let results = Box::pin(search_native::search(query, MAX_RESULTS)).await;
        Ok(search_native::render(query, &results))
    }
}

/// Extract the required, non-blank `query` parameter.
fn query_of(arguments: &Value) -> anyhow::Result<&str> {
    arguments
        .get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required string parameter `query`"))
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
    fn query_of_reads_the_query() {
        assert_eq!(query_of(&json!({"query": "rust http client"})).unwrap(), "rust http client");
    }

    #[test]
    fn query_of_rejects_missing_blank_or_non_string() {
        assert!(query_of(&json!({})).is_err());
        assert!(query_of(&json!({"query": "   "})).is_err());
        assert!(query_of(&json!({"query": 7})).is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_query() {
        assert!(tool().execute(json!({})).await.is_err());
    }
}

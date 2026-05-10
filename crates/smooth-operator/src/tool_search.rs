//! `tool_search` meta-tool — promotes deferred tools on demand
//! (pearl th-cfa1fb).
//!
//! Deferred tools live in [`ToolRegistry::register_deferred`] but
//! their schemas are hidden from the LLM. When the model needs one
//! it calls `tool_search("description …")`; this tool fuzzy-matches
//! the query against the deferred tools' names and descriptions,
//! promotes matches into the eager set, and returns each match's
//! schema as JSON. On the next iteration the agent recomputes its
//! tool list and the LLM can call the matched tool directly.
//!
//! Why this exists: as tool count grows past ~20-30 the LLM's
//! attention budget on the schema list dilutes — every turn pays
//! tokens to read tools it isn't going to use. The opencode/Claude
//! Code pattern keeps a small core eager set and defers the rest
//! behind a search step. This is Pillar D5 of the typed-sniffing-
//! badger plan.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tool::{Tool, ToolRegistry, ToolSchema};

/// `Arc`-shareable handle to a `ToolRegistry`. The meta-tool needs
/// a way to call `promote()` from within its `execute()` method —
/// since `execute()` takes `&self` and the registry's `promote` is
/// also `&self` (interior mut via `Arc<Mutex<…>>`), wrapping the
/// registry in an `Arc` and storing it in the meta-tool gives us
/// the call site without re-architecting the agent loop.
#[derive(Clone)]
pub struct ToolSearch {
    registry: Arc<ToolRegistry>,
}

impl ToolSearch {
    /// Construct the meta-tool with a handle to the registry whose
    /// deferred tools it will search and promote.
    #[must_use]
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for ToolSearch {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "tool_search".into(),
            description: "Search for additional tools by keyword. \
                Returns matching tool schemas as JSON; matched tools \
                become available on subsequent turns. Use when you \
                think a tool exists for a specific task but isn't in \
                your current tool list — e.g. tool_search(query=\"git\") \
                or tool_search(query=\"http request\")."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keyword to match against deferred tool names and descriptions. Case-insensitive substring match."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("tool_search: missing required `query` parameter"))?;
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(json!({
                "matched": 0,
                "tools": [],
                "note": "empty query — pass a keyword like \"git\" or \"network\""
            })
            .to_string());
        }

        let summary = self.registry.deferred_summary();
        let mut matched: Vec<(String, String)> = summary
            .into_iter()
            .filter(|(name, desc)| name.to_lowercase().contains(&needle) || desc.to_lowercase().contains(&needle))
            .collect();

        // Cap matches so a generic query like "tool" doesn't
        // promote the entire deferred set in one shot.
        const MAX_MATCHES: usize = 8;
        if matched.len() > MAX_MATCHES {
            matched.truncate(MAX_MATCHES);
        }

        for (name, _) in &matched {
            self.registry.promote(name);
        }

        // Return each match's full schema as the JSON payload.
        let tools: Vec<Value> = matched
            .iter()
            .filter_map(|(name, _)| {
                self.registry.tool_by_name(name).map(|t| {
                    let s = t.schema();
                    json!({
                        "name": s.name,
                        "description": s.description,
                        "parameters": s.parameters,
                    })
                })
            })
            .collect();

        Ok(json!({
            "matched": tools.len(),
            "tools": tools,
        })
        .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Tool, ToolRegistry, ToolSchema};

    struct FakeTool {
        name: &'static str,
        desc: &'static str,
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.into(),
                description: self.desc.into(),
                parameters: json!({"type": "object"}),
            }
        }
        async fn execute(&self, _args: Value) -> anyhow::Result<String> {
            Ok(format!("ran {}", self.name))
        }
    }

    fn make_registry_with_deferred() -> Arc<ToolRegistry> {
        let mut r = ToolRegistry::new();
        r.register_deferred(FakeTool {
            name: "git_status",
            desc: "Show git working tree status",
        });
        r.register_deferred(FakeTool {
            name: "git_diff",
            desc: "Show git diff between commits",
        });
        r.register_deferred(FakeTool {
            name: "http_get",
            desc: "Fetch a URL via HTTP GET",
        });
        Arc::new(r)
    }

    #[tokio::test]
    async fn tool_search_matches_by_name() {
        let registry = make_registry_with_deferred();
        let search = ToolSearch::new(registry.clone());
        let out = search.execute(json!({"query": "git"})).await.expect("execute");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["matched"], 2, "should match git_status + git_diff");
        // Both tools should now be promoted.
        assert!(registry.has_tool("git_status"));
        assert!(registry.has_tool("git_diff"));
        assert!(!registry.has_tool("http_get"), "http_get should remain deferred");
    }

    #[tokio::test]
    async fn tool_search_matches_by_description() {
        let registry = make_registry_with_deferred();
        let search = ToolSearch::new(registry.clone());
        let out = search.execute(json!({"query": "URL"})).await.expect("execute");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["matched"], 1);
        assert!(registry.has_tool("http_get"));
    }

    #[tokio::test]
    async fn tool_search_no_matches_returns_empty_list() {
        let registry = make_registry_with_deferred();
        let search = ToolSearch::new(registry);
        let out = search.execute(json!({"query": "xyzzy"})).await.expect("execute");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["matched"], 0);
        assert!(parsed["tools"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tool_search_rejects_missing_query() {
        let registry = make_registry_with_deferred();
        let search = ToolSearch::new(registry);
        let err = search.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("query"));
    }

    #[tokio::test]
    async fn promoted_tool_dispatches_through_registry() {
        let registry_arc = make_registry_with_deferred();
        let search = ToolSearch::new(registry_arc.clone());
        // Promote.
        let _ = search.execute(json!({"query": "git_status"})).await.unwrap();

        // After promotion the tool resolves via tool_by_name.
        let tool = registry_arc.tool_by_name("git_status").expect("promoted tool resolves");
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.contains("ran git_status"));
    }

    #[test]
    fn promotion_is_visible_across_clones() {
        let mut r = ToolRegistry::new();
        r.register_deferred(FakeTool { name: "alpha", desc: "first" });
        let cloned = r.clone();
        // Promote on the original — the clone shares the promoted set.
        assert!(r.promote("alpha"));
        assert!(cloned.has_tool("alpha"), "promotion must propagate across clones");
    }
}

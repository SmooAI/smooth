//! `remember` / `recall` — let the agent save and retrieve durable memories.
//!
//! `remember` writes to the daemon's `Memory` backend. Where the engine wires
//! `AgentConfig::with_memory`, it auto-recalls relevant entries ahead of a turn;
//! at the daemon's pinned engine rev there is **no server-side seam to inject
//! that backend** (the `LocalServerBuilder` / `StorageAdapter` / runner never
//! set `config.memory`), so `recall` is the explicit companion tool that lets
//! the agent pull memories back across sessions and restarts until that seam
//! lands. Both operate on the same shared backend, so a `remember` in one
//! session is `recall`-able in the next.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Memory, MemoryEntry, MemoryType, Tool, ToolSchema};

use crate::util::req_str;

/// `remember` tool — stores a memory in the agent's durable `Memory` backend.
pub struct RememberTool {
    /// The backend the entry is written to (shared with the engine's recall).
    pub memory: Arc<dyn Memory>,
}

/// Map the tool's `type` argument to a [`MemoryType`]; unknown → `LongTerm`.
fn parse_memory_type(s: &str) -> MemoryType {
    match s.trim().to_ascii_lowercase().as_str() {
        "user" => MemoryType::User,
        "feedback" => MemoryType::Feedback,
        "project" => MemoryType::Project,
        "reference" => MemoryType::Reference,
        "entity" => MemoryType::Entity,
        "short_term" | "shortterm" | "short" => MemoryType::ShortTerm,
        _ => MemoryType::LongTerm,
    }
}

#[async_trait]
impl Tool for RememberTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "remember".into(),
            description: "Save a durable memory that will be recalled automatically in future turns and sessions. \
                          Use for stable facts about the user, confirmed approach/feedback, current project state, or external references. \
                          Write the content as a self-contained sentence."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "The fact to remember, as a self-contained sentence." },
                    "type": {
                        "type": "string",
                        "enum": ["user", "feedback", "project", "reference", "entity", "long_term", "short_term"],
                        "description": "user=durable facts about the user; feedback=confirmed corrections/approach (highest leverage); project=current in-flight work (decays, verify before acting); reference=where info lives externally; long_term=other durable facts."
                    }
                },
                "required": ["content"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let content = req_str(&arguments, "content")?;
        let memory_type = arguments.get("type").and_then(Value::as_str).map_or(MemoryType::LongTerm, parse_memory_type);
        let entry = MemoryEntry::new(content.clone(), memory_type);
        let id = entry.id.clone();
        self.memory.store(entry)?;
        Ok(format!("Remembered ({memory_type:?}): {content} [id={id}]"))
    }
}

/// `recall` tool — retrieve durable memories relevant to a query.
///
/// Reads the same `Memory` backend the `remember` tool writes to — the read
/// half of cross-session memory. The engine's automatic recall isn't wired at
/// the daemon's pinned rev, so the agent calls this explicitly when a user
/// question might turn on something it was told to remember earlier.
pub struct RecallTool {
    /// The backend queried — shared with [`RememberTool`] so writes are visible.
    pub memory: Arc<dyn Memory>,
}

#[async_trait]
impl Tool for RecallTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "recall".into(),
            description: "Retrieve durable memories you saved earlier (with `remember`) that are relevant to a query. \
                          Call this when a user's request might depend on a stable fact, past decision, project state, or \
                          preference you were told before — especially at the start of a new session. Returns the most \
                          relevant saved memories."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What to look for, in a few keywords or a short phrase." },
                    "limit": { "type": "integer", "description": "Max memories to return (default 5).", "minimum": 1, "maximum": 25 }
                },
                "required": ["query"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        use std::fmt::Write as _;
        let query = req_str(&arguments, "query")?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped small positive limit")]
        let limit = arguments.get("limit").and_then(Value::as_u64).map_or(5, |n| n.clamp(1, 25) as usize);
        let hits = self.memory.recall(&query, limit)?;
        if hits.is_empty() {
            return Ok(format!("No saved memories match \"{query}\"."));
        }
        let mut out = format!("Recalled {} memory(ies) for \"{query}\":\n", hits.len());
        for m in &hits {
            let freshness = if m.memory_type.needs_freshness_check() {
                " (verify it's still current before acting)"
            } else {
                ""
            };
            let _ = writeln!(out, "- ({:?}, relevance={:.2}): {}{}", m.memory_type, m.relevance, m.content, freshness);
        }
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use smooth_operator::InMemoryMemory;

    #[test]
    fn parse_memory_type_maps_known_and_defaults() {
        assert_eq!(parse_memory_type("User"), MemoryType::User);
        assert_eq!(parse_memory_type("feedback"), MemoryType::Feedback);
        assert_eq!(parse_memory_type("PROJECT"), MemoryType::Project);
        assert_eq!(parse_memory_type("short"), MemoryType::ShortTerm);
        assert_eq!(parse_memory_type("whatever"), MemoryType::LongTerm);
    }

    #[tokio::test]
    async fn remember_stores_a_recallable_memory() {
        let memory: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        let tool = RememberTool { memory: Arc::clone(&memory) };
        let out = tool
            .execute(json!({"content": "the user deploys via GitHub Actions", "type": "project"}))
            .await
            .unwrap();
        assert!(out.contains("Remembered (Project)"), "{out}");

        // The stored memory is recallable through the same backend.
        let hits = memory.recall("github actions deploy", 5).unwrap();
        assert!(hits
            .iter()
            .any(|m| m.content.contains("GitHub Actions") && m.memory_type == MemoryType::Project));
    }

    #[tokio::test]
    async fn remember_requires_content() {
        let tool = RememberTool {
            memory: Arc::new(InMemoryMemory::new()),
        };
        assert!(tool.execute(json!({"type": "user"})).await.is_err(), "missing content is an error");
    }

    #[tokio::test]
    async fn remember_then_recall_round_trips_on_shared_backend() {
        let memory: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        let remember = RememberTool { memory: Arc::clone(&memory) };
        let recall = RecallTool { memory: Arc::clone(&memory) };
        remember
            .execute(json!({"content": "always add new shows to the smoo-hub watchlist", "type": "user"}))
            .await
            .unwrap();
        let out = recall.execute(json!({"query": "smoo-hub watchlist shows"})).await.unwrap();
        assert!(out.contains("Recalled 1 memory"), "{out}");
        assert!(out.contains("smoo-hub watchlist"), "{out}");
        assert!(out.contains("(User"), "renders the memory type: {out}");
    }

    #[tokio::test]
    async fn recall_reports_no_match() {
        let recall = RecallTool {
            memory: Arc::new(InMemoryMemory::new()),
        };
        let out = recall.execute(json!({"query": "anything"})).await.unwrap();
        assert!(out.contains("No saved memories match"), "{out}");
    }

    #[tokio::test]
    async fn recall_appends_freshness_nudge_for_time_sensitive_types() {
        let memory: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        RememberTool { memory: Arc::clone(&memory) }
            .execute(json!({"content": "the dashboard lives at grafana example", "type": "reference"}))
            .await
            .unwrap();
        let out = RecallTool { memory }.execute(json!({"query": "grafana dashboard"})).await.unwrap();
        assert!(out.contains("verify it's still current"), "reference memory nudges freshness: {out}");
    }

    #[tokio::test]
    async fn recall_requires_query() {
        let recall = RecallTool {
            memory: Arc::new(InMemoryMemory::new()),
        };
        assert!(recall.execute(json!({"limit": 5})).await.is_err(), "missing query is an error");
    }
}

//! `remember` — let the agent save a durable memory.
//!
//! Closes the loop with the engine's auto-recall: this tool writes to the
//! daemon's `Memory` backend, and the engine injects relevant entries ahead of
//! later user messages (in this and future sessions).

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
}

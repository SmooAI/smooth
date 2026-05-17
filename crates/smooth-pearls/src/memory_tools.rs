//! Agent-callable tools backed by `MemoryStore`.
//!
//! Pearl th-893801 Phase 3 iter-5b. Exposes three tools so
//! the agent can write learned context and read it back on a
//! later run. Registered separately from the pearl tools so
//! callers that don't want the persistence surface can skip
//! it.
//!
//! Tools:
//!
//! * `remember` — append a note (`content` + optional `source`).
//! * `recall_recent` — list the N most-recent notes, newest
//!   first.
//! * `recall_by_source` — same but filtered to a specific
//!   `source` tag (typically a pearl id).
//!
//! All three tools are intentionally narrow — the agent
//! decides when to use them. The system prompt is what
//! teaches it to call `recall_recent` at the start of a
//! task and `remember` at the end.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use smooth_operator::tool::{Tool, ToolSchema};

use crate::memory::MemoryStore;

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

/// Clamp an Option<u64> JSON arg into the valid limit range.
fn parse_limit(value: &serde_json::Value) -> usize {
    let raw = value.as_u64().unwrap_or(DEFAULT_LIMIT as u64);
    let raw = usize::try_from(raw).unwrap_or(DEFAULT_LIMIT);
    raw.clamp(1, MAX_LIMIT)
}

fn render_memory(out: &mut String, m: &crate::memory::Memory) {
    let _ = writeln!(
        out,
        "[{id}] {created} ({source})",
        id = m.id,
        created = m.created_at.format("%Y-%m-%d %H:%M:%S"),
        source = m.source
    );
    let _ = writeln!(out, "{content}", content = m.content);
    out.push('\n');
}

// ── RememberTool ────────────────────────────────────────────

/// Write a learned-context note. The agent calls this at end
/// of a task with anything it wants future runs to see.
pub struct RememberTool {
    pub store: Arc<MemoryStore>,
}

#[async_trait]
impl Tool for RememberTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "remember".to_string(),
            description: "Persist a short learned-context note. Call this at the end of a task to capture facts (codebase conventions, gotchas, paths, commands) that future runs on the same project should see. Keep notes concrete and short — one or two sentences.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["content"],
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "The note text. Free-form — facts, gotchas, commands, paths. Keep it short and concrete."
                    },
                    "source": {
                        "type": "string",
                        "description": "Optional origin tag — typically the pearl id (e.g. 'th-abc123') or operator id this note came from. Defaults to 'manual'."
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let content = arguments["content"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'content'"))?.to_string();
        let source = arguments["source"].as_str().unwrap_or("manual").to_string();
        let id = self.store.append(content, source)?;
        Ok(format!("remembered: {id}"))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── RecallRecentTool ────────────────────────────────────────

/// List the N most-recent notes.
pub struct RecallRecentTool {
    pub store: Arc<MemoryStore>,
}

#[async_trait]
impl Tool for RecallRecentTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "recall_recent".to_string(),
            description: "Read the N most-recent learned-context notes (newest first). Call this at the start of a task to prime context from prior runs on this project.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "number",
                        "description": "Max notes to return. Defaults to 20; clamped to [1, 100]."
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let limit = parse_limit(&arguments["limit"]);
        let memories = self.store.list_recent(limit)?;
        if memories.is_empty() {
            return Ok("no remembered notes yet".to_string());
        }
        let mut out = format!("{n} recent note(s):\n\n", n = memories.len());
        for m in &memories {
            render_memory(&mut out, m);
        }
        Ok(out)
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ── RecallBySourceTool ──────────────────────────────────────

/// List notes tagged with a specific source.
pub struct RecallBySourceTool {
    pub store: Arc<MemoryStore>,
}

#[async_trait]
impl Tool for RecallBySourceTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "recall_by_source".to_string(),
            description: "Read learned-context notes tagged with a specific source (typically a pearl id). Useful for picking up exactly where the agent left off on a particular work item.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["source"],
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Source tag to filter on (e.g. 'th-abc123' or 'operator-7')."
                    },
                    "limit": {
                        "type": "number",
                        "description": "Max notes to return. Defaults to 20; clamped to [1, 100]."
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let source = arguments["source"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'source'"))?;
        let limit = parse_limit(&arguments["limit"]);
        let memories = self.store.list_by_source(source, limit)?;
        if memories.is_empty() {
            return Ok(format!("no notes tagged '{source}'"));
        }
        let mut out = format!("{n} note(s) tagged '{source}':\n\n", n = memories.len());
        for m in &memories {
            render_memory(&mut out, m);
        }
        Ok(out)
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

/// Register the three memory tools on a tool registry. Same
/// shape as `register_pearl_tools` so callers can wire both
/// in one place.
pub fn register_memory_tools(registry: &mut smooth_operator::ToolRegistry, store: Arc<MemoryStore>) {
    registry.register(RememberTool { store: Arc::clone(&store) });
    registry.register(RecallRecentTool { store: Arc::clone(&store) });
    registry.register(RecallBySourceTool { store });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::store::PearlStore;
    use tempfile::TempDir;

    fn fresh_store() -> (TempDir, Arc<MemoryStore>) {
        let tmp = TempDir::new().unwrap();
        let pearl_store = PearlStore::init(&tmp.path().join(".smooth/dolt")).expect("init pearl store");
        (tmp, Arc::new(MemoryStore::new(pearl_store.dolt().clone())))
    }

    #[tokio::test]
    async fn remember_then_recall_round_trips() {
        let (_tmp, store) = fresh_store();
        let remember = RememberTool { store: Arc::clone(&store) };
        let recall = RecallRecentTool { store: Arc::clone(&store) };

        let resp = remember
            .execute(json!({
                "content": "this codebase uses cargo-zigbuild for cross-compile",
                "source": "th-abc123",
            }))
            .await
            .unwrap();
        assert!(resp.starts_with("remembered: mem-"));

        let dump = recall.execute(json!({"limit": 10})).await.unwrap();
        assert!(dump.contains("1 recent note"));
        assert!(dump.contains("cargo-zigbuild"));
        assert!(dump.contains("th-abc123"));
    }

    #[tokio::test]
    async fn recall_recent_with_no_notes_returns_friendly_message() {
        let (_tmp, store) = fresh_store();
        let recall = RecallRecentTool { store };
        let dump = recall.execute(json!({})).await.unwrap();
        assert_eq!(dump, "no remembered notes yet");
    }

    #[tokio::test]
    async fn recall_by_source_filters() {
        let (_tmp, store) = fresh_store();
        let remember = RememberTool { store: Arc::clone(&store) };
        let recall = RecallBySourceTool { store: Arc::clone(&store) };
        remember.execute(json!({"content": "alpha note", "source": "th-aaa"})).await.unwrap();
        remember.execute(json!({"content": "beta note", "source": "th-bbb"})).await.unwrap();

        let aaa = recall.execute(json!({"source": "th-aaa"})).await.unwrap();
        assert!(aaa.contains("alpha"));
        assert!(!aaa.contains("beta"));
        let nothing = recall.execute(json!({"source": "th-zzz"})).await.unwrap();
        assert!(nothing.contains("no notes tagged"));
    }

    #[tokio::test]
    async fn remember_defaults_source_to_manual() {
        let (_tmp, store) = fresh_store();
        let remember = RememberTool { store: Arc::clone(&store) };
        let recall = RecallBySourceTool { store: Arc::clone(&store) };
        remember.execute(json!({"content": "default source check"})).await.unwrap();
        let dump = recall.execute(json!({"source": "manual"})).await.unwrap();
        assert!(dump.contains("default source check"));
    }

    #[tokio::test]
    async fn missing_content_is_a_clean_error() {
        let (_tmp, store) = fresh_store();
        let remember = RememberTool { store };
        let err = remember.execute(json!({"source": "manual"})).await.unwrap_err();
        assert!(err.to_string().contains("missing 'content'"));
    }

    #[tokio::test]
    async fn recall_limit_is_clamped() {
        let (_tmp, store) = fresh_store();
        let remember = RememberTool { store: Arc::clone(&store) };
        // 5 notes is enough to exercise both clamps.
        for i in 0..5 {
            remember.execute(json!({"content": format!("n{i}")})).await.unwrap();
        }
        let recall = RecallRecentTool { store };
        // 0 is below the min — should clamp up to 1.
        let one = recall.execute(json!({"limit": 0})).await.unwrap();
        assert!(one.contains("1 recent note"));
        // 9999 is above the max — should clamp down but still
        // return all 5.
        let many = recall.execute(json!({"limit": 9999})).await.unwrap();
        assert!(many.contains("5 recent note"));
    }

    #[test]
    fn tool_schemas_advertise_read_only_correctly() {
        let store = Arc::new(MemoryStore::new(
            // Cheap to clone — use a temp dolt
            PearlStore::init(&TempDir::new().unwrap().path().join(".smooth/dolt")).unwrap().dolt().clone(),
        ));
        let remember = RememberTool { store: Arc::clone(&store) };
        let recall_recent = RecallRecentTool { store: Arc::clone(&store) };
        let recall_source = RecallBySourceTool { store };
        assert!(!remember.is_read_only(), "remember writes — must not be read-only");
        assert!(recall_recent.is_read_only(), "recall is read-only");
        assert!(recall_source.is_read_only(), "recall is read-only");
        assert_eq!(remember.schema().name, "remember");
        assert_eq!(recall_recent.schema().name, "recall_recent");
        assert_eq!(recall_source.schema().name, "recall_by_source");
    }
}

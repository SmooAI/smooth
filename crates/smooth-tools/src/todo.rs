//! `todo_write` — maintain a structured task list for multi-step work.
//!
//! The agent passes the FULL list on every call (stateless, like Claude Code's
//! TodoWrite — no per-turn merge). We normalise it and write a `todos`
//! directive onto the turn's directive sink, which every face renders as a live
//! checklist (protocol contract:
//! `{ "type": "todos", "items": [{ "text": string, "status": "pending"|"in_progress"|"completed" }] }`).
//!
//! Same sink mechanism as `send_file` / `present_plan` — the sink comes from
//! `ToolProviderContext.directive_sink`, captured fresh each turn.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

/// `todo_write` — replace the visible task list with `items`.
pub struct TodoWriteTool {
    sink: Arc<Mutex<Value>>,
}

impl TodoWriteTool {
    pub fn new(sink: Arc<Mutex<Value>>) -> Self {
        Self { sink }
    }
}

/// Normalise one raw item to `{ text, status }`, defaulting an absent/unknown
/// status to `pending`. Returns `None` when there is no usable text so a
/// malformed entry is dropped rather than rendered blank.
fn normalise_item(raw: &Value) -> Option<Value> {
    // Accept `text` or `content` (Claude-Code parity) as the label.
    let text = raw
        .get("text")
        .or_else(|| raw.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let status = match raw.get("status").and_then(Value::as_str) {
        Some("in_progress") => "in_progress",
        Some("completed" | "done") => "completed",
        _ => "pending",
    };
    Some(json!({ "text": text, "status": status }))
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "todo_write".into(),
            description: "Maintain a visible task list for multi-step work. Pass the COMPLETE list every time (it replaces the previous one) — mark items in_progress as you start them and completed as you finish. Keep exactly one item in_progress. Use it to plan and track any task with more than a couple of steps so the user can follow along.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "description": "The full ordered task list.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": { "type": "string", "description": "The task, imperative and short." },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed"], "description": "Task state." }
                            },
                            "required": ["text", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let items = arguments
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("`items` (an array) is required"))?;
        let normalised: Vec<Value> = items.iter().filter_map(normalise_item).collect();
        let done = normalised.iter().filter(|i| i["status"] == "completed").count();
        let total = normalised.len();
        {
            let mut guard = self.sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // Merge into an existing directive: a turn may emit send_file AND
            // todos. Only overwrite the todos slot — but the sink is a single
            // Value (last-write-wins per the protocol), so we just set it. If a
            // send_file directive is already present, todos supersede it for this
            // turn; faces that want both would need a richer sink — out of scope.
            *guard = json!({ "type": "todos", "items": normalised });
        }
        Ok(format!("Task list updated: {done}/{total} done."))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn sink() -> Arc<Mutex<Value>> {
        Arc::new(Mutex::new(Value::Null))
    }

    #[tokio::test]
    async fn writes_a_todos_directive() {
        let s = sink();
        let tool = TodoWriteTool::new(Arc::clone(&s));
        let out = tool
            .execute(json!({ "items": [
                { "text": "Read the code", "status": "completed" },
                { "text": "Make the change", "status": "in_progress" },
                { "text": "Test it", "status": "pending" }
            ] }))
            .await
            .unwrap();
        assert!(out.contains("1/3 done"), "result: {out}");
        let d = s.lock().unwrap().clone();
        assert_eq!(d["type"], "todos");
        assert_eq!(d["items"].as_array().unwrap().len(), 3);
        assert_eq!(d["items"][1]["status"], "in_progress");
    }

    #[tokio::test]
    async fn accepts_content_alias_and_defaults_bad_status() {
        let s = sink();
        let tool = TodoWriteTool::new(Arc::clone(&s));
        tool.execute(json!({ "items": [ { "content": "Do a thing", "status": "weird" } ] }))
            .await
            .unwrap();
        let d = s.lock().unwrap().clone();
        assert_eq!(d["items"][0]["text"], "Do a thing");
        assert_eq!(d["items"][0]["status"], "pending", "unknown status → pending");
    }

    #[tokio::test]
    async fn drops_blank_items_and_requires_the_array() {
        let s = sink();
        let tool = TodoWriteTool::new(Arc::clone(&s));
        // Blank/whitespace text is dropped, not rendered.
        tool.execute(json!({ "items": [ { "text": "  " }, { "text": "keep" } ] })).await.unwrap();
        assert_eq!(s.lock().unwrap()["items"].as_array().unwrap().len(), 1);
        // A missing array is an error.
        assert!(tool.execute(json!({})).await.is_err());
    }
}

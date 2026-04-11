use async_trait::async_trait;
use smooth_operator::tool::{ToolCall, ToolHook, ToolResult};

use crate::log_entry::{LogEntry, LogLevel};

/// A `ToolHook` that sends audit log entries to a Scribe server.
///
/// Logs both the start (`pre_call`) and completion (`post_call`) of every tool
/// invocation. Network, shell, and write hooks are no-ops — the Wonk hook
/// handles gating for those.
pub struct AuditHook {
    scribe_url: String,
    client: reqwest::Client,
    operator_id: String,
}

impl AuditHook {
    /// Create a new `AuditHook` that posts log entries to `scribe_url`.
    /// Trailing slashes on `scribe_url` are normalised away.
    pub fn new(scribe_url: &str, operator_id: &str) -> Self {
        Self {
            scribe_url: scribe_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            operator_id: operator_id.to_string(),
        }
    }

    /// Return the normalised Scribe URL.
    pub fn scribe_url(&self) -> &str {
        &self.scribe_url
    }

    /// Return the operator ID attached to every log entry.
    pub fn operator_id(&self) -> &str {
        &self.operator_id
    }

    /// Build a `LogEntry` for a tool event and send it to Scribe.
    async fn send_log(&self, entry: &LogEntry) -> anyhow::Result<()> {
        let url = format!("{}/log", self.scribe_url);
        self.client.post(&url).json(entry).send().await?;
        Ok(())
    }

    /// Build a log entry for a tool call start.
    pub fn build_pre_call_entry(&self, call: &ToolCall) -> LogEntry {
        let mut entry = LogEntry::new("smooth-operator", LogLevel::Info, format!("tool_call_start: {}", call.name)).with_operator(&self.operator_id);
        entry.fields.insert("tool_call_id".into(), call.id.clone());
        entry.fields.insert("tool_name".into(), call.name.clone());
        entry.fields.insert("arguments".into(), call.arguments.to_string());
        entry
    }

    /// Build a log entry for a tool call completion.
    pub fn build_post_call_entry(&self, call: &ToolCall, result: &ToolResult) -> LogEntry {
        let level = if result.is_error { LogLevel::Error } else { LogLevel::Info };
        let mut entry = LogEntry::new("smooth-operator", level, format!("tool_call_end: {}", call.name)).with_operator(&self.operator_id);
        entry.fields.insert("tool_call_id".into(), call.id.clone());
        entry.fields.insert("tool_name".into(), call.name.clone());
        entry.fields.insert("is_error".into(), result.is_error.to_string());
        entry.fields.insert("content_length".into(), result.content.len().to_string());
        entry
    }
}

#[async_trait]
impl ToolHook for AuditHook {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        let entry = self.build_pre_call_entry(call);
        // Best-effort logging — don't block tool execution if Scribe is down.
        if let Err(e) = self.send_log(&entry).await {
            tracing::warn!(error = %e, "failed to send pre_call audit log");
        }
        Ok(())
    }

    async fn post_call(&self, call: &ToolCall, result: &ToolResult) -> anyhow::Result<()> {
        let entry = self.build_post_call_entry(call, result);
        if let Err(e) = self.send_log(&entry).await {
            tracing::warn!(error = %e, "failed to send post_call audit log");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_call(name: &str) -> ToolCall {
        ToolCall {
            id: "call-42".into(),
            name: name.into(),
            arguments: serde_json::json!({"path": "/src/main.rs"}),
        }
    }

    fn make_result(is_error: bool) -> ToolResult {
        ToolResult {
            tool_call_id: "call-42".into(),
            content: "file contents here".into(),
            is_error,
            details: None,
        }
    }

    // -----------------------------------------------------------------------
    // 5. AuditHook creation with operator_id
    // -----------------------------------------------------------------------
    #[test]
    fn audit_hook_stores_operator_id() {
        let hook = AuditHook::new("http://localhost:8401", "op-123");
        assert_eq!(hook.operator_id(), "op-123");
        assert_eq!(hook.scribe_url(), "http://localhost:8401");
    }

    #[test]
    fn audit_hook_normalises_trailing_slash() {
        let hook = AuditHook::new("http://localhost:8401/", "op-1");
        assert_eq!(hook.scribe_url(), "http://localhost:8401");
    }

    // -----------------------------------------------------------------------
    // 6. pre_call creates LogEntry with correct fields
    // -----------------------------------------------------------------------
    #[test]
    fn pre_call_entry_has_correct_fields() {
        let hook = AuditHook::new("http://localhost:8401", "op-test");
        let call = make_call("read_file");
        let entry = hook.build_pre_call_entry(&call);

        assert_eq!(entry.service, "smooth-operator");
        assert_eq!(entry.level, LogLevel::Info);
        assert!(entry.message.contains("tool_call_start"));
        assert!(entry.message.contains("read_file"));
        assert_eq!(entry.operator_id.as_deref(), Some("op-test"));
        assert_eq!(entry.fields.get("tool_call_id").map(String::as_str), Some("call-42"));
        assert_eq!(entry.fields.get("tool_name").map(String::as_str), Some("read_file"));
        assert!(entry.fields.contains_key("arguments"));
    }

    // -----------------------------------------------------------------------
    // 7. post_call creates LogEntry with result info
    // -----------------------------------------------------------------------
    #[test]
    fn post_call_entry_success() {
        let hook = AuditHook::new("http://localhost:8401", "op-test");
        let call = make_call("read_file");
        let result = make_result(false);
        let entry = hook.build_post_call_entry(&call, &result);

        assert_eq!(entry.level, LogLevel::Info);
        assert!(entry.message.contains("tool_call_end"));
        assert_eq!(entry.fields.get("is_error").map(String::as_str), Some("false"));
        assert_eq!(
            entry.fields.get("content_length").map(String::as_str),
            Some("18") // "file contents here".len()
        );
    }

    #[test]
    fn post_call_entry_error() {
        let hook = AuditHook::new("http://localhost:8401", "op-test");
        let call = make_call("read_file");
        let result = make_result(true);
        let entry = hook.build_post_call_entry(&call, &result);

        assert_eq!(entry.level, LogLevel::Error);
        assert_eq!(entry.fields.get("is_error").map(String::as_str), Some("true"));
    }

    // -----------------------------------------------------------------------
    // 8. Log entry serialisation matches Scribe format
    // -----------------------------------------------------------------------
    #[test]
    fn log_entry_serialises_to_scribe_format() {
        let hook = AuditHook::new("http://localhost:8401", "op-test");
        let call = make_call("code_search");
        let entry = hook.build_pre_call_entry(&call);
        let json = serde_json::to_value(&entry).expect("serialize");

        // Scribe expects these top-level keys
        assert!(json.get("id").is_some());
        assert!(json.get("timestamp").is_some());
        assert_eq!(json["service"], "smooth-operator");
        assert_eq!(json["level"], "info");
        assert!(json["message"].as_str().unwrap_or("").contains("tool_call_start"));
        assert_eq!(json["operator_id"], "op-test");
        // fields is a map of string→string
        assert!(json["fields"].is_object());
        assert_eq!(json["fields"]["tool_name"], "code_search");
    }

    // -----------------------------------------------------------------------
    // Integration-style: round-trip through a mock Scribe server
    // -----------------------------------------------------------------------
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::{Arc, Mutex};

    async fn mock_post_log(axum::extract::State(logs): axum::extract::State<Arc<Mutex<Vec<LogEntry>>>>, Json(entry): Json<LogEntry>) -> StatusCode {
        logs.lock().expect("lock").push(entry);
        StatusCode::CREATED
    }

    async fn start_mock_scribe() -> (String, Arc<Mutex<Vec<LogEntry>>>) {
        let logs: Arc<Mutex<Vec<LogEntry>>> = Arc::new(Mutex::new(Vec::new()));
        let logs_clone = Arc::clone(&logs);
        let router = Router::new().route("/log", post(mock_post_log)).with_state(logs_clone);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        (format!("http://{addr}"), logs)
    }

    #[tokio::test]
    async fn pre_call_sends_log_to_scribe() {
        let (url, logs) = start_mock_scribe().await;
        let hook = AuditHook::new(&url, "op-integration");
        let call = make_call("code_search");
        hook.pre_call(&call).await.expect("pre_call");

        // Give the server a moment to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let entries = logs.lock().expect("lock");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].message.contains("tool_call_start"));
        assert_eq!(entries[0].operator_id.as_deref(), Some("op-integration"));
    }

    #[tokio::test]
    async fn post_call_sends_log_to_scribe() {
        let (url, logs) = start_mock_scribe().await;
        let hook = AuditHook::new(&url, "op-integration");
        let call = make_call("code_search");
        let result = make_result(false);
        hook.post_call(&call, &result).await.expect("post_call");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let entries = logs.lock().expect("lock");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].message.contains("tool_call_end"));
    }
}

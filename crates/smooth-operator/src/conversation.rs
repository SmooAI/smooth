use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Role of a message participant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<crate::tool::ToolCall>,
    pub timestamp: DateTime<Utc>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            timestamp: Utc::now(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            timestamp: Utc::now(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            timestamp: Utc::now(),
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: vec![],
            timestamp: Utc::now(),
        }
    }

    /// Estimate token count (rough: ~4 chars per token).
    pub fn estimated_tokens(&self) -> usize {
        self.content.len() / 4 + 1
    }
}

/// Strategy for compacting a conversation when it approaches the context limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompactionStrategy {
    /// Drop oldest non-system messages (current behavior, made explicit).
    SlidingWindow,
    /// Snip tool call/result pairs, keeping only tool names and error status.
    SnipToolResults { keep_recent: usize },
    /// Replace old messages with a summary message (requires LLM call).
    Summarize { keep_recent: usize },
    /// Multi-layer: snip tool results first, then summarize if still over.
    Layered { snip_keep: usize, summarize_keep: usize },
}

impl Default for CompactionStrategy {
    fn default() -> Self {
        Self::SnipToolResults { keep_recent: 10 }
    }
}

/// Tracks compaction attempts with a circuit breaker to avoid infinite retry loops.
///
/// When reactive compaction is triggered (e.g., by a "context too long" LLM error),
/// this struct records successes and failures. After `max_consecutive_failures` failures
/// in a row, the circuit "opens" and further compaction attempts should be skipped.
#[derive(Debug, Clone)]
pub struct ReactiveCompaction {
    consecutive_failures: u32,
    max_consecutive_failures: u32,
    total_compactions: u32,
    total_failures: u32,
}

impl ReactiveCompaction {
    /// Create a new tracker with a default threshold of 3 consecutive failures.
    pub fn new() -> Self {
        Self {
            consecutive_failures: 0,
            max_consecutive_failures: 3,
            total_compactions: 0,
            total_failures: 0,
        }
    }

    /// Record a successful compaction, resetting the consecutive failure counter.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.total_compactions += 1;
    }

    /// Record a failed compaction attempt.
    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.total_failures += 1;
    }

    /// Returns `true` if the circuit breaker is open (too many consecutive failures).
    pub fn is_circuit_open(&self) -> bool {
        self.consecutive_failures >= self.max_consecutive_failures
    }

    /// Return a snapshot of compaction statistics.
    pub fn stats(&self) -> CompactionStats {
        CompactionStats {
            total_compactions: self.total_compactions,
            total_failures: self.total_failures,
            consecutive_failures: self.consecutive_failures,
            circuit_open: self.is_circuit_open(),
        }
    }
}

impl Default for ReactiveCompaction {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot of compaction statistics from [`ReactiveCompaction`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionStats {
    pub total_compactions: u32,
    pub total_failures: u32,
    pub consecutive_failures: u32,
    pub circuit_open: bool,
}

/// Result of a compaction operation.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub messages_removed: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub summary_injected: bool,
}

/// A conversation is an ordered list of messages with context management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub messages: Vec<Message>,
    pub max_context_tokens: usize,
}

impl Conversation {
    pub fn new(max_context_tokens: usize) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            messages: vec![],
            max_context_tokens,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.messages.push(Message::system(prompt));
        self
    }

    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Get messages within the context window, always keeping the system prompt.
    pub fn context_window(&self) -> Vec<&Message> {
        let mut result = Vec::new();
        let mut total_tokens = 0;

        // Always include system messages first
        let system_msgs: Vec<&Message> = self.messages.iter().filter(|m| m.role == Role::System).collect();
        for msg in &system_msgs {
            total_tokens += msg.estimated_tokens();
            result.push(*msg);
        }

        // Add remaining messages from most recent, respecting token limit
        let non_system: Vec<&Message> = self.messages.iter().filter(|m| m.role != Role::System).collect();
        let mut recent = Vec::new();
        for msg in non_system.iter().rev() {
            let tokens = msg.estimated_tokens();
            if total_tokens + tokens > self.max_context_tokens {
                break;
            }
            total_tokens += tokens;
            recent.push(*msg);
        }
        recent.reverse();
        result.extend(recent);

        result
    }

    /// Total estimated tokens in the full conversation.
    pub fn total_tokens(&self) -> usize {
        self.messages.iter().map(Message::estimated_tokens).sum()
    }

    /// Number of messages.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Get the last assistant message content, if any.
    pub fn last_assistant_content(&self) -> Option<&str> {
        self.messages.iter().rev().find(|m| m.role == Role::Assistant).map(|m| m.content.as_str())
    }

    /// Check if conversation needs compaction (> 80% of `max_context_tokens`).
    pub fn needs_compaction(&self) -> bool {
        self.total_tokens() > self.max_context_tokens * 4 / 5
    }

    /// Compact the conversation using the given strategy.
    ///
    /// For `Summarize` and the summarize phase of `Layered`, pass a summary string
    /// (the caller is responsible for the LLM call). If no summary is provided and
    /// summarization is needed, the step is skipped.
    pub fn compact(&mut self, strategy: &CompactionStrategy, summary: Option<&str>) -> CompactionResult {
        let tokens_before = self.total_tokens();
        let messages_before = self.messages.len();

        match strategy {
            CompactionStrategy::SlidingWindow => {
                self.compact_sliding_window();
            }
            CompactionStrategy::SnipToolResults { keep_recent } => {
                self.compact_snip_tool_results(*keep_recent);
            }
            CompactionStrategy::Summarize { keep_recent } => {
                self.compact_summarize(*keep_recent, summary);
            }
            CompactionStrategy::Layered { snip_keep, summarize_keep } => {
                // First apply snip
                self.compact_snip_tool_results(*snip_keep);
                // If still over budget (60%), apply summarize
                if self.total_tokens() > self.max_context_tokens * 3 / 5 {
                    self.compact_summarize(*summarize_keep, summary);
                }
            }
        }

        let tokens_after = self.total_tokens();
        let messages_after = self.messages.len();
        let summary_injected = summary.is_some() && matches!(strategy, CompactionStrategy::Summarize { .. } | CompactionStrategy::Layered { .. });

        CompactionResult {
            messages_removed: messages_before.saturating_sub(messages_after),
            tokens_before,
            tokens_after,
            summary_injected,
        }
    }

    /// Drop oldest non-system messages until under 60% capacity.
    fn compact_sliding_window(&mut self) {
        let target = self.max_context_tokens * 3 / 5;
        while self.total_tokens() > target {
            // Find the first non-system message and remove it
            if let Some(idx) = self.messages.iter().position(|m| m.role != Role::System) {
                self.messages.remove(idx);
            } else {
                break; // only system messages left
            }
        }
    }

    /// Replace old tool call + tool result pairs with compact one-liners.
    /// Messages within `keep_recent` of the end are preserved.
    fn compact_snip_tool_results(&mut self, keep_recent: usize) {
        let len = self.messages.len();
        if len <= keep_recent {
            return;
        }
        let boundary = len - keep_recent;

        // Collect tool_call_ids from Tool-role messages in the snip zone
        let tool_result_ids: std::collections::HashSet<String> = self.messages[..boundary]
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();

        // Build replacement: for each assistant message with tool_calls in the zone,
        // replace it and its corresponding tool results with a compact summary.
        let mut new_messages: Vec<Message> = Vec::new();
        let mut consumed_tool_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (i, msg) in self.messages.iter().enumerate() {
            if i >= boundary {
                // Keep recent messages as-is
                new_messages.push(msg.clone());
                continue;
            }

            match msg.role {
                Role::Assistant if !msg.tool_calls.is_empty() => {
                    // Snip: replace tool calls with compact summaries
                    for tc in &msg.tool_calls {
                        if tool_result_ids.contains(&tc.id) {
                            // Find the corresponding tool result to check error status
                            let is_error = self.messages[..boundary]
                                .iter()
                                .any(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some(&tc.id) && m.content.to_lowercase().contains("error"));
                            let status = if is_error { "error" } else { "ok" };
                            new_messages.push(Message::system(format!("[tool: {}, result: {}]", tc.name, status)));
                            consumed_tool_ids.insert(tc.id.clone());
                        }
                    }
                    // If the assistant message also had content, keep it
                    if !msg.content.is_empty() {
                        let mut content_msg = Message::assistant(&msg.content);
                        content_msg.timestamp = msg.timestamp;
                        new_messages.push(content_msg);
                    }
                }
                Role::Tool if msg.tool_call_id.as_ref().is_some_and(|id| consumed_tool_ids.contains(id)) => {
                    // Already replaced by the snip above — skip
                }
                _ => {
                    new_messages.push(msg.clone());
                }
            }
        }

        self.messages = new_messages;
    }

    /// Replace messages older than `keep_recent` with a single summary message.
    fn compact_summarize(&mut self, keep_recent: usize, summary: Option<&str>) {
        let Some(summary_text) = summary else {
            return; // caller didn't provide a summary, skip
        };

        let len = self.messages.len();
        if len <= keep_recent {
            return;
        }
        let boundary = len - keep_recent;

        // Keep system messages + inject summary + keep recent
        let mut new_messages: Vec<Message> = Vec::new();

        // Preserve system messages from the old zone
        for msg in &self.messages[..boundary] {
            if msg.role == Role::System {
                new_messages.push(msg.clone());
            }
        }

        // Inject summary
        new_messages.push(Message::system(format!("[conversation summary]: {summary_text}")));

        // Keep recent messages
        for msg in &self.messages[boundary..] {
            new_messages.push(msg.clone());
        }

        self.messages = new_messages;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_constructors() {
        let sys = Message::system("You are helpful");
        assert_eq!(sys.role, Role::System);
        assert_eq!(sys.content, "You are helpful");

        let user = Message::user("Hello");
        assert_eq!(user.role, Role::User);

        let asst = Message::assistant("Hi there");
        assert_eq!(asst.role, Role::Assistant);

        let tool = Message::tool_result("call-123", "result data");
        assert_eq!(tool.role, Role::Tool);
        assert_eq!(tool.tool_call_id.as_deref(), Some("call-123"));
    }

    #[test]
    fn conversation_basics() {
        let mut conv = Conversation::new(100_000).with_system_prompt("Be helpful");
        assert_eq!(conv.len(), 1);
        assert!(!conv.is_empty());

        conv.push(Message::user("Hello"));
        conv.push(Message::assistant("Hi!"));
        assert_eq!(conv.len(), 3);
        assert_eq!(conv.last_assistant_content(), Some("Hi!"));
    }

    #[test]
    fn context_window_keeps_system() {
        let mut conv = Conversation::new(50).with_system_prompt("System");
        for i in 0..100 {
            conv.push(Message::user(format!("msg {i}")));
        }
        let window = conv.context_window();
        assert_eq!(window[0].role, Role::System);
        assert!(window.len() < conv.len()); // should trim
    }

    #[test]
    fn context_window_small_limit() {
        let mut conv = Conversation::new(10).with_system_prompt("S");
        conv.push(Message::user("A short message"));
        conv.push(Message::user("Another message"));
        let window = conv.context_window();
        assert!(!window.is_empty());
        // System always included
        assert_eq!(window[0].role, Role::System);
    }

    #[test]
    fn token_estimation() {
        let msg = Message::user("Hello world!"); // 12 chars → ~4 tokens
        assert!(msg.estimated_tokens() > 0);
    }

    #[test]
    fn message_serialization() {
        let msg = Message::user("Hello");
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains("\"role\":\"user\""));
        let parsed: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.role, Role::User);
        assert_eq!(parsed.content, "Hello");
    }

    #[test]
    fn conversation_serialization() {
        let conv = Conversation::new(100_000).with_system_prompt("Test");
        let json = serde_json::to_string(&conv).expect("serialize");
        let parsed: Conversation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn empty_conversation() {
        let conv = Conversation::new(100_000);
        assert!(conv.is_empty());
        assert_eq!(conv.total_tokens(), 0);
        assert_eq!(conv.last_assistant_content(), None);
    }

    // ── Compaction tests ────────────────────────────────────────────

    /// Helper: build an assistant message with tool_calls attached.
    fn assistant_with_tool_calls(content: &str, tool_calls: Vec<crate::tool::ToolCall>) -> Message {
        let mut msg = Message::assistant(content);
        msg.tool_calls = tool_calls;
        msg
    }

    #[test]
    fn sliding_window_drops_oldest_keeps_system() {
        // System prompt ~4 tokens, each user msg ~5 tokens. max=30 => 60% target=18
        let mut conv = Conversation::new(30).with_system_prompt("Sys");
        for i in 0..10 {
            conv.push(Message::user(format!("msg-{i:03}"))); // 7 chars => ~2 tokens each
        }
        let before_len = conv.len();
        let result = conv.compact(&CompactionStrategy::SlidingWindow, None);

        // System prompt must survive
        assert_eq!(conv.messages[0].role, Role::System);
        assert_eq!(conv.messages[0].content, "Sys");
        // Messages were removed
        assert!(conv.len() < before_len);
        assert!(result.messages_removed > 0);
        // Under 60% budget
        assert!(conv.total_tokens() <= 30 * 3 / 5);
    }

    #[test]
    fn snip_tool_results_replaces_pairs() {
        let mut conv = Conversation::new(100_000).with_system_prompt("Sys");
        conv.push(Message::user("do something"));
        // Assistant with a tool call
        conv.push(assistant_with_tool_calls(
            "",
            vec![crate::tool::ToolCall {
                id: "tc1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({}),
            }],
        ));
        // Tool result
        conv.push(Message::tool_result("tc1", "file contents here, lots of data"));
        // More recent messages
        conv.push(Message::user("thanks"));
        conv.push(Message::assistant("you're welcome"));

        let result = conv.compact(&CompactionStrategy::SnipToolResults { keep_recent: 2 }, None);

        // The tool call pair should be replaced with a one-liner
        let snipped: Vec<&Message> = conv.messages.iter().filter(|m| m.content.contains("[tool: read_file")).collect();
        assert_eq!(snipped.len(), 1);
        assert!(snipped[0].content.contains("result: ok"));
        assert!(result.messages_removed > 0);
        // Original verbose tool result should be gone
        assert!(!conv.messages.iter().any(|m| m.content.contains("file contents here")));
    }

    #[test]
    fn snip_tool_results_preserves_recent() {
        let mut conv = Conversation::new(100_000).with_system_prompt("Sys");
        // Old tool pair
        conv.push(assistant_with_tool_calls(
            "",
            vec![crate::tool::ToolCall {
                id: "tc-old".into(),
                name: "old_tool".into(),
                arguments: serde_json::json!({}),
            }],
        ));
        conv.push(Message::tool_result("tc-old", "old result"));
        // Recent tool pair (within keep_recent)
        conv.push(assistant_with_tool_calls(
            "",
            vec![crate::tool::ToolCall {
                id: "tc-new".into(),
                name: "new_tool".into(),
                arguments: serde_json::json!({}),
            }],
        ));
        conv.push(Message::tool_result("tc-new", "new result"));
        conv.push(Message::assistant("done"));

        conv.compact(&CompactionStrategy::SnipToolResults { keep_recent: 3 }, None);

        // Recent tool result should still be present verbatim
        assert!(conv.messages.iter().any(|m| m.content == "new result"));
        // Old tool result should be snipped
        assert!(!conv.messages.iter().any(|m| m.content == "old result"));
    }

    #[test]
    fn needs_compaction_true_at_80_percent() {
        // max=100, each msg ~2 tokens (4 chars + 1). Need >80 tokens.
        let mut conv = Conversation::new(100);
        // System prompt: "S" => 1 token. We need ~80 more tokens.
        conv = conv.with_system_prompt("S");
        // Each "XXXX" message is 4 chars => 2 tokens. We need 40 of them for 80 tokens.
        for _ in 0..45 {
            conv.push(Message::user("XXXX"));
        }
        assert!(conv.total_tokens() > 80, "total_tokens={} should be >80", conv.total_tokens());
        assert!(conv.needs_compaction());
    }

    #[test]
    fn needs_compaction_false_below_threshold() {
        let mut conv = Conversation::new(100_000).with_system_prompt("Sys");
        conv.push(Message::user("Hello"));
        assert!(!conv.needs_compaction());
    }

    #[test]
    fn compaction_result_token_counts() {
        let mut conv = Conversation::new(30).with_system_prompt("S");
        for i in 0..10 {
            conv.push(Message::user(format!("message-{i:04}")));
        }
        let result = conv.compact(&CompactionStrategy::SlidingWindow, None);
        assert!(result.tokens_before > result.tokens_after);
        assert!(result.tokens_before > 0);
        assert!(result.tokens_after > 0);
    }

    #[test]
    fn compaction_preserves_message_ordering() {
        let mut conv = Conversation::new(30).with_system_prompt("System");
        for i in 0..10 {
            conv.push(Message::user(format!("u{i}")));
            conv.push(Message::assistant(format!("a{i}")));
        }
        conv.compact(&CompactionStrategy::SlidingWindow, None);

        // First message must be the system prompt
        assert_eq!(conv.messages[0].role, Role::System);
        assert_eq!(conv.messages[0].content, "System");

        // No system messages after the first (except compaction-injected ones)
        // The remaining messages should be in chronological order
        let non_system: Vec<&Message> = conv.messages.iter().skip(1).collect();
        for w in non_system.windows(2) {
            assert!(w[0].timestamp <= w[1].timestamp, "messages out of order");
        }
    }

    // ── ReactiveCompaction tests ─────────────────────────────────────

    #[test]
    fn reactive_compaction_starts_with_zero_failures() {
        let rc = ReactiveCompaction::new();
        assert_eq!(rc.consecutive_failures, 0);
        assert_eq!(rc.total_compactions, 0);
        assert_eq!(rc.total_failures, 0);
        assert!(!rc.is_circuit_open());
    }

    #[test]
    fn record_success_resets_consecutive_counter() {
        let mut rc = ReactiveCompaction::new();
        rc.record_failure();
        rc.record_failure();
        assert_eq!(rc.consecutive_failures, 2);
        rc.record_success();
        assert_eq!(rc.consecutive_failures, 0);
        assert_eq!(rc.total_compactions, 1);
        // total_failures should still reflect the history
        assert_eq!(rc.total_failures, 2);
    }

    #[test]
    fn record_failure_increments_consecutive_counter() {
        let mut rc = ReactiveCompaction::new();
        rc.record_failure();
        assert_eq!(rc.consecutive_failures, 1);
        rc.record_failure();
        assert_eq!(rc.consecutive_failures, 2);
        assert_eq!(rc.total_failures, 2);
    }

    #[test]
    fn circuit_opens_after_max_consecutive_failures() {
        let mut rc = ReactiveCompaction::new();
        for _ in 0..3 {
            rc.record_failure();
        }
        assert!(rc.is_circuit_open());
    }

    #[test]
    fn circuit_stays_closed_below_threshold() {
        let mut rc = ReactiveCompaction::new();
        rc.record_failure();
        rc.record_failure();
        assert!(!rc.is_circuit_open());
    }

    #[test]
    fn stats_reports_correctly() {
        let mut rc = ReactiveCompaction::new();
        rc.record_success();
        rc.record_failure();
        rc.record_success();
        rc.record_failure();
        rc.record_failure();
        let stats = rc.stats();
        assert_eq!(stats.total_compactions, 2);
        assert_eq!(stats.total_failures, 3);
        assert_eq!(stats.consecutive_failures, 2);
        assert!(!stats.circuit_open);
    }

    #[test]
    fn compaction_stats_serialization() {
        let stats = CompactionStats {
            total_compactions: 5,
            total_failures: 2,
            consecutive_failures: 1,
            circuit_open: false,
        };
        let json = serde_json::to_string(&stats).expect("serialize");
        assert!(json.contains("\"total_compactions\":5"));
        assert!(json.contains("\"circuit_open\":false"));
        let parsed: CompactionStats = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, stats);
    }

    #[test]
    fn layered_applies_snip_then_checks_budget() {
        let mut conv = Conversation::new(100).with_system_prompt("S");
        // Add tool pairs that are large
        for i in 0..5 {
            let id = format!("tc{i}");
            conv.push(assistant_with_tool_calls(
                "",
                vec![crate::tool::ToolCall {
                    id: id.clone(),
                    name: format!("tool_{i}"),
                    arguments: serde_json::json!({}),
                }],
            ));
            // Big tool results to inflate token count
            conv.push(Message::tool_result(&id, &"x".repeat(40)));
        }
        conv.push(Message::user("final"));
        conv.push(Message::assistant("ok"));

        let tokens_before = conv.total_tokens();
        let result = conv.compact(
            &CompactionStrategy::Layered {
                snip_keep: 2,
                summarize_keep: 2,
            },
            None,
        );

        // Should have removed messages
        assert!(result.messages_removed > 0);
        // Tokens should be reduced
        assert!(result.tokens_after < tokens_before);
        // System prompt preserved
        assert_eq!(conv.messages[0].role, Role::System);
    }
}

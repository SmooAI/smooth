//! An `LlmProvider` seam over the LLM call so the agent loop and workflows can
//! be unit-tested deterministically, without a live model or network.
//!
//! The real [`LlmClient`](crate::llm::LlmClient) implements [`LlmProvider`] by
//! delegating to its inherent methods. Tests use [`MockLlmClient`], which
//! replays scripted responses (text, tool-calls, errors, streaming events) and
//! records every request it received so tests can assert on the messages and
//! tool schemas the agent sent.
//!
//! This is Phase 0 of the parity work (SMOODEV-1467): the foundation every
//! later phase (checkpointing, HITL resume, memory, RAG, structured output,
//! OTel) is tested against.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::Stream;
use futures_util::stream::{self, StreamExt};

use crate::conversation::Message;
use crate::llm::{LlmClient, LlmResponse, StreamEvent, Usage};
use crate::tool::{ToolCall, ToolSchema};

/// Boxed stream of streaming chat events — mirrors the return type of
/// [`LlmClient::chat_stream`](crate::llm::LlmClient::chat_stream).
pub type LlmEventStream = Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>;

/// The LLM call surface the agent loop depends on. Abstracting it behind a
/// trait lets production wire the real [`LlmClient`] while tests inject a
/// [`MockLlmClient`].
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Non-streaming completion.
    async fn chat(&self, messages: &[&Message], tools: &[ToolSchema]) -> anyhow::Result<LlmResponse>;

    /// Streaming completion. Yields incremental [`StreamEvent`]s.
    async fn chat_stream(&self, messages: &[&Message], tools: &[ToolSchema]) -> anyhow::Result<LlmEventStream>;
}

// The `LlmClient::` paths are intentional (not `Self::`): they fully-qualify the
// *inherent* methods so we delegate to the real implementation instead of
// recursing back into these trait methods.
#[allow(clippy::use_self)]
#[async_trait]
impl LlmProvider for LlmClient {
    async fn chat(&self, messages: &[&Message], tools: &[ToolSchema]) -> anyhow::Result<LlmResponse> {
        LlmClient::chat(self, messages, tools).await
    }

    async fn chat_stream(&self, messages: &[&Message], tools: &[ToolSchema]) -> anyhow::Result<LlmEventStream> {
        LlmClient::chat_stream(self, messages, tools).await
    }
}

/// Build a plain text [`LlmResponse`] with `stop` finish reason and otherwise
/// empty/default fields. Handy for scripting the mock and for assertions.
#[must_use]
pub fn text_response(content: impl Into<String>) -> LlmResponse {
    LlmResponse {
        content: content.into(),
        tool_calls: vec![],
        finish_reason: "stop".into(),
        usage: Usage::default(),
        rate_limit: None,
        gateway_cost_usd: None,
        resolved_model: None,
        reasoning_content: None,
    }
}

/// Build an [`LlmResponse`] that requests a single tool call (`tool_calls`
/// finish reason). `arguments` is the tool's JSON argument object.
#[must_use]
pub fn tool_call_response(id: impl Into<String>, name: impl Into<String>, arguments: serde_json::Value) -> LlmResponse {
    LlmResponse {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }],
        finish_reason: "tool_calls".into(),
        usage: Usage::default(),
        rate_limit: None,
        gateway_cost_usd: None,
        resolved_model: None,
        reasoning_content: None,
    }
}

/// One request the mock received, captured for assertions.
#[derive(Clone, Debug)]
pub struct RecordedCall {
    /// The messages passed to the call (cloned out of the `&[&Message]` slice).
    pub messages: Vec<Message>,
    /// The tool schemas offered to the model.
    pub tools: Vec<ToolSchema>,
    /// `true` if this was a `chat_stream` call, `false` for `chat`.
    pub streamed: bool,
}

/// A scripted outcome for a `chat` call.
enum ChatOutcome {
    Response(Box<LlmResponse>),
    Error(String),
}

/// A scripted outcome for a `chat_stream` call.
enum StreamOutcome {
    Events(Vec<StreamEvent>),
    SetupError(String),
}

#[derive(Default)]
struct MockState {
    chat: VecDeque<ChatOutcome>,
    streams: VecDeque<StreamOutcome>,
    calls: Vec<RecordedCall>,
}

/// A deterministic [`LlmProvider`] for tests. Script the responses it should
/// return (in FIFO order), drive your code, then assert on [`MockLlmClient::calls`].
///
/// Cloning shares the same underlying state (`Arc<Mutex<_>>`), so a clone handed
/// to an `Agent` and the original held by the test see the same script + recordings.
///
/// ```
/// use smooth_operator::llm_provider::{LlmProvider, MockLlmClient};
/// use smooth_operator::conversation::Message;
///
/// let rt = tokio::runtime::Runtime::new().unwrap();
/// rt.block_on(async {
///     let mock = MockLlmClient::new();
///     mock.push_text("hello there");
///     let resp = mock.chat(&[&Message::user("hi")], &[]).await.unwrap();
///     assert_eq!(resp.content, "hello there");
///     assert_eq!(mock.call_count(), 1);
/// });
/// ```
#[derive(Clone, Default)]
pub struct MockLlmClient {
    state: Arc<Mutex<MockState>>,
}

impl MockLlmClient {
    /// A mock with an empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MockState> {
        // Recover the guard even if a prior test panicked while holding it — a
        // poisoned mock mutex carries no invariant worth aborting over.
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Queue a full [`LlmResponse`] for the next `chat` call.
    pub fn push_response(&self, response: LlmResponse) -> &Self {
        self.lock().chat.push_back(ChatOutcome::Response(Box::new(response)));
        self
    }

    /// Queue a plain-text response for the next `chat` call.
    pub fn push_text(&self, content: impl Into<String>) -> &Self {
        self.push_response(text_response(content))
    }

    /// Queue a single-tool-call response for the next `chat` call.
    pub fn push_tool_call(&self, id: impl Into<String>, name: impl Into<String>, arguments: serde_json::Value) -> &Self {
        self.push_response(tool_call_response(id, name, arguments))
    }

    /// Queue an error for the next `chat` call.
    pub fn push_error(&self, message: impl Into<String>) -> &Self {
        self.lock().chat.push_back(ChatOutcome::Error(message.into()));
        self
    }

    /// Queue a sequence of [`StreamEvent`]s for the next `chat_stream` call.
    pub fn push_stream(&self, events: Vec<StreamEvent>) -> &Self {
        self.lock().streams.push_back(StreamOutcome::Events(events));
        self
    }

    /// Queue a setup error (returned before any events) for the next `chat_stream` call.
    pub fn push_stream_error(&self, message: impl Into<String>) -> &Self {
        self.lock().streams.push_back(StreamOutcome::SetupError(message.into()));
        self
    }

    /// Every request the mock has received so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.lock().calls.clone()
    }

    /// Number of requests received (chat + chat_stream).
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.lock().calls.len()
    }

    /// The most recent request, if any.
    #[must_use]
    pub fn last_call(&self) -> Option<RecordedCall> {
        self.lock().calls.last().cloned()
    }

    fn record(&self, messages: &[&Message], tools: &[ToolSchema], streamed: bool) {
        self.lock().calls.push(RecordedCall {
            messages: messages.iter().map(|m| (*m).clone()).collect(),
            tools: tools.to_vec(),
            streamed,
        });
    }
}

#[async_trait]
impl LlmProvider for MockLlmClient {
    async fn chat(&self, messages: &[&Message], tools: &[ToolSchema]) -> anyhow::Result<LlmResponse> {
        self.record(messages, tools, false);
        let outcome = self.lock().chat.pop_front();
        match outcome {
            Some(ChatOutcome::Response(r)) => Ok(*r),
            Some(ChatOutcome::Error(e)) => Err(anyhow::anyhow!(e)),
            // Empty script: a benign terminal response so loops don't hang.
            None => Ok(text_response("")),
        }
    }

    async fn chat_stream(&self, messages: &[&Message], tools: &[ToolSchema]) -> anyhow::Result<LlmEventStream> {
        self.record(messages, tools, true);
        let outcome = self.lock().streams.pop_front();
        match outcome {
            Some(StreamOutcome::Events(events)) => Ok(stream::iter(events.into_iter().map(Ok)).boxed()),
            Some(StreamOutcome::SetupError(e)) => Err(anyhow::anyhow!(e)),
            // Empty script: a single Done event so consumers terminate cleanly.
            None => Ok(stream::iter(vec![Ok(StreamEvent::Done { finish_reason: "stop".into() })]).boxed()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(m: &Message) -> Vec<&Message> {
        vec![m]
    }

    #[tokio::test]
    async fn chat_returns_scripted_responses_in_fifo_order() {
        let mock = MockLlmClient::new();
        mock.push_text("first").push_text("second");
        let u = Message::user("hi");

        let r1 = mock.chat(&msgs(&u), &[]).await.expect("first chat");
        let r2 = mock.chat(&msgs(&u), &[]).await.expect("second chat");

        assert_eq!(r1.content, "first");
        assert_eq!(r2.content, "second");
    }

    #[tokio::test]
    async fn chat_records_messages_and_tools() {
        let mock = MockLlmClient::new();
        mock.push_text("ok");
        let sys = Message::system("be helpful");
        let user = Message::user("hello");
        let tool = ToolSchema {
            name: "search".into(),
            description: "search the web".into(),
            parameters: serde_json::json!({"type": "object"}),
        };

        mock.chat(&[&sys, &user], std::slice::from_ref(&tool)).await.expect("chat");

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].messages.len(), 2);
        assert_eq!(calls[0].messages[0].content, "be helpful");
        assert_eq!(calls[0].messages[1].content, "hello");
        assert_eq!(calls[0].tools.len(), 1);
        assert_eq!(calls[0].tools[0].name, "search");
        assert!(!calls[0].streamed);
    }

    #[tokio::test]
    async fn chat_default_when_script_empty_is_benign_terminal() {
        let mock = MockLlmClient::new();
        let u = Message::user("hi");
        let r = mock.chat(&msgs(&u), &[]).await.expect("chat");
        assert_eq!(r.content, "");
        assert_eq!(r.finish_reason, "stop");
        assert!(r.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn chat_scripts_errors() {
        let mock = MockLlmClient::new();
        mock.push_error("rate limited");
        let u = Message::user("hi");
        let err = mock.chat(&msgs(&u), &[]).await.expect_err("should error");
        assert!(err.to_string().contains("rate limited"));
    }

    #[tokio::test]
    async fn tool_call_response_carries_the_call() {
        let mock = MockLlmClient::new();
        mock.push_tool_call("call_1", "get_weather", serde_json::json!({"city": "SF"}));
        let u = Message::user("weather?");
        let r = mock.chat(&msgs(&u), &[]).await.expect("chat");
        assert_eq!(r.finish_reason, "tool_calls");
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].name, "get_weather");
        assert_eq!(r.tool_calls[0].arguments["city"], "SF");
    }

    #[tokio::test]
    async fn chat_stream_yields_scripted_events() {
        let mock = MockLlmClient::new();
        mock.push_stream(vec![
            StreamEvent::Delta { content: "hel".into() },
            StreamEvent::Delta { content: "lo".into() },
            StreamEvent::Done { finish_reason: "stop".into() },
        ]);
        let u = Message::user("hi");

        let stream = mock.chat_stream(&msgs(&u), &[]).await.expect("stream");
        let events: Vec<_> = stream.collect::<Vec<_>>().await.into_iter().map(|e| e.expect("event")).collect();

        assert_eq!(events.len(), 3);
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Delta { content } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "hello");
        assert!(mock.last_call().expect("call").streamed);
    }

    #[tokio::test]
    async fn chat_stream_setup_error() {
        let mock = MockLlmClient::new();
        mock.push_stream_error("upstream 503");
        let u = Message::user("hi");
        // `chat_stream`'s Ok type (a boxed stream) isn't `Debug`, so `.err()` + `expect`.
        let err = mock.chat_stream(&msgs(&u), &[]).await.err().expect("should be an error");
        assert!(err.to_string().contains("upstream 503"));
    }

    #[tokio::test]
    async fn chat_stream_default_when_empty_terminates() {
        let mock = MockLlmClient::new();
        let u = Message::user("hi");
        let stream = mock.chat_stream(&msgs(&u), &[]).await.expect("stream");
        let events: Vec<_> = stream.collect::<Vec<_>>().await;
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Ok(StreamEvent::Done { .. })));
    }

    #[tokio::test]
    async fn clone_shares_state() {
        let mock = MockLlmClient::new();
        let clone = mock.clone();
        // Script on the original, consume via the clone.
        mock.push_text("shared");
        let u = Message::user("hi");
        let r = clone.chat(&msgs(&u), &[]).await.expect("chat");
        assert_eq!(r.content, "shared");
        // Recording is visible from the original too.
        assert_eq!(mock.call_count(), 1);
    }

    #[tokio::test]
    async fn usable_as_trait_object() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockLlmClient::new());
        let u = Message::user("hi");
        // Compiles + runs through the trait object.
        let r = provider.chat(&msgs(&u), &[]).await.expect("chat");
        assert_eq!(r.finish_reason, "stop");
    }
}

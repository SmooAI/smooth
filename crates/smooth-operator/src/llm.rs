use std::collections::HashMap;
use std::pin::Pin;

use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::ReceiverStream;

use crate::conversation::{Message, Role};
use crate::tool::{ToolCall, ToolSchema};

/// Configuration for the LLM client.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

impl LlmConfig {
    pub fn opencode_zen(api_key: impl Into<String>) -> Self {
        Self {
            api_url: "https://opencode.ai/zen/v1".into(),
            api_key: api_key.into(),
            model: "anthropic/claude-sonnet-4-20250514".into(),
            max_tokens: 8192,
            temperature: 0.0,
        }
    }

    pub fn anthropic(api_key: impl Into<String>) -> Self {
        Self {
            api_url: "https://api.anthropic.com/v1".into(),
            api_key: api_key.into(),
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 8192,
            temperature: 0.0,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_temperature(mut self, temp: f32) -> Self {
        self.temperature = temp;
        self
    }

    pub fn with_max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = max;
        self
    }
}

/// Response from the LLM.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// OpenAI-compatible chat completion request.
#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatTool>,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ChatToolCall>,
}

#[derive(Debug, Serialize)]
struct ChatTool {
    r#type: String,
    function: ChatFunction,
}

#[derive(Debug, Serialize)]
struct ChatFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatToolCall {
    id: String,
    r#type: String,
    function: ChatToolCallFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatToolCallFunction {
    name: String,
    arguments: String,
}

/// OpenAI-compatible chat completion response.
#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatToolCall>>,
}

#[derive(Debug, Deserialize)]
#[allow(clippy::struct_field_names)]
struct ChatUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

/// Events emitted during streaming LLM responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    Delta { content: String },
    ToolCallStart { id: String, name: String },
    ToolCallArgumentsDelta { id: String, arguments_chunk: String },
    Usage(Usage),
    Done { finish_reason: String },
}

/// A streaming chat completion chunk (OpenAI SSE format).
#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<StreamToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct StreamFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// LLM client using OpenAI-compatible chat completion API.
pub struct LlmClient {
    config: LlmConfig,
    client: reqwest::Client,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Send a chat completion request.
    ///
    /// # Errors
    /// Returns error if the API call fails or returns an invalid response.
    pub async fn chat(&self, messages: &[&Message], tools: &[ToolSchema]) -> anyhow::Result<LlmResponse> {
        let chat_messages: Vec<ChatMessage> = messages.iter().map(|m| to_chat_message(m)).collect();

        let chat_tools: Vec<ChatTool> = tools
            .iter()
            .map(|t| ChatTool {
                r#type: "function".into(),
                function: ChatFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect();

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: chat_messages,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            tools: chat_tools,
        };

        let url = format!("{}/chat/completions", self.config.api_url);

        let resp = self.client.post(&url).bearer_auth(&self.config.api_key).json(&request).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM API error {status}: {body}");
        }

        let chat_resp: ChatResponse = resp.json().await?;
        let choice = chat_resp.choices.into_iter().next().ok_or_else(|| anyhow::anyhow!("no choices in response"))?;

        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tc| {
                let args: serde_json::Value = serde_json::from_str(&tc.function.arguments).ok()?;
                Some(ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments: args,
                })
            })
            .collect();

        let usage = chat_resp.usage.map_or_else(Usage::default, |u| Usage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });

        Ok(LlmResponse {
            content: choice.message.content.unwrap_or_default(),
            tool_calls,
            finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".into()),
            usage,
        })
    }

    /// Send a streaming chat completion request.
    ///
    /// Returns a stream of `StreamEvent`s parsed from the OpenAI SSE format.
    /// The stream ends after a `StreamEvent::Done` event or when the server
    /// sends `data: [DONE]`.
    ///
    /// # Errors
    /// Returns error if the API call fails. Individual stream items may also
    /// contain errors for malformed chunks.
    pub async fn chat_stream(
        &self,
        messages: &[&Message],
        tools: &[ToolSchema],
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        let chat_messages: Vec<ChatMessage> = messages.iter().map(|m| to_chat_message(m)).collect();

        let chat_tools: Vec<ChatTool> = tools
            .iter()
            .map(|t| ChatTool {
                r#type: "function".into(),
                function: ChatFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect();

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: chat_messages,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            tools: chat_tools,
        };

        let url = format!("{}/chat/completions", self.config.api_url);

        let mut request_body = serde_json::to_value(&request)?;
        request_body
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("serialized request is not a JSON object"))?
            .insert("stream".into(), serde_json::Value::Bool(true));

        let resp = self.client.post(&url).bearer_auth(&self.config.api_key).json(&request_body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM API error {status}: {body}");
        }

        let byte_stream = resp.bytes_stream();

        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<StreamEvent>>(256);

        tokio::spawn(async move {
            let mut buffer = String::new();
            let mut stream = byte_stream;

            while let Some(chunk_result) = stream.next().await {
                let chunk: Bytes = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("stream read error: {e}"))).await;
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Process complete lines
                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim().to_string();
                    buffer = buffer[newline_pos + 1..].to_string();

                    let events = parse_sse_line(&line);
                    for event in events {
                        if tx.send(event).await.is_err() {
                            return; // receiver dropped
                        }
                    }
                }
            }

            // Process any remaining data in buffer
            let remaining = buffer.trim().to_string();
            if !remaining.is_empty() {
                let events = parse_sse_line(&remaining);
                for event in events {
                    if tx.send(event).await.is_err() {
                        return;
                    }
                }
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }
}

/// Parse a single SSE line into zero or more `StreamEvent`s.
///
/// Returns an empty vec for blank lines, `event:` lines, and comments.
/// Returns `Done` for the `[DONE]` sentinel.
/// Parses `data: {...}` JSON chunks into the appropriate event types.
fn parse_sse_line(line: &str) -> Vec<anyhow::Result<StreamEvent>> {
    let line = line.trim();

    // Skip empty lines, comments, and event: lines
    if line.is_empty() || line.starts_with(':') || line.starts_with("event:") {
        return vec![];
    }

    // Must be a data: line
    let data = if let Some(stripped) = line.strip_prefix("data:") {
        stripped.trim()
    } else {
        return vec![];
    };

    // [DONE] sentinel
    if data == "[DONE]" {
        return vec![Ok(StreamEvent::Done { finish_reason: "stop".into() })];
    }

    // Parse JSON chunk
    let chunk: StreamChunk = match serde_json::from_str(data) {
        Ok(c) => c,
        Err(e) => return vec![Err(anyhow::anyhow!("failed to parse SSE chunk: {e}"))],
    };

    let mut events = Vec::new();

    for choice in &chunk.choices {
        // Text delta
        if let Some(content) = &choice.delta.content {
            if !content.is_empty() {
                events.push(Ok(StreamEvent::Delta { content: content.clone() }));
            }
        }

        // Tool call deltas
        if let Some(tool_calls) = &choice.delta.tool_calls {
            for tc in tool_calls {
                if let Some(func) = &tc.function {
                    // If we have both an id and a name, this is a new tool call
                    if let (Some(id), Some(name)) = (&tc.id, &func.name) {
                        events.push(Ok(StreamEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                        }));
                    }
                    // If we have arguments, emit argument delta
                    if let Some(args) = &func.arguments {
                        if !args.is_empty() {
                            let id = tc.id.clone().unwrap_or_else(|| format!("index-{}", tc.index));
                            events.push(Ok(StreamEvent::ToolCallArgumentsDelta {
                                id,
                                arguments_chunk: args.clone(),
                            }));
                        }
                    }
                }
            }
        }

        // Finish reason
        if let Some(reason) = &choice.finish_reason {
            events.push(Ok(StreamEvent::Done { finish_reason: reason.clone() }));
        }
    }

    // Usage info (often in the last chunk)
    if let Some(usage) = &chunk.usage {
        events.push(Ok(StreamEvent::Usage(Usage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        })));
    }

    events
}

/// Accumulate stream events into a complete `LlmResponse`.
///
/// Consumes the entire stream, collecting text deltas into content,
/// tool call starts + argument deltas into complete tool calls,
/// and capturing usage and finish reason.
///
/// # Errors
/// Returns error if any stream event is an error.
pub async fn accumulate_stream_events(mut stream: Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>) -> anyhow::Result<LlmResponse> {
    let mut content = String::new();
    let mut finish_reason = String::from("stop");
    let mut usage = Usage::default();

    // Track tool calls: id -> (name, accumulated_arguments)
    let mut tool_call_map: HashMap<String, (String, String)> = HashMap::new();
    // Track ordering
    let mut tool_call_order: Vec<String> = Vec::new();

    while let Some(event_result) = stream.next().await {
        match event_result? {
            StreamEvent::Delta { content: delta } => {
                content.push_str(&delta);
            }
            StreamEvent::ToolCallStart { id, name } => {
                if !tool_call_map.contains_key(&id) {
                    tool_call_order.push(id.clone());
                }
                tool_call_map.insert(id, (name, String::new()));
            }
            StreamEvent::ToolCallArgumentsDelta { id, arguments_chunk } => {
                let entry = tool_call_map.entry(id.clone()).or_insert_with(|| {
                    tool_call_order.push(id);
                    (String::new(), String::new())
                });
                entry.1.push_str(&arguments_chunk);
            }
            StreamEvent::Usage(u) => {
                usage = u;
            }
            StreamEvent::Done { finish_reason: reason } => {
                finish_reason = reason;
            }
        }
    }

    let tool_calls: Vec<ToolCall> = tool_call_order
        .into_iter()
        .filter_map(|id| {
            let (name, args_str) = tool_call_map.remove(&id)?;
            let arguments: serde_json::Value = serde_json::from_str(&args_str).unwrap_or(serde_json::Value::Null);
            Some(ToolCall { id, name, arguments })
        })
        .collect();

    Ok(LlmResponse {
        content,
        tool_calls,
        finish_reason,
        usage,
    })
}

fn to_chat_message(msg: &Message) -> ChatMessage {
    let role = match msg.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    let tool_calls: Vec<ChatToolCall> = msg
        .tool_calls
        .iter()
        .map(|tc| ChatToolCall {
            id: tc.id.clone(),
            r#type: "function".into(),
            function: ChatToolCallFunction {
                name: tc.name.clone(),
                arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
            },
        })
        .collect();

    ChatMessage {
        role: role.into(),
        content: msg.content.clone(),
        tool_call_id: msg.tool_call_id.clone(),
        tool_calls,
    }
}

/// Try to load an API key from OpenCode's auth file.
///
/// # Errors
/// Returns error if the file cannot be read or parsed.
pub fn load_opencode_api_key() -> anyhow::Result<String> {
    let home = dirs_next::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let auth_path = home.join(".local/share/opencode/auth.json");
    let contents = std::fs::read_to_string(&auth_path)?;
    let auth: serde_json::Value = serde_json::from_str(&contents)?;
    auth["token"].as_str().map(String::from).ok_or_else(|| anyhow::anyhow!("no token in auth.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_zen_config() {
        let config = LlmConfig::opencode_zen("test-key");
        assert_eq!(config.api_url, "https://opencode.ai/zen/v1");
        assert!(!config.api_key.is_empty());
    }

    #[test]
    fn anthropic_config() {
        let config = LlmConfig::anthropic("sk-ant-test");
        assert_eq!(config.api_url, "https://api.anthropic.com/v1");
    }

    #[test]
    fn config_builder() {
        let config = LlmConfig::opencode_zen("key").with_model("gpt-4o").with_temperature(0.7).with_max_tokens(4096);
        assert_eq!(config.model, "gpt-4o");
        assert!((config.temperature - 0.7).abs() < f32::EPSILON);
        assert_eq!(config.max_tokens, 4096);
    }

    #[test]
    fn to_chat_message_user() {
        let msg = Message::user("Hello");
        let chat = to_chat_message(&msg);
        assert_eq!(chat.role, "user");
        assert_eq!(chat.content, "Hello");
        assert!(chat.tool_call_id.is_none());
    }

    #[test]
    fn to_chat_message_tool() {
        let msg = Message::tool_result("call-1", "result");
        let chat = to_chat_message(&msg);
        assert_eq!(chat.role, "tool");
        assert_eq!(chat.tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn chat_request_serialization() {
        let req = ChatRequest {
            model: "test-model".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "hello".into(),
                tool_call_id: None,
                tool_calls: vec![],
            }],
            max_tokens: 100,
            temperature: 0.0,
            tools: vec![],
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("test-model"));
        assert!(!json.contains("tools")); // empty vec should be skipped
    }

    #[test]
    fn chat_response_deserialization() {
        let json = r#"{
            "choices": [{
                "message": {"content": "Hello!", "tool_calls": null},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).expect("deserialize");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("Hello!"));
        assert_eq!(resp.usage.as_ref().map(|u| u.total_tokens), Some(15));
    }

    #[test]
    fn chat_response_with_tool_calls() {
        let json = r#"{
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {"name": "echo", "arguments": "{\"text\":\"hi\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).expect("deserialize");
        let tool_calls = resp.choices[0].message.tool_calls.as_ref().expect("tool_calls");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "echo");
    }

    #[test]
    fn usage_default() {
        let usage = Usage::default();
        assert_eq!(usage.total_tokens, 0);
    }

    // --- Streaming tests ---

    #[test]
    fn stream_event_delta_serialization() {
        let event = StreamEvent::Delta { content: "Hello".into() };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"Delta\""));
        assert!(json.contains("\"content\":\"Hello\""));
        let parsed: StreamEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            StreamEvent::Delta { content } => assert_eq!(content, "Hello"),
            _ => panic!("expected Delta"),
        }
    }

    #[test]
    fn stream_event_tool_call_start_serialization() {
        let event = StreamEvent::ToolCallStart {
            id: "call-1".into(),
            name: "echo".into(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"ToolCallStart\""));
        assert!(json.contains("\"id\":\"call-1\""));
        assert!(json.contains("\"name\":\"echo\""));
        let parsed: StreamEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            StreamEvent::ToolCallStart { id, name } => {
                assert_eq!(id, "call-1");
                assert_eq!(name, "echo");
            }
            _ => panic!("expected ToolCallStart"),
        }
    }

    #[test]
    fn stream_event_done_serialization() {
        let event = StreamEvent::Done { finish_reason: "stop".into() };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"Done\""));
        assert!(json.contains("\"finish_reason\":\"stop\""));
        let parsed: StreamEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            StreamEvent::Done { finish_reason } => assert_eq!(finish_reason, "stop"),
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn parse_sse_line_with_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}"#;
        let events = parse_sse_line(line);
        assert_eq!(events.len(), 1);
        match events[0].as_ref().expect("ok") {
            StreamEvent::Delta { content } => assert_eq!(content, "Hi"),
            other => panic!("expected Delta, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_line_done_sentinel() {
        let line = "data: [DONE]";
        let events = parse_sse_line(line);
        assert_eq!(events.len(), 1);
        match events[0].as_ref().expect("ok") {
            StreamEvent::Done { finish_reason } => assert_eq!(finish_reason, "stop"),
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_line_skips_empty_and_malformed() {
        assert!(parse_sse_line("").is_empty());
        assert!(parse_sse_line("   ").is_empty());
        assert!(parse_sse_line(": comment").is_empty());
        assert!(parse_sse_line("event: chunk").is_empty());
        assert!(parse_sse_line("not a data line").is_empty());
    }

    #[tokio::test]
    async fn accumulate_stream_events_collects_deltas() {
        let events = vec![
            Ok(StreamEvent::Delta { content: "Hello".into() }),
            Ok(StreamEvent::Delta { content: " world".into() }),
            Ok(StreamEvent::Usage(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            })),
            Ok(StreamEvent::Done { finish_reason: "stop".into() }),
        ];
        let stream: Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>> = Box::pin(futures_util::stream::iter(events));
        let response = accumulate_stream_events(stream).await.expect("accumulate");
        assert_eq!(response.content, "Hello world");
        assert_eq!(response.finish_reason, "stop");
        assert_eq!(response.usage.total_tokens, 15);
        assert!(response.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn accumulate_stream_events_collects_tool_calls() {
        let events = vec![
            Ok(StreamEvent::ToolCallStart {
                id: "call-1".into(),
                name: "echo".into(),
            }),
            Ok(StreamEvent::ToolCallArgumentsDelta {
                id: "call-1".into(),
                arguments_chunk: r#"{"tex"#.into(),
            }),
            Ok(StreamEvent::ToolCallArgumentsDelta {
                id: "call-1".into(),
                arguments_chunk: r#"t":"hi"}"#.into(),
            }),
            Ok(StreamEvent::Done {
                finish_reason: "tool_calls".into(),
            }),
        ];
        let stream: Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>> = Box::pin(futures_util::stream::iter(events));
        let response = accumulate_stream_events(stream).await.expect("accumulate");
        assert!(response.content.is_empty());
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "echo");
        assert_eq!(response.tool_calls[0].id, "call-1");
        assert_eq!(response.tool_calls[0].arguments, serde_json::json!({"text": "hi"}));
        assert_eq!(response.finish_reason, "tool_calls");
    }
}

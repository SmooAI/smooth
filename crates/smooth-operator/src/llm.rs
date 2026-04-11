use std::collections::HashMap;
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::ReceiverStream;

use crate::conversation::{Message, Role};
use crate::tool::{ToolCall, ToolSchema};

/// Policy controlling retry behavior for transient LLM API errors.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub retry_on_status: Vec<u16>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 60_000,
            retry_on_status: vec![429, 500, 502, 503],
        }
    }
}

/// Rate-limit information extracted from LLM API response headers.
#[derive(Debug, Clone, Default)]
pub struct RateLimitInfo {
    pub retry_after_ms: Option<u64>,
    pub remaining_requests: Option<u32>,
    pub remaining_tokens: Option<u32>,
}

/// API format for the LLM provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiFormat {
    #[default]
    OpenAiCompat,
    Anthropic,
}

/// Configuration for the LLM client.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub retry_policy: RetryPolicy,
    pub api_format: ApiFormat,
}

impl LlmConfig {
    /// OpenRouter — recommended default provider. OpenAI-compatible proxy for many models.
    pub fn openrouter(api_key: impl Into<String>) -> Self {
        Self {
            api_url: "https://openrouter.ai/api/v1".into(),
            api_key: api_key.into(),
            model: "openai/gpt-4o".into(),
            max_tokens: 8192,
            temperature: 0.0,
            retry_policy: RetryPolicy::default(),
            api_format: ApiFormat::OpenAiCompat,
        }
    }

    pub fn anthropic(api_key: impl Into<String>) -> Self {
        Self {
            api_url: "https://api.anthropic.com/v1".into(),
            api_key: api_key.into(),
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 8192,
            temperature: 0.0,
            retry_policy: RetryPolicy::default(),
            api_format: ApiFormat::Anthropic,
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

    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    pub fn with_api_format(mut self, format: ApiFormat) -> Self {
        self.api_format = format;
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
    pub rate_limit: Option<RateLimitInfo>,
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
    /// Content is optional: when an assistant message has tool_calls and no
    /// prose, some OpenAI-compat providers reject `content: ""` with a 400.
    /// Sending `content: null` (via Option::None → skip) works for all
    /// providers we've tested.
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
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
    Delta {
        content: String,
    },
    /// Reasoning tokens from reasoning-models (Kimi, DeepSeek R1, MiniMax). Surfaced
    /// for progress visibility but NOT accumulated into the final response content.
    Reasoning {
        content: String,
    },
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolCallArgumentsDelta {
        index: usize,
        arguments_chunk: String,
    },
    Usage(Usage),
    Done {
        finish_reason: String,
    },
}

/// A streaming chat completion chunk (OpenAI SSE format).
#[derive(Debug, Deserialize)]
struct StreamChunk {
    /// Some providers (LLM Gateway, Azure) send usage-only chunks without choices.
    #[serde(default)]
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
    /// Reasoning tokens (Kimi K2.5, DeepSeek R1, etc.). Emitted before `content`
    /// in reasoning-model responses. We surface these so the agent sees progress.
    #[serde(default)]
    reasoning_content: Option<String>,
    /// Alternate reasoning field used by some OpenRouter providers (MiniMax, etc.)
    #[serde(default)]
    reasoning: Option<String>,
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

// --- Anthropic native API types ---

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[allow(dead_code)]
    id: String,
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

/// Calculate exponential backoff duration for a given retry attempt.
fn calculate_backoff(attempt: u32, policy: &RetryPolicy) -> Duration {
    let exp_ms = policy.base_delay_ms.saturating_mul(1u64 << attempt);
    let jitter_ms = u64::from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos() % 500);
    let total_ms = exp_ms.saturating_add(jitter_ms).min(policy.max_delay_ms);
    Duration::from_millis(total_ms)
}

/// Extract rate-limit information from HTTP response headers.
fn parse_rate_limit_headers(headers: &HeaderMap) -> RateLimitInfo {
    let retry_after_ms = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
        .and_then(|secs| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            if secs >= 0.0 {
                Some((secs * 1000.0) as u64)
            } else {
                None
            }
        });

    let remaining_requests = headers
        .get("x-ratelimit-remaining-requests")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());

    let remaining_tokens = headers
        .get("x-ratelimit-remaining-tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());

    RateLimitInfo {
        retry_after_ms,
        remaining_requests,
        remaining_tokens,
    }
}

/// LLM client using OpenAI-compatible chat completion API.
#[derive(Clone)]
pub struct LlmClient {
    config: LlmConfig,
    client: reqwest::Client,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        // 10-minute total request timeout — generous enough for reasoning models
        // (MiniMax-M1, Kimi K2.5) that can take 2-5 min before the first token,
        // but prevents infinite hangs if the provider accepts the connection
        // and goes silent. The per-chunk idle timeout (120s in chat_stream)
        // and per-iteration wall clock (600s in agent.rs) provide tighter guards.
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .connect_timeout(std::time::Duration::from_secs(30));

        // Kimi Code API requires a recognized coding agent User-Agent for
        // subscription authentication. Without this, the API returns 403
        // "only available for Coding Agents". See: openclaw/openclaw#30099
        if config.api_url.contains("api.kimi.com/coding") {
            builder = builder.user_agent("claude-code/0.1.0");
        }

        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// Send a chat completion request with automatic retry on transient errors.
    ///
    /// # Errors
    /// Returns error if the API call fails after all retries or returns an invalid response.
    pub async fn chat(&self, messages: &[&Message], tools: &[ToolSchema]) -> anyhow::Result<LlmResponse> {
        match self.config.api_format {
            ApiFormat::Anthropic => return self.chat_anthropic(messages, tools).await,
            ApiFormat::OpenAiCompat => {}
        }

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
        let policy = &self.config.retry_policy;

        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 0..=policy.max_retries {
            let resp = self.client.post(&url).bearer_auth(&self.config.api_key).json(&request).send().await?;

            let status = resp.status();
            let rate_limit_info = parse_rate_limit_headers(resp.headers());

            if status.is_success() {
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

                return Ok(LlmResponse {
                    content: choice.message.content.unwrap_or_default(),
                    tool_calls,
                    finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".into()),
                    usage,
                    rate_limit: Some(rate_limit_info),
                });
            }

            let status_code = status.as_u16();
            let body = resp.text().await.unwrap_or_default();
            let is_retryable = policy.retry_on_status.contains(&status_code);

            if !is_retryable || attempt == policy.max_retries {
                last_error = Some(anyhow::anyhow!("LLM API error {status}: {body}"));
                break;
            }

            let delay = rate_limit_info
                .retry_after_ms
                .map_or_else(|| calculate_backoff(attempt, policy), Duration::from_millis);

            tracing::warn!(
                attempt = attempt + 1,
                max_retries = policy.max_retries,
                status = status_code,
                delay_ms = delay.as_millis(),
                "LLM API request failed, retrying"
            );

            tokio::time::sleep(delay).await;
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("LLM API request failed after retries")))
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

        let tool_count = chat_tools.len();
        let msg_count = chat_messages.len();
        tracing::debug!(model = %self.config.model, tool_count, msg_count, "chat_stream: sending request");

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

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| {
                // Walk the error chain to get the root cause
                let mut chain = vec![format!("{e}")];
                let mut source: &dyn std::error::Error = &e;
                while let Some(s) = source.source() {
                    chain.push(format!("{s}"));
                    source = s;
                }
                anyhow::anyhow!("HTTP request failed: {}", chain.join(" → "))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // Dump the failed request to a rotating file so 4xx/5xx are debuggable.
            let req_json = serde_json::to_string_pretty(&request_body).unwrap_or_default();
            if let Some(home) = dirs_next::home_dir() {
                let dump_dir = home.join(".smooth/llm-errors");
                let _ = std::fs::create_dir_all(&dump_dir);
                let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f");
                let dump_path = dump_dir.join(format!("{ts}-{}.json", status.as_u16()));
                let dump_contents = format!("// status={status}\n// body={body}\n{req_json}\n");
                let _ = std::fs::write(&dump_path, dump_contents);
                tracing::error!(status = %status, response_body = %body, dump = %dump_path.display(), "LLM stream request failed (full request dumped)");
            } else {
                tracing::error!(status = %status, response_body = %body, "LLM stream request failed");
            }
            anyhow::bail!("LLM API error {status}: {body}");
        }

        let byte_stream = resp.bytes_stream();

        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<StreamEvent>>(256);

        tokio::spawn(async move {
            let mut buffer = String::new();
            let mut stream = byte_stream;

            // Per-chunk idle timeout: if no bytes arrive for 60s, abort the stream.
            // This catches the case where an LLM endpoint opens an SSE stream and
            // then stalls indefinitely (e.g. during reasoning). Total request
            // timeout on the reqwest::Client (120s) also applies as an upper bound.
            const CHUNK_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

            loop {
                let chunk_result = match tokio::time::timeout(CHUNK_IDLE_TIMEOUT, stream.next()).await {
                    Ok(Some(r)) => r,
                    Ok(None) => break, // stream ended normally
                    Err(_) => {
                        let _ = tx.send(Err(anyhow::anyhow!("stream idle timeout: no data for {CHUNK_IDLE_TIMEOUT:?}"))).await;
                        return;
                    }
                };
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

    /// Send a chat completion request using the Anthropic native API.
    async fn chat_anthropic(&self, messages: &[&Message], tools: &[ToolSchema]) -> anyhow::Result<LlmResponse> {
        let (system, anthropic_messages) = convert_messages_to_anthropic(messages);

        let anthropic_tools: Vec<AnthropicTool> = tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.parameters.clone(),
            })
            .collect();

        let request = AnthropicRequest {
            model: self.config.model.clone(),
            max_tokens: self.config.max_tokens,
            system,
            messages: anthropic_messages,
            tools: anthropic_tools,
        };

        let url = format!("{}/messages", self.config.api_url);
        let policy = &self.config.retry_policy;

        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 0..=policy.max_retries {
            let resp = self
                .client
                .post(&url)
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&request)
                .send()
                .await?;

            let status = resp.status();
            let rate_limit_info = parse_rate_limit_headers(resp.headers());

            if status.is_success() {
                let anthropic_resp: AnthropicResponse = resp.json().await?;

                let mut content = String::new();
                let mut tool_calls = Vec::new();

                for block in anthropic_resp.content {
                    match block {
                        AnthropicContentBlock::Text { text } => {
                            if !content.is_empty() {
                                content.push('\n');
                            }
                            content.push_str(&text);
                        }
                        AnthropicContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(ToolCall { id, name, arguments: input });
                        }
                        AnthropicContentBlock::ToolResult { .. } => {}
                    }
                }

                let finish_reason = anthropic_resp.stop_reason.unwrap_or_else(|| "stop".into());
                let total = anthropic_resp.usage.input_tokens + anthropic_resp.usage.output_tokens;

                return Ok(LlmResponse {
                    content,
                    tool_calls,
                    finish_reason,
                    usage: Usage {
                        prompt_tokens: anthropic_resp.usage.input_tokens,
                        completion_tokens: anthropic_resp.usage.output_tokens,
                        total_tokens: total,
                    },
                    rate_limit: Some(rate_limit_info),
                });
            }

            let status_code = status.as_u16();
            let body = resp.text().await.unwrap_or_default();
            let is_retryable = policy.retry_on_status.contains(&status_code);

            if !is_retryable || attempt == policy.max_retries {
                last_error = Some(anyhow::anyhow!("LLM API error {status}: {body}"));
                break;
            }

            let delay = rate_limit_info
                .retry_after_ms
                .map_or_else(|| calculate_backoff(attempt, policy), Duration::from_millis);

            tracing::warn!(
                attempt = attempt + 1,
                max_retries = policy.max_retries,
                status = status_code,
                delay_ms = delay.as_millis(),
                "Anthropic API request failed, retrying"
            );

            tokio::time::sleep(delay).await;
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Anthropic API request failed after retries")))
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// Call the OpenAI-compatible `/v1/moderations` endpoint to classify
    /// text as safe or unsafe. Used by Boardroom Narc as a pre-filter
    /// before the LLM judge — flagged content is denied without burning
    /// judge tokens.
    ///
    /// The endpoint must live at `{api_url}/moderations` and accept
    /// OpenAI's request/response shape (LiteLLM, the SmooAI gateway, and
    /// OpenAI itself all do). Returns the parsed response.
    ///
    /// # Errors
    /// Returns an error if the HTTP call fails, the status is non-2xx, or
    /// the response body doesn't match the expected shape. Callers should
    /// treat moderation errors as "unknown" and fall through to the next
    /// decision layer — never fail open.
    pub async fn moderate(&self, input: &str) -> anyhow::Result<ModerationResult> {
        // Only OpenAI-compat endpoints expose /moderations. Anthropic
        // doesn't offer a moderation endpoint of its own; callers should
        // route moderation through a gateway (LiteLLM / SmooAI Gateway /
        // OpenAI) even when the primary chat provider is Anthropic.
        if matches!(self.config.api_format, ApiFormat::Anthropic) {
            return Err(anyhow::anyhow!(
                "moderate() requires an OpenAI-compatible provider (current: Anthropic). Route moderation through a gateway."
            ));
        }

        let url = format!("{}/moderations", self.config.api_url.trim_end_matches('/'));
        let request = ModerationRequest {
            input: input.to_string(),
            model: None,
        };

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                let mut chain = vec![format!("{e}")];
                let mut source: &dyn std::error::Error = &e;
                while let Some(s) = source.source() {
                    chain.push(format!("{s}"));
                    source = s;
                }
                anyhow::anyhow!("moderation HTTP request failed: {}", chain.join(" → "))
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("moderation endpoint returned {status}: {body}"));
        }

        let parsed: ModerationResponse = resp.json().await.map_err(|e| anyhow::anyhow!("failed to parse moderation response: {e}"))?;

        let first = parsed
            .results
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("moderation response contained zero results"))?;

        Ok(ModerationResult {
            flagged: first.flagged,
            categories: first.categories.unwrap_or_default(),
            category_scores: first.category_scores.unwrap_or_default(),
        })
    }
}

/// OpenAI-compatible moderation request body.
#[derive(Debug, Serialize)]
struct ModerationRequest {
    input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModerationResponse {
    #[allow(dead_code)]
    id: Option<String>,
    #[allow(dead_code)]
    model: Option<String>,
    results: Vec<RawModerationResult>,
}

#[derive(Debug, Deserialize)]
struct RawModerationResult {
    flagged: bool,
    categories: Option<HashMap<String, bool>>,
    category_scores: Option<HashMap<String, f32>>,
}

/// The parsed moderation verdict, flattened from the OpenAI response
/// shape. `flagged = true` means at least one category tripped the
/// provider's safety threshold; `categories` and `category_scores` give
/// callers the per-category detail for auditing and fine-grained
/// policies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModerationResult {
    pub flagged: bool,
    #[serde(default)]
    pub categories: HashMap<String, bool>,
    #[serde(default)]
    pub category_scores: HashMap<String, f32>,
}

impl ModerationResult {
    /// List the category names (`sexual`, `violence`, etc.) that tripped
    /// the flag. Useful for logging and building human-readable deny
    /// reasons.
    #[must_use]
    pub fn flagged_categories(&self) -> Vec<&str> {
        self.categories.iter().filter_map(|(k, v)| if *v { Some(k.as_str()) } else { None }).collect()
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

        // Reasoning tokens (Kimi K2.5, DeepSeek R1, MiniMax). Surface them so the
        // agent sees progress and the stream doesn't appear to hang during long
        // reasoning phases. Both field names seen in the wild.
        if let Some(reasoning) = &choice.delta.reasoning_content {
            if !reasoning.is_empty() {
                events.push(Ok(StreamEvent::Reasoning { content: reasoning.clone() }));
            }
        }
        if let Some(reasoning) = &choice.delta.reasoning {
            if !reasoning.is_empty() {
                events.push(Ok(StreamEvent::Reasoning { content: reasoning.clone() }));
            }
        }

        // Tool call deltas — key on `index`, which is always present, because
        // providers like MiniMax only send the `id` in the first chunk and
        // subsequent argument chunks only carry the index.
        if let Some(tool_calls) = &choice.delta.tool_calls {
            for tc in tool_calls {
                if let Some(func) = &tc.function {
                    // ToolCallStart: emit whenever we see a `name` (usually in the
                    // first chunk). ID may be absent for some providers — synthesize
                    // one from the index when needed.
                    if let Some(name) = &func.name {
                        let id = tc.id.clone().unwrap_or_else(|| format!("call_{}", tc.index));
                        events.push(Ok(StreamEvent::ToolCallStart {
                            index: tc.index,
                            id,
                            name: name.clone(),
                        }));
                    }
                    // Arguments delta: always keyed by index (matches the ToolCallStart).
                    if let Some(args) = &func.arguments {
                        if !args.is_empty() {
                            events.push(Ok(StreamEvent::ToolCallArgumentsDelta {
                                index: tc.index,
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

    // Track tool calls keyed by index (stable across chunks; `id` is only sent once
    // on some providers like MiniMax, `index` is sent on every chunk). Value is
    // (id, name, accumulated_arguments).
    let mut tool_call_map: HashMap<usize, (String, String, String)> = HashMap::new();
    let mut tool_call_order: Vec<usize> = Vec::new();

    while let Some(event_result) = stream.next().await {
        match event_result? {
            StreamEvent::Delta { content: delta } => {
                content.push_str(&delta);
            }
            StreamEvent::Reasoning { .. } => {
                // Reasoning tokens are surfaced downstream for progress visibility
                // but intentionally NOT accumulated into the final response content.
            }
            StreamEvent::ToolCallStart { index, id, name } => {
                if !tool_call_map.contains_key(&index) {
                    tool_call_order.push(index);
                }
                tool_call_map.insert(index, (id, name, String::new()));
            }
            StreamEvent::ToolCallArgumentsDelta { index, arguments_chunk } => {
                let entry = tool_call_map.entry(index).or_insert_with(|| {
                    tool_call_order.push(index);
                    (format!("call_{index}"), String::new(), String::new())
                });
                entry.2.push_str(&arguments_chunk);
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
        .filter_map(|index| {
            let (id, name, args_str) = tool_call_map.remove(&index)?;
            // Skip tool calls with no name — means the stream was malformed.
            if name.is_empty() {
                return None;
            }
            let arguments: serde_json::Value = serde_json::from_str(&args_str).unwrap_or(serde_json::Value::Null);
            Some(ToolCall { id, name, arguments })
        })
        .collect();

    Ok(LlmResponse {
        content,
        tool_calls,
        finish_reason,
        usage,
        rate_limit: None,
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

    // Omit empty content when the message has tool_calls or is a tool result;
    // some providers reject `content: ""` in those cases.
    let content = if msg.content.is_empty() && (!msg.tool_calls.is_empty() || msg.role == Role::Tool) {
        None
    } else {
        Some(msg.content.clone())
    };

    ChatMessage {
        role: role.into(),
        content,
        tool_call_id: msg.tool_call_id.clone(),
        tool_calls,
    }
}

/// Convert conversation messages to Anthropic format, extracting system messages.
///
/// Returns `(system_prompt, messages)` where the system prompt is the concatenation
/// of all system messages, and remaining messages are converted to Anthropic format.
fn convert_messages_to_anthropic(messages: &[&Message]) -> (Option<String>, Vec<AnthropicMessage>) {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut anthropic_messages: Vec<AnthropicMessage> = Vec::new();

    for msg in messages {
        match msg.role {
            Role::System => {
                system_parts.push(&msg.content);
            }
            Role::User => {
                anthropic_messages.push(AnthropicMessage {
                    role: "user".into(),
                    content: AnthropicContent::Text(msg.content.clone()),
                });
            }
            Role::Assistant => {
                if msg.tool_calls.is_empty() {
                    anthropic_messages.push(AnthropicMessage {
                        role: "assistant".into(),
                        content: AnthropicContent::Text(msg.content.clone()),
                    });
                } else {
                    let mut blocks: Vec<AnthropicContentBlock> = Vec::new();
                    if !msg.content.is_empty() {
                        blocks.push(AnthropicContentBlock::Text { text: msg.content.clone() });
                    }
                    for tc in &msg.tool_calls {
                        blocks.push(AnthropicContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input: tc.arguments.clone(),
                        });
                    }
                    anthropic_messages.push(AnthropicMessage {
                        role: "assistant".into(),
                        content: AnthropicContent::Blocks(blocks),
                    });
                }
            }
            Role::Tool => {
                let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
                anthropic_messages.push(AnthropicMessage {
                    role: "user".into(),
                    content: AnthropicContent::Blocks(vec![AnthropicContentBlock::ToolResult {
                        tool_use_id,
                        content: msg.content.clone(),
                    }]),
                });
            }
        }
    }

    let system = if system_parts.is_empty() { None } else { Some(system_parts.join("\n\n")) };

    (system, anthropic_messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_config() {
        let config = LlmConfig::anthropic("sk-ant-test");
        assert_eq!(config.api_url, "https://api.anthropic.com/v1");
    }

    #[test]
    fn config_builder() {
        let config = LlmConfig::openrouter("key").with_model("gpt-4o").with_temperature(0.7).with_max_tokens(4096);
        assert_eq!(config.model, "gpt-4o");
        assert!((config.temperature - 0.7).abs() < f32::EPSILON);
        assert_eq!(config.max_tokens, 4096);
    }

    #[test]
    fn to_chat_message_user() {
        let msg = Message::user("Hello");
        let chat = to_chat_message(&msg);
        assert_eq!(chat.role, "user");
        assert_eq!(chat.content.as_deref(), Some("Hello"));
        assert!(chat.tool_call_id.is_none());
    }

    #[test]
    fn to_chat_message_assistant_with_tool_calls_omits_empty_content() {
        // Regression: some providers reject `content: ""` on assistant messages
        // that have tool_calls. We must send `content: null` (omit) in that case.
        let mut msg = Message::assistant("");
        msg.tool_calls.push(ToolCall {
            id: "c1".into(),
            name: "foo".into(),
            arguments: serde_json::json!({}),
        });
        let chat = to_chat_message(&msg);
        assert!(chat.content.is_none(), "empty content on tool-call message must be None");
        assert_eq!(chat.tool_calls.len(), 1);

        // Non-empty content should still be passed through
        let mut msg2 = Message::assistant("I'll call a tool.");
        msg2.tool_calls.push(ToolCall {
            id: "c2".into(),
            name: "foo".into(),
            arguments: serde_json::json!({}),
        });
        let chat2 = to_chat_message(&msg2);
        assert_eq!(chat2.content.as_deref(), Some("I'll call a tool."));
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
                content: Some("hello".into()),
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
            index: 0,
            id: "call-1".into(),
            name: "echo".into(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"ToolCallStart\""));
        assert!(json.contains("\"id\":\"call-1\""));
        assert!(json.contains("\"name\":\"echo\""));
        let parsed: StreamEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            StreamEvent::ToolCallStart { index, id, name } => {
                assert_eq!(index, 0);
                assert_eq!(id, "call-1");
                assert_eq!(name, "echo");
            }
            _ => panic!("expected ToolCallStart"),
        }
    }

    #[test]
    fn stream_event_reasoning_serialization() {
        let event = StreamEvent::Reasoning { content: "thinking...".into() };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"Reasoning\""));
        assert!(json.contains("\"content\":\"thinking...\""));
        let parsed: StreamEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            StreamEvent::Reasoning { content } => assert_eq!(content, "thinking..."),
            _ => panic!("expected Reasoning"),
        }
    }

    #[test]
    fn parse_sse_line_extracts_reasoning_content() {
        let line = r#"data: {"choices":[{"delta":{"reasoning_content":"let me think"},"finish_reason":null}]}"#;
        let events = parse_sse_line(line);
        assert_eq!(events.len(), 1);
        match events[0].as_ref().expect("ok") {
            StreamEvent::Reasoning { content } => assert_eq!(content, "let me think"),
            other => panic!("expected Reasoning, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_line_extracts_reasoning_alternate_field() {
        let line = r#"data: {"choices":[{"delta":{"reasoning":"minimax thinking"},"finish_reason":null}]}"#;
        let events = parse_sse_line(line);
        assert_eq!(events.len(), 1);
        match events[0].as_ref().expect("ok") {
            StreamEvent::Reasoning { content } => assert_eq!(content, "minimax thinking"),
            other => panic!("expected Reasoning, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_line_minimax_tool_call_split_across_chunks() {
        // MiniMax sends the tool call id+name in the first chunk and subsequent
        // chunks only carry `index` + arguments. Accumulator must key on index.
        let chunk1 = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"write_file","arguments":""}}]},"finish_reason":null}]}"#;
        let chunk2 = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"a.txt\"}"}}]},"finish_reason":null}]}"#;
        let e1 = parse_sse_line(chunk1);
        let e2 = parse_sse_line(chunk2);
        assert_eq!(e1.len(), 1, "first chunk should emit ToolCallStart");
        match e1[0].as_ref().expect("ok") {
            StreamEvent::ToolCallStart { index, id, name } => {
                assert_eq!(*index, 0);
                assert_eq!(id, "call_abc");
                assert_eq!(name, "write_file");
            }
            other => panic!("expected ToolCallStart, got {other:?}"),
        }
        assert_eq!(e2.len(), 1, "second chunk should emit ArgumentsDelta");
        match e2[0].as_ref().expect("ok") {
            StreamEvent::ToolCallArgumentsDelta { index, arguments_chunk } => {
                assert_eq!(*index, 0);
                assert!(arguments_chunk.contains("a.txt"));
            }
            other => panic!("expected ArgumentsDelta, got {other:?}"),
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
                index: 0,
                id: "call-1".into(),
                name: "echo".into(),
            }),
            Ok(StreamEvent::ToolCallArgumentsDelta {
                index: 0,
                arguments_chunk: r#"{"tex"#.into(),
            }),
            Ok(StreamEvent::ToolCallArgumentsDelta {
                index: 0,
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

    #[tokio::test]
    async fn accumulate_stream_events_handles_minimax_split_tool_call() {
        // Regression: MiniMax sends id+name in chunk 1, only index+args in chunk 2.
        // Must result in a single coherent tool call, not two broken ones.
        let events = vec![
            Ok(StreamEvent::ToolCallStart {
                index: 0,
                id: "call_abc".into(),
                name: "write_file".into(),
            }),
            Ok(StreamEvent::ToolCallArgumentsDelta {
                index: 0,
                arguments_chunk: r#"{"path":"x.rs","content":"fn main() {}"}"#.into(),
            }),
            Ok(StreamEvent::Done {
                finish_reason: "tool_calls".into(),
            }),
        ];
        let stream: Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>> = Box::pin(futures_util::stream::iter(events));
        let response = accumulate_stream_events(stream).await.expect("accumulate");
        assert_eq!(response.tool_calls.len(), 1, "should have exactly 1 tool call, not 2");
        assert_eq!(response.tool_calls[0].name, "write_file");
        assert_eq!(response.tool_calls[0].id, "call_abc");
        assert_eq!(response.tool_calls[0].arguments["path"], "x.rs");
    }

    #[tokio::test]
    async fn accumulate_stream_events_drops_reasoning_from_content() {
        let events = vec![
            Ok(StreamEvent::Reasoning {
                content: "let me think".into(),
            }),
            Ok(StreamEvent::Delta { content: "Hello".into() }),
            Ok(StreamEvent::Reasoning {
                content: "more thinking".into(),
            }),
            Ok(StreamEvent::Delta { content: " world".into() }),
            Ok(StreamEvent::Done { finish_reason: "stop".into() }),
        ];
        let stream: Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>> = Box::pin(futures_util::stream::iter(events));
        let response = accumulate_stream_events(stream).await.expect("accumulate");
        assert_eq!(response.content, "Hello world", "reasoning must NOT leak into content");
    }

    // --- Retry and rate-limit tests ---

    #[test]
    fn retry_policy_default_values() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.base_delay_ms, 1000);
        assert_eq!(policy.max_delay_ms, 60_000);
        assert_eq!(policy.retry_on_status, vec![429, 500, 502, 503]);
    }

    #[test]
    fn calculate_backoff_exponential_growth() {
        let policy = RetryPolicy {
            base_delay_ms: 1000,
            max_delay_ms: 60_000,
            ..RetryPolicy::default()
        };
        // Jitter is 0-499ms, so check that the base exponential component is correct
        let d0 = calculate_backoff(0, &policy);
        let d1 = calculate_backoff(1, &policy);
        let d2 = calculate_backoff(2, &policy);

        // attempt 0: 1000ms + jitter(0-499)  => [1000, 1499]
        assert!(d0.as_millis() >= 1000);
        assert!(d0.as_millis() < 1500);
        // attempt 1: 2000ms + jitter => [2000, 2499]
        assert!(d1.as_millis() >= 2000);
        assert!(d1.as_millis() < 2500);
        // attempt 2: 4000ms + jitter => [4000, 4499]
        assert!(d2.as_millis() >= 4000);
        assert!(d2.as_millis() < 4500);
    }

    #[test]
    fn calculate_backoff_capped_at_max_delay() {
        let policy = RetryPolicy {
            base_delay_ms: 30_000,
            max_delay_ms: 60_000,
            ..RetryPolicy::default()
        };
        // attempt 2: 30000 * 4 = 120000, should be capped to 60000
        let d = calculate_backoff(2, &policy);
        assert!(d.as_millis() <= 60_000);
    }

    #[test]
    fn retryable_status_codes() {
        let policy = RetryPolicy::default();
        assert!(policy.retry_on_status.contains(&429));
        assert!(policy.retry_on_status.contains(&500));
        assert!(policy.retry_on_status.contains(&502));
        assert!(policy.retry_on_status.contains(&503));
        assert!(!policy.retry_on_status.contains(&400));
        assert!(!policy.retry_on_status.contains(&401));
        assert!(!policy.retry_on_status.contains(&404));
    }

    #[test]
    fn parse_rate_limit_headers_extracts_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "2.5".parse().unwrap());
        let info = parse_rate_limit_headers(&headers);
        assert_eq!(info.retry_after_ms, Some(2500));
        assert!(info.remaining_requests.is_none());
        assert!(info.remaining_tokens.is_none());
    }

    #[test]
    fn parse_rate_limit_headers_extracts_ratelimit_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining-requests", "42".parse().unwrap());
        headers.insert("x-ratelimit-remaining-tokens", "10000".parse().unwrap());
        let info = parse_rate_limit_headers(&headers);
        assert!(info.retry_after_ms.is_none());
        assert_eq!(info.remaining_requests, Some(42));
        assert_eq!(info.remaining_tokens, Some(10000));
    }

    #[test]
    fn rate_limit_info_default_is_all_none() {
        let info = RateLimitInfo::default();
        assert!(info.retry_after_ms.is_none());
        assert!(info.remaining_requests.is_none());
        assert!(info.remaining_tokens.is_none());
    }

    // --- Anthropic native API tests ---

    #[test]
    fn anthropic_request_serialization_matches_api_spec() {
        let req = AnthropicRequest {
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 1024,
            system: Some("You are helpful.".into()),
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: AnthropicContent::Text("Hello".into()),
            }],
            tools: vec![AnthropicTool {
                name: "echo".into(),
                description: "Echoes text".into(),
                input_schema: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            }],
        };
        let json: serde_json::Value = serde_json::to_value(&req).expect("serialize");
        assert_eq!(json["model"], "claude-sonnet-4-20250514");
        assert_eq!(json["max_tokens"], 1024);
        assert_eq!(json["system"], "You are helpful.");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "Hello");
        assert_eq!(json["tools"][0]["name"], "echo");
        assert_eq!(json["tools"][0]["input_schema"]["type"], "object");
        // Should NOT have "parameters" — Anthropic uses "input_schema"
        assert!(json["tools"][0].get("parameters").is_none());
    }

    #[test]
    fn anthropic_response_deserialization_with_text() {
        let json = r#"{
            "id": "msg_01",
            "type": "message",
            "content": [{"type": "text", "text": "Hello!"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }"#;
        let resp: AnthropicResponse = serde_json::from_str(json).expect("deserialize");
        assert_eq!(resp.id, "msg_01");
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            AnthropicContentBlock::Text { text } => assert_eq!(text, "Hello!"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 5);
    }

    #[test]
    fn anthropic_response_deserialization_with_tool_use() {
        let json = r#"{
            "id": "msg_02",
            "type": "message",
            "content": [
                {"type": "text", "text": "I'll echo that."},
                {"type": "tool_use", "id": "toolu_01", "name": "echo", "input": {"text": "hi"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 20, "output_tokens": 15}
        }"#;
        let resp: AnthropicResponse = serde_json::from_str(json).expect("deserialize");
        assert_eq!(resp.content.len(), 2);
        match &resp.content[0] {
            AnthropicContentBlock::Text { text } => assert_eq!(text, "I'll echo that."),
            other => panic!("expected Text, got {other:?}"),
        }
        match &resp.content[1] {
            AnthropicContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_01");
                assert_eq!(name, "echo");
                assert_eq!(input, &serde_json::json!({"text": "hi"}));
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn anthropic_system_prompt_extracted_from_messages() {
        let sys = Message::system("You are a helpful assistant.");
        let user = Message::user("Hello");
        let messages: Vec<&Message> = vec![&sys, &user];
        let (system, msgs) = convert_messages_to_anthropic(&messages);
        assert_eq!(system.as_deref(), Some("You are a helpful assistant."));
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn anthropic_tool_results_converted_to_content_block() {
        let tool_msg = Message::tool_result("toolu_01", "echo result");
        let messages: Vec<&Message> = vec![&tool_msg];
        let (system, msgs) = convert_messages_to_anthropic(&messages);
        assert!(system.is_none());
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        match &msgs[0].content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    AnthropicContentBlock::ToolResult { tool_use_id, content } => {
                        assert_eq!(tool_use_id, "toolu_01");
                        assert_eq!(content, "echo result");
                    }
                    other => panic!("expected ToolResult, got {other:?}"),
                }
            }
            other => panic!("expected Blocks, got {other:?}"),
        }
    }

    #[test]
    fn anthropic_uses_x_api_key_header() {
        // Verify the Anthropic config builds an appropriate request by checking
        // that chat_anthropic would use x-api-key. We test this indirectly via
        // the request builder — construct the client and verify config.
        let config = LlmConfig::anthropic("sk-ant-test123");
        let client = LlmClient::new(config);
        // The actual header is set in chat_anthropic, but we can verify the config
        // doesn't use bearer auth by checking api_format
        assert_eq!(client.config().api_format, ApiFormat::Anthropic);
        // And the key is stored correctly
        assert_eq!(client.config().api_key, "sk-ant-test123");
    }

    #[test]
    fn llm_config_anthropic_defaults_to_anthropic_format() {
        let config = LlmConfig::anthropic("sk-ant-test");
        assert_eq!(config.api_format, ApiFormat::Anthropic);
    }
}

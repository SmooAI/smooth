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
            max_tokens: 32768,
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
            max_tokens: 32768,
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
    /// Authoritative cost in USD as reported by the gateway
    /// (LiteLLM's `x-litellm-response-cost` response header).
    /// `None` when the gateway didn't report a cost — the caller
    /// falls back to local `ModelPricing` in that case.
    pub gateway_cost_usd: Option<f64>,
    /// The concrete upstream model the gateway resolved this
    /// request to, copied from the `model` field of the OpenAI- or
    /// Anthropic-shape response body. When the request was routed
    /// through a smooth-* alias (e.g. `smooth-coding`) this is the
    /// actual provider model (e.g. `qwen3-coder-flash`); when the
    /// request asked for a concrete model directly it'll just echo
    /// it back. `None` only when the response body omitted the
    /// field (rare — LiteLLM and OpenAI both populate it). Pearl
    /// th-a10c2d.
    pub resolved_model: Option<String>,
    /// Reasoning/thinking content captured from streaming
    /// `reasoning_content` / `reasoning` deltas. Pearl th-eae0f8:
    /// LiteLLM thinking-mode upstreams (DeepSeek R1, Anthropic
    /// extended thinking, OpenAI o-series) REQUIRE this be passed
    /// back on subsequent requests; failing to do so triggers a 400
    /// "reasoning_content in the thinking mode must be passed back".
    /// The agent loop copies this into the assistant `Message` it
    /// appends to the conversation, and the chat builder serializes
    /// it back into the wire request.
    pub reasoning_content: Option<String>,
}

/// Parse the gateway's authoritative cost from an HTTP response's
/// headers. Checks a few header name variants so the same parser
/// works across LiteLLM versions and other OpenAI-compat gateways
/// that echo a cost header.
pub fn parse_gateway_cost(headers: &reqwest::header::HeaderMap) -> Option<f64> {
    // LiteLLM splits cost into a few headers. `-margin-amount` is
    // what the caller actually pays (includes the gateway's
    // configured markup); `-original` is the raw upstream cost.
    // Prefer margin when present, fall back to original, then the
    // legacy `x-litellm-response-cost` shape older LiteLLM versions
    // emit. Takes the first non-zero match so a config that happens
    // to report 0 in `-margin-amount` (no markup) still surfaces
    // the underlying cost.
    const CANDIDATES: &[&str] = &[
        "x-litellm-response-cost-margin-amount",
        "x-litellm-response-cost-original",
        "x-litellm-response-cost",
        "x-response-cost",
        "x-cost-usd",
    ];
    for name in CANDIDATES {
        if let Some(v) = headers.get(*name).and_then(|h| h.to_str().ok()) {
            if let Ok(cost) = v.trim().parse::<f64>() {
                if cost > 0.0 {
                    return Some(cost);
                }
            }
        }
    }
    // All candidates were either absent or reported zero. Return
    // None so the caller falls back to local ModelPricing rather
    // than locking in $0 for the rest of the dispatch. Pearl
    // th-431ba2: LiteLLM's cost-tracking config on llm.smoo.ai
    // currently returns 0 for smooth-* aliases on every response
    // — taking that at face value pinned cost_usd at 0 across
    // every bench run even when ModelPricing could give a
    // reasonable token-count estimate.
    None
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// Subset of `prompt_tokens` that hit Anthropic's prompt cache (or the
    /// equivalent on other providers). Surfaced by OpenAI-compat gateways
    /// (LiteLLM, vLLM with prompt caching) in `usage.prompt_tokens_details.
    /// cached_tokens`. Defaults to 0 when the gateway doesn't report it.
    /// Pearl th-litellm-caching-client.
    #[serde(default)]
    pub cached_tokens: u32,
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
    /// Explicit `"auto"` when we send tools. Pearl th-67e338: small
    /// coding models (e.g. qwen3-coder-flash) sometimes default to
    /// emitting `<function=...>` XML in content when tool_choice is
    /// omitted; sending `"auto"` explicitly makes the API's tool-call
    /// path more salient for those upstreams. Skip when no tools are
    /// attached so we don't trip OpenAI-compat shims that reject
    /// the field without `tools`.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    /// Content is either a plain string (the common shape that every
    /// OpenAI-compat upstream we've tested accepts), `null` for
    /// assistant messages that only carry `tool_calls`, or — when
    /// Anthropic prompt caching is enabled (pearl
    /// th-litellm-caching-client) — an array of content blocks so we
    /// can attach `cache_control: {type: ephemeral}` to specific
    /// blocks. LiteLLM (with `cache_control_injection_points`
    /// configured) forwards Anthropic-shaped content blocks through to
    /// Anthropic and accepts them on the inbound side without choking.
    /// See the LiteLLM prompt-caching docs.
    ///
    /// We default to the plain-string form to avoid risking 400s on
    /// providers that haven't been verified with the block form
    /// (Gemini's compat shim, older LiteLLM versions). The cache-mark
    /// path opts a message into the array form.
    ///
    /// Wire-form history (string form):
    ///   - `"content": "..."`  (string) — normal prose
    ///   - `"content": null`   — explicit "no prose"
    ///   - field omitted       — also "no prose", but a foot-gun
    ///     (LiteLLM's strict deserializer rejected it as "400 missing
    ///     field content", pearl th-e8e15e).
    content: ChatContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    /// The name of the tool that produced this result. Required by Gemini's
    /// OpenAI-compat shim (it maps `role: tool` to `functionResponse`,
    /// which needs a name); ignored by OpenAI; Anthropic uses tool_use_id
    /// instead but doesn't reject the field. Sending it always is the
    /// safest serialization across providers.
    #[serde(rename = "name", skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ChatToolCall>,
    /// Reasoning content captured from the prior turn's response.
    /// Pearl th-eae0f8: LiteLLM thinking-mode upstreams REQUIRE this
    /// be replayed on assistant messages or they 400 with
    /// "reasoning_content must be passed back in the thinking mode".
    /// Omitted for non-assistant messages and when the prior turn
    /// didn't produce reasoning.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

/// Wire form for a chat message's `content` field. See `ChatMessage::content`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ChatContent {
    /// Plain string (or null) — the default for every provider.
    Text(Option<String>),
    /// Anthropic-style content blocks. Used when we need to attach
    /// `cache_control` to a specific text block. LiteLLM's
    /// `cache_control_injection_points` config forwards these through
    /// to Anthropic.
    Blocks(Vec<ChatTextBlock>),
}

/// One block in a `ChatContent::Blocks` array. We currently only emit
/// text blocks; tool_use / tool_result still ride on the top-level
/// `tool_calls` / `tool_call_id` fields. `cache_control` on a block
/// caches THAT block plus everything before it in the request.
#[derive(Debug, Serialize)]
struct ChatTextBlock {
    #[serde(rename = "type")]
    block_type: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

/// Anthropic-shaped cache-control marker. `{"type": "ephemeral"}`
/// gives Anthropic's default 5-minute TTL — what we want for the
/// agent-loop pattern (system + tools + recent history reused turn
/// after turn within a single dispatch).
#[derive(Debug, Clone, Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
}

impl CacheControl {
    const fn ephemeral() -> Self {
        Self { kind: "ephemeral" }
    }
}

impl ChatContent {
    /// Test helper: returns the plain text when the content is in the
    /// string form, or the concatenated block text when it's in the
    /// block form. Always returns `Some` for both forms (the `None`
    /// case is only the explicit-null wire shape).
    #[cfg(test)]
    fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => s.as_deref(),
            // Test paths only construct single-block content arrays
            // when verifying the cache-control wire shape; the
            // serialization assertions cover the JSON itself.
            Self::Blocks(blocks) => blocks.first().map(|b| b.text.as_str()),
        }
    }
}

#[derive(Debug, Serialize)]
struct ChatTool {
    r#type: String,
    function: ChatFunction,
    /// Anthropic prompt-cache marker. When attached to the LAST tool in
    /// the tools array (per LiteLLM's `cache_control_injection_points`
    /// pattern), Anthropic caches the entire tool-definitions block plus
    /// everything before it (the system prompt). Tools and system rarely
    /// change inside an agent run, so this is the highest-ROI cache
    /// breakpoint. Pearl th-litellm-caching-client.
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
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
    /// Concrete model the gateway resolved this request to. Both
    /// OpenAI and LiteLLM populate this; smooth-* aliases get
    /// rewritten here (e.g. `smooth-coding` → `qwen3-coder-flash`).
    /// `None` when the upstream omitted it. Pearl th-a10c2d.
    #[serde(default)]
    model: Option<String>,
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
    /// Reasoning/thinking content from LiteLLM thinking-mode upstreams.
    /// Pearl th-eae0f8. LiteLLM emits both names depending on the
    /// upstream provider; deserialize either via the alias attribute.
    #[serde(default, alias = "reasoning")]
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(clippy::struct_field_names)]
struct ChatUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    /// OpenAI-shaped cache-hit reporting nested under `usage`. Anthropic via
    /// LiteLLM, OpenAI's gpt-4o prompt caching, and a few other providers
    /// surface cache hits here. Default-None for upstreams that omit it.
    /// Pearl th-litellm-caching-client.
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
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
    /// Concrete upstream model the gateway resolved this stream to.
    /// Carries the `model` field from the SSE chunks. Pearl th-a10c2d.
    Model {
        name: String,
    },
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
    /// Concrete upstream model resolved by the gateway. Echoed by
    /// LiteLLM on every chunk; we only need it once, so the
    /// accumulator captures the first non-empty value it sees.
    /// Pearl th-a10c2d.
    #[serde(default)]
    model: Option<String>,
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
    /// Concrete model the upstream resolved this request to. Pearl
    /// th-a10c2d.
    #[serde(default)]
    model: Option<String>,
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

        let mut chat_messages: Vec<ChatMessage> = messages.iter().map(|m| to_chat_message(m)).collect();

        let mut chat_tools: Vec<ChatTool> = tools
            .iter()
            .map(|t| ChatTool {
                r#type: "function".into(),
                function: ChatFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
                cache_control: None,
            })
            .collect();

        if supports_anthropic_cache_control(&self.config.model, &self.config.api_url) {
            apply_cache_control(&mut chat_messages, &mut chat_tools);
        }

        let tool_choice = if chat_tools.is_empty() { None } else { Some("auto".to_string()) };
        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: chat_messages,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            tools: chat_tools,
            tool_choice,
        };

        let url = format!("{}/chat/completions", self.config.api_url);
        let policy = &self.config.retry_policy;

        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 0..=policy.max_retries {
            let resp = self.client.post(&url).bearer_auth(&self.config.api_key).json(&request).send().await?;

            let status = resp.status();
            let rate_limit_info = parse_rate_limit_headers(resp.headers());

            if status.is_success() {
                let gateway_cost_usd = parse_gateway_cost(resp.headers());
                let chat_resp: ChatResponse = resp.json().await?;
                let resolved_model = chat_resp.model.clone().filter(|s| !s.is_empty());
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

                let mut usage = chat_resp.usage.map_or_else(Usage::default, |u| Usage {
                    prompt_tokens: u.prompt_tokens,
                    completion_tokens: u.completion_tokens,
                    total_tokens: u.total_tokens,
                    cached_tokens: u.prompt_tokens_details.as_ref().map_or(0, |d| d.cached_tokens),
                });

                // Fallback estimation when the gateway omits usage
                // entirely (LiteLLM at llm.smoo.ai/v1 currently does
                // for smooth-* aliases — see pearl th-eff0d0).
                // ~4 chars per token is the standard OpenAI rule of
                // thumb; this isn't billing-grade but a non-zero
                // estimate is a real improvement on a hard zero.
                let content_for_estimate = choice.message.content.clone().unwrap_or_default();
                if usage.prompt_tokens == 0 && usage.completion_tokens == 0 {
                    let prompt_chars: usize = serde_json::to_string(&request).map(|s| s.len()).unwrap_or(0);
                    usage.prompt_tokens = u32::try_from(prompt_chars / 4).unwrap_or(u32::MAX);
                    usage.completion_tokens = u32::try_from(content_for_estimate.len() / 4).unwrap_or(u32::MAX);
                    usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
                    tracing::debug!(
                        prompt_tokens_est = usage.prompt_tokens,
                        completion_tokens_est = usage.completion_tokens,
                        "gateway returned no usage — estimating from char counts (pearl th-eff0d0)"
                    );
                }

                return Ok(LlmResponse {
                    content: choice.message.content.unwrap_or_default(),
                    tool_calls,
                    finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".into()),
                    usage,
                    rate_limit: Some(rate_limit_info),
                    gateway_cost_usd,
                    resolved_model,
                    reasoning_content: choice.message.reasoning_content.clone(),
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
        // Pearl th-366aa8: dispatch on api_format so Anthropic-native
        // providers go through `/v1/messages` SSE instead of the
        // OpenAI-compat `/chat/completions` shim. The shim mangles
        // Claude's native tool_use blocks, which caused Claude to
        // score 0/6 in the bench matrix.
        if let ApiFormat::Anthropic = self.config.api_format {
            return self.chat_anthropic_stream(messages, tools).await;
        }

        let mut chat_messages: Vec<ChatMessage> = messages.iter().map(|m| to_chat_message(m)).collect();

        let mut chat_tools: Vec<ChatTool> = tools
            .iter()
            .map(|t| ChatTool {
                r#type: "function".into(),
                function: ChatFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
                cache_control: None,
            })
            .collect();

        if supports_anthropic_cache_control(&self.config.model, &self.config.api_url) {
            apply_cache_control(&mut chat_messages, &mut chat_tools);
        }

        let tool_count = chat_tools.len();
        let msg_count = chat_messages.len();
        tracing::debug!(model = %self.config.model, tool_count, msg_count, "chat_stream: sending request");
        // Bench-debug instrumentation (pearl th-67e338): print the
        // tool count to stderr so a bench operator can confirm tools
        // are actually being attached to the request. Gated behind
        // SMOOTH_BENCH_TRACE_TOOLS so production runs aren't noisy.
        if std::env::var("SMOOTH_BENCH_TRACE_TOOLS").is_ok() {
            let first_tool = chat_tools.first().map(|t| t.function.name.as_str()).unwrap_or("<none>");
            eprintln!(
                "[SMOOTH_BENCH_TRACE] chat_stream: model={} tools={} first_tool={} msgs={}",
                self.config.model, tool_count, first_tool, msg_count,
            );
        }

        let tool_choice = if chat_tools.is_empty() { None } else { Some("auto".to_string()) };
        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: chat_messages,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            tools: chat_tools,
            tool_choice,
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

            // Pearl th-cb3c2a: per-stream content + per-tool-call argument
            // normalizer. Most OpenAI-compatible providers stream `delta.content`
            // as incremental deltas, but some (LiteLLM proxies of certain
            // upstreams, Azure OpenAI in some configs, several OpenRouter
            // providers) emit cumulative content per chunk — the chunk
            // contains everything-so-far rather than just the new tail.
            // Treating those as deltas produces N²-sized output ("II'll",
            // "LetLet me me first first read read", whole-paragraph repeats).
            // We track the running accumulated content per stream and convert
            // any chunk that's a strict prefix-extension of the accumulator
            // into a real delta before forwarding. True deltas pass through
            // unchanged. See `normalize_delta_against_accumulator`.
            let mut content_norm = StreamContentNormalizer::default();
            let mut tool_arg_norms: std::collections::HashMap<usize, StreamContentNormalizer> = std::collections::HashMap::new();

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
                        let event = normalize_stream_event(event, &mut content_norm, &mut tool_arg_norms);
                        if let Some(event) = event {
                            if tx.send(event).await.is_err() {
                                return; // receiver dropped
                            }
                        }
                    }
                }
            }

            // Process any remaining data in buffer
            let remaining = buffer.trim().to_string();
            if !remaining.is_empty() {
                let events = parse_sse_line(&remaining);
                for event in events {
                    let event = normalize_stream_event(event, &mut content_norm, &mut tool_arg_norms);
                    if let Some(event) = event {
                        if tx.send(event).await.is_err() {
                            return;
                        }
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
                let gateway_cost_usd = parse_gateway_cost(resp.headers());
                let anthropic_resp: AnthropicResponse = resp.json().await?;
                let resolved_model = anthropic_resp.model.clone().filter(|s| !s.is_empty());

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
                    reasoning_content: None, // Anthropic native path: reasoning lives in `thinking` blocks within content, handled differently
                    usage: Usage {
                        prompt_tokens: anthropic_resp.usage.input_tokens,
                        completion_tokens: anthropic_resp.usage.output_tokens,
                        total_tokens: total,
                        // The Anthropic native API uses different cache-hit
                        // fields (`cache_read_input_tokens`); not surfaced
                        // here because Smoo dispatches through LiteLLM
                        // (OpenAI-compat) in practice. Default-0 keeps the
                        // Anthropic-native path compiling.
                        cached_tokens: 0,
                    },
                    rate_limit: Some(rate_limit_info),
                    gateway_cost_usd,
                    resolved_model,
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

    /// Send a streaming chat completion request using the Anthropic native
    /// `/v1/messages` SSE API. Pearl th-366aa8.
    ///
    /// Anthropic SSE uses standard `event: NAME\ndata: JSON\n\n` framing.
    /// We translate each event into the `StreamEvent` shape the agent loop
    /// already consumes (Delta, Reasoning, ToolCallStart,
    /// ToolCallArgumentsDelta, Usage, Model, Done) so the downstream
    /// accumulator and tool dispatcher need no further changes.
    ///
    /// # Errors
    /// Returns error if the API call fails. Individual stream items may
    /// also contain errors for malformed event blocks.
    async fn chat_anthropic_stream(
        &self,
        messages: &[&Message],
        tools: &[ToolSchema],
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
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

        // Add `stream: true` to the request body. AnthropicRequest doesn't
        // carry the flag in its typed form (the non-streaming path doesn't
        // need it) — patch the JSON object on the wire instead.
        let mut request_body = serde_json::to_value(&request)?;
        request_body
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("serialized Anthropic request is not a JSON object"))?
            .insert("stream".into(), serde_json::Value::Bool(true));

        let url = format!("{}/messages", self.config.api_url);

        // Pearl th-ceadff: retry the initial POST on 429 / 5xx with
        // exponential backoff seeded from Anthropic's `retry-after`
        // header. Anthropic returns 429 when the per-minute token
        // budget is exceeded — bench tasks hit the lowest-tier limit
        // (30k input tokens/min) within ~2 prompts because each task
        // turn replays the full prior_history. Without retry every
        // 429 surfaces as a fatal error to the agent loop, which the
        // workflow then re-dispatches as a fresh runner — wasting
        // budget on the same retry storm. The retry happens BEFORE
        // we start consuming the bytes_stream so partial-token loss
        // is impossible.
        const MAX_RETRIES: u32 = 5;
        const BASE_BACKOFF_MS: u64 = 1_000;
        let mut attempt: u32 = 0;
        let resp = loop {
            attempt += 1;
            let resp = self
                .client
                .post(&url)
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&request_body)
                .send()
                .await
                .map_err(|e| {
                    let mut chain = vec![format!("{e}")];
                    let mut source: &dyn std::error::Error = &e;
                    while let Some(s) = source.source() {
                        chain.push(format!("{s}"));
                        source = s;
                    }
                    anyhow::anyhow!("HTTP request failed: {}", chain.join(" → "))
                })?;

            let status = resp.status();
            let retryable = status.as_u16() == 429 || (500..600).contains(&status.as_u16());
            if status.is_success() || !retryable || attempt >= MAX_RETRIES {
                break resp;
            }

            // Honor Anthropic's `retry-after` if present (seconds, integer).
            // Cap at 60s to keep individual waits bounded — repeated 429s
            // with long retry-after will exhaust MAX_RETRIES quickly and
            // surface as a normal error rather than hanging the agent.
            let header_wait_ms = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|secs| secs.saturating_mul(1_000).min(60_000));
            let backoff_ms = header_wait_ms.unwrap_or_else(|| BASE_BACKOFF_MS.saturating_mul(1u64 << (attempt - 1).min(5)));
            tracing::warn!(
                attempt,
                max_retries = MAX_RETRIES,
                status = %status,
                backoff_ms,
                "Anthropic API throttle/error — backing off and retrying"
            );
            drop(resp);
            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let req_json = serde_json::to_string_pretty(&request_body).unwrap_or_default();
            if let Some(home) = dirs_next::home_dir() {
                let dump_dir = home.join(".smooth/llm-errors");
                let _ = std::fs::create_dir_all(&dump_dir);
                let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f");
                let dump_path = dump_dir.join(format!("{ts}-anthropic-{}.json", status.as_u16()));
                let dump_contents = format!("// status={status}\n// body={body}\n{req_json}\n");
                let _ = std::fs::write(&dump_path, dump_contents);
                tracing::error!(status = %status, response_body = %body, dump = %dump_path.display(), "Anthropic stream request failed after {attempt} attempt(s) (full request dumped)");
            } else {
                tracing::error!(status = %status, response_body = %body, "Anthropic stream request failed after {attempt} attempt(s)");
            }
            anyhow::bail!("Anthropic API error {status} after {attempt} attempt(s): {body}");
        }

        let byte_stream = resp.bytes_stream();
        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<StreamEvent>>(256);

        tokio::spawn(async move {
            let mut buffer = String::new();
            let mut stream = byte_stream;
            // Track per-content-block kind so we know how to interpret
            // input_json_delta vs text_delta.
            let mut block_kinds: std::collections::HashMap<usize, AnthropicBlockKind> = std::collections::HashMap::new();
            // Track usage from message_start so we can emit a complete
            // Usage event at message_stop (message_start carries
            // input_tokens; message_delta usage carries final
            // output_tokens).
            let mut prompt_tokens: u32 = 0;
            let mut completion_tokens: u32 = 0;
            let mut stop_reason: Option<String> = None;

            const CHUNK_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

            loop {
                let chunk_result = match tokio::time::timeout(CHUNK_IDLE_TIMEOUT, stream.next()).await {
                    Ok(Some(r)) => r,
                    Ok(None) => break,
                    Err(_) => {
                        let _ = tx
                            .send(Err(anyhow::anyhow!("Anthropic stream idle timeout: no data for {CHUNK_IDLE_TIMEOUT:?}")))
                            .await;
                        return;
                    }
                };
                let chunk: Bytes = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("Anthropic stream read error: {e}"))).await;
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Anthropic SSE event blocks are separated by blank
                // lines (`\n\n`). Drain complete blocks from the buffer.
                while let Some(sep) = buffer.find("\n\n") {
                    let block = buffer[..sep].to_string();
                    buffer = buffer[sep + 2..].to_string();
                    let events = parse_anthropic_sse_block(&block, &mut block_kinds, &mut prompt_tokens, &mut completion_tokens, &mut stop_reason);
                    for ev in events {
                        if tx.send(ev).await.is_err() {
                            return;
                        }
                    }
                }
            }

            // Trailing block without final blank line — rare, but
            // handle for robustness.
            let remaining = std::mem::take(&mut buffer);
            let remaining = remaining.trim();
            if !remaining.is_empty() {
                let events = parse_anthropic_sse_block(remaining, &mut block_kinds, &mut prompt_tokens, &mut completion_tokens, &mut stop_reason);
                for ev in events {
                    if tx.send(ev).await.is_err() {
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

    /// Call the OpenAI-compatible `/v1/moderations` endpoint to classify
    /// text as safe or unsafe. Used by Safehouse Narc as a pre-filter
    /// before the LLM judge — flagged content is denied without burning
    /// judge tokens.
    ///
    /// The endpoint must live at `{api_url}/moderations` and accept
    /// OpenAI's request/response shape (LiteLLM, the Smoo AI gateway, and
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
        // route moderation through a gateway (LiteLLM / Smoo AI Gateway /
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

/// Per-stream running accumulator used to normalize cumulative-content
/// providers into true deltas. Pearl th-cb3c2a.
///
/// Some OpenAI-compatible providers (notably some LiteLLM-proxied
/// Anthropic / Vertex configurations and Azure OpenAI under certain
/// stream modes) emit `delta.content` chunks that contain the
/// **entire response so far** instead of the new tail. The agent
/// loop, the in-VM runner, and the TUI all treat each chunk as a
/// delta and `push_str` it onto the running buffer; if the provider
/// is actually sending cumulative chunks this produces quadratic
/// blowup: `"I"`, `"I'll"`, `"I'll help"` accumulates to
/// `"II'llI'll help"`, and a full 200-chunk response explodes into
/// the prose-repeats-itself-4-times pattern observed in
/// `~/.smooth/coding-sessions/*.json`.
///
/// The normalizer remembers everything it has emitted as a delta so
/// far. For each incoming chunk:
///   * If the chunk is the exact accumulated content (cumulative
///     restart, common on retry chunks), it's dropped entirely.
///   * If the chunk strictly starts with the accumulated content,
///     only the new tail is emitted as a delta.
///   * If the chunk doesn't start with the accumulated content,
///     it's treated as a normal delta (the provider is well-behaved
///     or the chunk is unrelated text like a reasoning token).
///
/// This is correct for both well-behaved (delta-emitting) providers
/// and cumulative-emitting providers: a delta chunk never starts with
/// the full accumulated buffer (it would have to be longer than the
/// buffer for that to be coherent), so the normalizer reduces to a
/// no-op `push_str` on well-behaved streams.
#[derive(Default, Debug)]
struct StreamContentNormalizer {
    accumulated: String,
}

impl StreamContentNormalizer {
    /// Normalize an incoming chunk against the running accumulator.
    /// Returns the actual delta to emit, or `None` to drop the chunk.
    fn normalize<'a>(&mut self, chunk: &'a str) -> Option<&'a str> {
        if chunk.is_empty() {
            return None;
        }
        if self.accumulated.is_empty() {
            self.accumulated.push_str(chunk);
            return Some(chunk);
        }
        // Cumulative restart — chunk is exactly what we already emitted.
        if chunk == self.accumulated.as_str() {
            return None;
        }
        // Cumulative extension — chunk = accumulated + new tail.
        if chunk.len() > self.accumulated.len() && chunk.starts_with(self.accumulated.as_str()) {
            let tail = &chunk[self.accumulated.len()..];
            self.accumulated.push_str(tail);
            return Some(tail);
        }
        // True delta (well-behaved provider). Just append.
        self.accumulated.push_str(chunk);
        Some(chunk)
    }
}

/// Run a `StreamEvent` through the per-stream normalizers. Returns the
/// possibly-rewritten event or `None` when the event collapsed to empty
/// (e.g. a cumulative-restart chunk that adds nothing new).
fn normalize_stream_event(
    event: anyhow::Result<StreamEvent>,
    content_norm: &mut StreamContentNormalizer,
    tool_arg_norms: &mut std::collections::HashMap<usize, StreamContentNormalizer>,
) -> Option<anyhow::Result<StreamEvent>> {
    match event {
        Ok(StreamEvent::Delta { content }) => {
            let normalized = content_norm.normalize(&content)?;
            Some(Ok(StreamEvent::Delta {
                content: normalized.to_string(),
            }))
        }
        Ok(StreamEvent::ToolCallArgumentsDelta { index, arguments_chunk }) => {
            let norm = tool_arg_norms.entry(index).or_default();
            let normalized = norm.normalize(&arguments_chunk)?;
            Some(Ok(StreamEvent::ToolCallArgumentsDelta {
                index,
                arguments_chunk: normalized.to_string(),
            }))
        }
        // Reasoning, tool call starts, model, usage, done, errors — pass through.
        other => Some(other),
    }
}

/// Parse a single SSE line into zero or more `StreamEvent`s.
///
/// Per-content-block kind for an Anthropic streaming response. Pearl
/// th-366aa8. Anthropic SSE emits `content_block_start` once per block
/// with the block type (text, tool_use, thinking), then a sequence of
/// `content_block_delta` events keyed by block index. We need to
/// remember the type so we know how to interpret each delta:
/// `text_delta` for text blocks, `input_json_delta` for tool_use,
/// `thinking_delta` for thinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnthropicBlockKind {
    Text,
    ToolUse,
    Thinking,
    /// Server text+image blocks (rare for chat completions) or
    /// future types we haven't taught the parser yet. Deltas on
    /// these blocks are dropped with a tracing::debug! so the
    /// stream keeps flowing.
    Unknown,
}

/// Parse one Anthropic SSE event block — the text between two
/// `\n\n` separators — into zero or more `StreamEvent`s.
///
/// Anthropic SSE shape (one block):
/// ```text
/// event: content_block_delta
/// data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}
/// ```
///
/// We ignore the `event:` line (the `data:` JSON has its own `type`
/// field) and parse the `data:` payload. Multi-line `data:` (Anthropic
/// doesn't currently use this, but the SSE spec allows it) is joined
/// with `\n` before JSON parse.
///
/// The mutable state tracks cross-block context: per-block kinds
/// (set on `content_block_start`, read on `content_block_delta`),
/// usage tokens (input from `message_start`, output from
/// `message_delta.usage`), and the final stop reason (from
/// `message_delta.delta.stop_reason`). On `message_stop` we emit a
/// `Usage` event with the accumulated counts plus a `Done` event
/// carrying the stop reason — symmetric to how the OpenAI SSE path
/// emits `Done` at `[DONE]`.
fn parse_anthropic_sse_block(
    block: &str,
    block_kinds: &mut std::collections::HashMap<usize, AnthropicBlockKind>,
    prompt_tokens: &mut u32,
    completion_tokens: &mut u32,
    stop_reason: &mut Option<String>,
) -> Vec<anyhow::Result<StreamEvent>> {
    // Concatenate all `data:` lines in the block (per SSE spec; in
    // practice Anthropic emits one `data:` line per block).
    let mut data_lines: Vec<&str> = Vec::new();
    for line in block.lines() {
        let line = line.trim_start();
        if line.starts_with(':') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
        // `event:` lines are decorative — the JSON has the type.
    }
    if data_lines.is_empty() {
        return vec![];
    }
    let payload = data_lines.join("\n");

    let value: serde_json::Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(e) => return vec![Err(anyhow::anyhow!("anthropic sse: parse data payload: {e} (payload={payload:.200?})"))],
    };
    let ty = value.get("type").and_then(|v| v.as_str()).unwrap_or("");

    let mut out: Vec<anyhow::Result<StreamEvent>> = Vec::new();
    match ty {
        "message_start" => {
            if let Some(msg) = value.get("message") {
                if let Some(model) = msg.get("model").and_then(|v| v.as_str()) {
                    out.push(Ok(StreamEvent::Model { name: model.to_string() }));
                }
                if let Some(u) = msg.get("usage") {
                    if let Some(n) = u.get("input_tokens").and_then(serde_json::Value::as_u64) {
                        *prompt_tokens = u32::try_from(n).unwrap_or(u32::MAX);
                    }
                    if let Some(n) = u.get("output_tokens").and_then(serde_json::Value::as_u64) {
                        *completion_tokens = u32::try_from(n).unwrap_or(u32::MAX);
                    }
                }
            }
        }
        "content_block_start" => {
            let idx = value.get("index").and_then(serde_json::Value::as_u64).unwrap_or(0) as usize;
            let cb = value.get("content_block").cloned().unwrap_or_default();
            let cb_type = cb.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let kind = match cb_type {
                "text" => AnthropicBlockKind::Text,
                "tool_use" => AnthropicBlockKind::ToolUse,
                "thinking" | "redacted_thinking" => AnthropicBlockKind::Thinking,
                _ => AnthropicBlockKind::Unknown,
            };
            block_kinds.insert(idx, kind);
            if kind == AnthropicBlockKind::ToolUse {
                let id = cb.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let name = cb.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                out.push(Ok(StreamEvent::ToolCallStart { index: idx, id, name }));
            }
        }
        "content_block_delta" => {
            let idx = value.get("index").and_then(serde_json::Value::as_u64).unwrap_or(0) as usize;
            let kind = block_kinds.get(&idx).copied().unwrap_or(AnthropicBlockKind::Unknown);
            let delta = value.get("delta").cloned().unwrap_or_default();
            let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match (kind, delta_type) {
                (AnthropicBlockKind::Text, "text_delta") => {
                    if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            out.push(Ok(StreamEvent::Delta { content: text.to_string() }));
                        }
                    }
                }
                (AnthropicBlockKind::ToolUse, "input_json_delta") => {
                    if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                        out.push(Ok(StreamEvent::ToolCallArgumentsDelta {
                            index: idx,
                            arguments_chunk: partial.to_string(),
                        }));
                    }
                }
                (AnthropicBlockKind::Thinking, "thinking_delta") => {
                    if let Some(text) = delta.get("thinking").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            out.push(Ok(StreamEvent::Reasoning { content: text.to_string() }));
                        }
                    }
                }
                _ => {
                    // Unknown (block_kind, delta_type) pair — log and
                    // skip rather than failing the stream.
                    tracing::debug!(?kind, delta_type, "anthropic sse: skipping unknown delta");
                }
            }
        }
        "content_block_stop" => {
            // No-op: the agent loop accumulates per-block content
            // until message_stop.
        }
        "message_delta" => {
            if let Some(d) = value.get("delta") {
                if let Some(r) = d.get("stop_reason").and_then(|v| v.as_str()) {
                    *stop_reason = Some(r.to_string());
                }
            }
            if let Some(u) = value.get("usage") {
                // Anthropic's `message_delta.usage` carries only the
                // running output_tokens (the input was emitted at
                // message_start). Take the latest.
                if let Some(n) = u.get("output_tokens").and_then(serde_json::Value::as_u64) {
                    *completion_tokens = u32::try_from(n).unwrap_or(u32::MAX);
                }
            }
        }
        "message_stop" => {
            // Flush usage + done at end of message. The OpenAI SSE
            // path emits these from `[DONE]` + the final usage chunk;
            // Anthropic separates them.
            let total = prompt_tokens.saturating_add(*completion_tokens);
            out.push(Ok(StreamEvent::Usage(Usage {
                prompt_tokens: *prompt_tokens,
                completion_tokens: *completion_tokens,
                total_tokens: total,
                cached_tokens: 0,
            })));
            let reason = stop_reason.clone().unwrap_or_else(|| "stop".to_string());
            // Anthropic's stop_reason vocabulary is "end_turn",
            // "max_tokens", "stop_sequence", "tool_use". Normalize
            // "end_turn" → "stop" so downstream gates that key on the
            // OpenAI vocabulary keep working. Pass others through.
            let normalized = if reason == "end_turn" { "stop".to_string() } else { reason };
            out.push(Ok(StreamEvent::Done { finish_reason: normalized }));
        }
        "ping" => {
            // Heartbeat — drop.
        }
        "error" => {
            let msg = value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("anthropic stream returned `error` event");
            out.push(Err(anyhow::anyhow!("anthropic stream error: {msg}")));
        }
        other => {
            tracing::debug!(event_type = other, "anthropic sse: skipping unknown event type");
        }
    }
    out
}

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

    // Surface the resolved upstream model (LiteLLM / OpenAI both
    // populate `model` on every chunk). The accumulator only keeps
    // the first non-empty value, so emitting on every chunk is fine
    // — duplicates collapse downstream. Pearl th-a10c2d.
    if let Some(model) = chunk.model.as_deref() {
        if !model.is_empty() {
            events.push(Ok(StreamEvent::Model { name: model.to_string() }));
        }
    }

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
            cached_tokens: usage.prompt_tokens_details.as_ref().map_or(0, |d| d.cached_tokens),
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
    let mut reasoning = String::new();
    let mut finish_reason = String::from("stop");
    let mut usage = Usage::default();
    let mut resolved_model: Option<String> = None;

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
            StreamEvent::Reasoning { content: delta } => {
                // Pearl th-eae0f8: ACCUMULATE reasoning into a separate
                // buffer so we can pass it back on the next turn.
                // LiteLLM thinking-mode upstreams (DeepSeek R1, etc.)
                // 400 us with "reasoning_content must be passed back"
                // when this is missing on the assistant message replay.
                // It does NOT go into `content` — that stays
                // strictly the model's final answer text.
                reasoning.push_str(&delta);
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
            StreamEvent::Model { name } => {
                // Capture the first non-empty model name and ignore
                // subsequent ones — LiteLLM echoes it on every chunk.
                if resolved_model.is_none() && !name.is_empty() {
                    resolved_model = Some(name);
                }
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
            // Fall back to an EMPTY OBJECT, not Null. When we echo the
            // assistant turn back on the next LLM call, the arguments
            // field must serialize to valid JSON object content —
            // strict providers (qwen3-coder-plus via DashScope) reject
            // `arguments: "null"` with "must be in JSON format".
            let arguments: serde_json::Value = serde_json::from_str(&args_str).unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
            Some(ToolCall { id, name, arguments })
        })
        .collect();

    // Pearl th-c65ca3: qwen3-coder models (DashScope upstream behind the
    // `smooth-coding` alias) sometimes emit their native Qwen tool-call
    // XML as `content` instead of OpenAI-style `tool_calls`. The
    // OpenAI-compat shim at DashScope is supposed to translate this,
    // but for some prompts it falls through. Symptom: the user sees a
    // raw `<function=NAME><parameter=K> V</tool_call>` block in chat
    // and the agent's tool didn't actually run. Recovery: when the
    // accumulated content contains pseudo-XML AND no native tool_calls
    // came through, parse the XML into synthetic ToolCalls and blank
    // the content. The agent loop then executes them like real calls
    // so the next turn has real tool-result history to reason about.
    let (content, tool_calls) = if tool_calls.is_empty() && content_has_pseudo_tool_xml(&content) {
        let parsed = parse_pseudo_tool_xml(&content);
        if parsed.is_empty() {
            (content, tool_calls)
        } else {
            tracing::warn!(
                count = parsed.len(),
                "recovered {} pseudo-XML tool call(s) from content (pearl th-c65ca3)",
                parsed.len()
            );
            // Blank the content so it doesn't surface as raw XML in
            // the next turn's prior_messages; the synthesized tool
            // calls carry the real intent.
            (String::new(), parsed)
        }
    } else {
        (content, tool_calls)
    };

    // Streaming counterpart to the chat() path's fallback: if no
    // StreamEvent::Usage ever arrived (LiteLLM at llm.smoo.ai/v1
    // currently drops it for smooth-* aliases), estimate from
    // content lengths so cost_tracker has something to multiply.
    // Pearl th-eff0d0.
    if usage.prompt_tokens == 0 && usage.completion_tokens == 0 {
        // Streaming path doesn't have the outgoing request in
        // scope; estimate prompt_tokens at zero and only
        // capture completion (~4 chars/token rule of thumb).
        // The caller's record path will still see non-zero
        // tokens and produce a real cost number against
        // ModelPricing.
        let total_args_chars: usize = tool_calls.iter().map(|tc| tc.arguments.to_string().len()).sum();
        let completion_chars = content.len() + total_args_chars;
        usage.completion_tokens = u32::try_from(completion_chars / 4).unwrap_or(u32::MAX);
        usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
    }

    Ok(LlmResponse {
        content,
        tool_calls,
        finish_reason,
        usage,
        rate_limit: None,
        gateway_cost_usd: None,
        resolved_model,
        reasoning_content: if reasoning.is_empty() { None } else { Some(reasoning) },
    })
}

/// True iff `content` contains either the Qwen `<tool_call>…</tool_call>`
/// wrapper or the malformed `<function=…>` / `<parameter=…>` variant.
fn content_has_pseudo_tool_xml(content: &str) -> bool {
    content.contains("<function=") || (content.contains("<tool_call>") && content.contains("</tool_call>"))
}

/// Parse Qwen-style pseudo-XML tool calls out of `content`. Handles two
/// shapes both emitted by qwen3-coder under DashScope's OpenAI-compat
/// shim (pearl th-c65ca3):
///
/// 1. Canonical wrapper — JSON body inside `<tool_call>…</tool_call>`:
///    ```text
///    <tool_call>
///    {"name": "host_tool", "arguments": {"tool": "curl", "args": [...]}}
///    </tool_call>
///    ```
///
/// 2. Malformed inline — name in opening tag, args as `<parameter=K> V`
///    chunks, closed by `</tool_call>`:
///    ```text
///    <function=host_tool> <parameter=tool> curl <parameter=args> ["-I", "https://x.com"] </tool_call>
///    ```
///    Each `<parameter=K> V` element becomes one key in arguments; the
///    value is the text from after the `>` to the start of the next
///    `<parameter=` or `</tool_call>`, trimmed. JSON-looking values
///    (lists, objects, booleans, numbers) are parsed; everything else
///    stays as a string.
///
/// Returns an empty vec if nothing parses cleanly — caller should leave
/// the original content alone in that case.
fn parse_pseudo_tool_xml(content: &str) -> Vec<crate::tool::ToolCall> {
    let mut out: Vec<crate::tool::ToolCall> = Vec::new();
    let mut rest = content;
    let mut counter: u64 = 0;
    while !rest.is_empty() {
        // Pick whichever opener comes first.
        let func_idx = rest.find("<function=");
        let canon_idx = rest.find("<tool_call>");
        let (start, is_canon) = match (func_idx, canon_idx) {
            (Some(f), Some(c)) if c < f => (c, true),
            (Some(f), _) => (f, false),
            (None, Some(c)) => (c, true),
            (None, None) => break,
        };
        rest = &rest[start..];
        if is_canon {
            let Some(close) = rest.find("</tool_call>") else {
                break;
            };
            let body = rest["<tool_call>".len()..close].trim();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                if !name.is_empty() {
                    let arguments = v.get("arguments").cloned().unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                    counter += 1;
                    out.push(crate::tool::ToolCall {
                        id: format!("xml_recovered_{counter}"),
                        name,
                        arguments,
                    });
                }
            }
            rest = &rest[close + "</tool_call>".len()..];
        } else {
            // `<function=NAME>` opener. Find the closing `>` of THIS tag.
            let Some(name_end) = rest[1..].find('>') else {
                break;
            };
            let name = rest["<function=".len()..1 + name_end].trim().to_string();
            // Body runs from after this `>` to `</tool_call>` or, if
            // absent, to the next `<function=` (the model sometimes
            // omits the closer when chaining calls).
            let body_start = 1 + name_end + 1;
            let tail = &rest[body_start..];
            let body_end_rel = tail.find("</tool_call>").or_else(|| tail.find("<function=")).unwrap_or(tail.len());
            let body = &tail[..body_end_rel];
            let mut arguments = serde_json::Map::new();
            // Walk `<parameter=K> V` chunks.
            let mut cursor = body;
            while let Some(p_idx) = cursor.find("<parameter=") {
                cursor = &cursor[p_idx..];
                let Some(k_end) = cursor[1..].find('>') else {
                    break;
                };
                let key = cursor["<parameter=".len()..1 + k_end].trim().to_string();
                let value_start = 1 + k_end + 1;
                let after = &cursor[value_start..];
                let value_end = after.find("<parameter=").unwrap_or(after.len());
                let raw_value = after[..value_end].trim();
                let parsed: serde_json::Value = serde_json::from_str(raw_value).unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()));
                if !key.is_empty() {
                    arguments.insert(key, parsed);
                }
                cursor = &after[value_end..];
            }
            if !name.is_empty() {
                counter += 1;
                out.push(crate::tool::ToolCall {
                    id: format!("xml_recovered_{counter}"),
                    name,
                    arguments: serde_json::Value::Object(arguments),
                });
            }
            // Advance past the closing tag if present, else past this opener.
            let advance = body_start
                + body_end_rel
                + if tail[body_end_rel..].starts_with("</tool_call>") {
                    "</tool_call>".len()
                } else {
                    0
                };
            rest = &rest[advance..];
        }
    }
    out
}

/// Serialize tool-call arguments to the canonical OpenAI wire
/// format: a JSON-string whose content is a JSON object (never
/// `"null"`, never an empty string, never a non-object primitive).
///
/// Strict providers — qwen3-coder-plus on DashScope for one — reject
/// `function.arguments` unless it parses as a JSON object. Our
/// streaming path used to fall back to `Value::Null` when argument
/// deltas arrived malformed; echoing that back produced
/// `arguments: "null"` and qwen3 would trip with
/// `InternalError.Algo.InvalidParameter: The "function.arguments"
/// parameter of the code model must be in JSON format.`
///
/// This function is the single gate for the wire format: it coerces
/// anything non-object to `"{}"`, which every OpenAI-compat provider
/// accepts. Keep it strict — the cost of being lenient here is
/// per-provider wire errors at runtime.
pub fn canonical_tool_arguments_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(_) => serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
        _ => "{}".to_string(),
    }
}

/// Decide whether the configured upstream understands Anthropic-shaped
/// `cache_control` markers. We send them when:
///   - the model id looks Claude-ish (`claude-`, `sonnet`, `opus`, `haiku`),
///     OR
///   - the model is one of our LiteLLM gateway aliases (`smooth-coding-claude`,
///     `smooth-thinking`, etc. that route to Claude), OR
///   - the api_base looks like our LiteLLM gateway (`llm.smoo.ai`, `litellm`)
///     or anthropic.* directly.
///
/// We deliberately do NOT send cache_control to bare OpenAI / Gemini /
/// Groq endpoints — they 400 on unknown extension fields. The LiteLLM
/// gateway's `cache_control_injection_points` config is what actually
/// passes the markers through to Anthropic; without that gateway-side
/// change this code is a no-op.
fn supports_anthropic_cache_control(model: &str, api_url: &str) -> bool {
    let model_lower = model.to_ascii_lowercase();
    let url_lower = api_url.to_ascii_lowercase();
    let model_looks_claude = model_lower.contains("claude") || model_lower.contains("sonnet") || model_lower.contains("opus") || model_lower.contains("haiku");
    // Smooth's LiteLLM aliases that we know route to Claude. The
    // generic `smooth-` prefix alone isn't enough — `smooth-fast`
    // routes to Groq/Llama, which would 400 on cache_control.
    let model_is_smooth_claude_alias = model_lower.starts_with("smooth-coding")
        || model_lower.starts_with("smooth-thinking")
        || model_lower.starts_with("smooth-planning")
        || model_lower.starts_with("smooth-reviewing");
    let url_is_litellm = url_lower.contains("litellm") || url_lower.contains("llm.smoo.ai");
    let url_is_anthropic = url_lower.contains("anthropic.");
    (model_looks_claude || model_is_smooth_claude_alias) && (url_is_litellm || url_is_anthropic)
}

/// Attach `cache_control: ephemeral` to the strategic prefix boundaries
/// so Anthropic's prompt cache covers what changes least:
///   1. The (last) system message — caches the system prompt.
///   2. The last tool definition — caches the tool block + system prefix
///      ahead of it. This is the highest-ROI breakpoint: tools rarely
///      change inside a dispatch but are large (the whole tool registry
///      schema lives here).
///   3. The last message in history — caches the running conversation
///      so each turn within a 5-minute window pays only for the new
///      delta. Skipping this gets you cache hits on system+tools only;
///      including it gets you turn-by-turn savings too.
///
/// Per Anthropic's docs, marking a block caches THAT block plus
/// everything before it. We only need cache_control on the last block
/// of each prefix we want to reuse.
fn apply_cache_control(messages: &mut [ChatMessage], tools: &mut [ChatTool]) {
    // 1. Mark the last system message — its content gets rewritten
    //    into the Blocks form with cache_control on the (single) text
    //    block.
    if let Some(sys) = messages.iter_mut().rfind(|m| m.role == "system") {
        sys.content = wrap_with_cache_control(&sys.content);
    }

    // 2. Mark the last tool — cache_control here covers the entire
    //    tools array plus the system prefix.
    if let Some(last_tool) = tools.last_mut() {
        last_tool.cache_control = Some(CacheControl::ephemeral());
    }

    // 3. Mark the last message in history so turn-by-turn caching
    //    extends. Skip when the only message is the system we already
    //    marked (avoid double-marking the same block).
    let last_idx = messages.len().saturating_sub(1);
    if messages.len() > 1 {
        if let Some(last) = messages.get_mut(last_idx) {
            // Tool-result messages don't currently get block-form
            // content (the result text is the whole payload); mark
            // them by wrapping the content too.
            last.content = wrap_with_cache_control(&last.content);
        }
    }
}

/// Convert a `ChatContent` into the block form with the (single) text
/// block carrying `cache_control: ephemeral`. Tool-call-only messages
/// (content == None) stay as `Text(None)` — there's nothing to cache
/// on them, and the marker on the LAST block before the assistant turn
/// already covers the prefix.
fn wrap_with_cache_control(existing: &ChatContent) -> ChatContent {
    let text = match existing {
        ChatContent::Text(Some(s)) => s.clone(),
        ChatContent::Text(None) => return ChatContent::Text(None),
        ChatContent::Blocks(blocks) => {
            // Already in block form (re-marking case). Re-emit with
            // cache_control on the last block.
            let mut new_blocks: Vec<ChatTextBlock> = blocks
                .iter()
                .map(|b| ChatTextBlock {
                    block_type: b.block_type,
                    text: b.text.clone(),
                    cache_control: None,
                })
                .collect();
            if let Some(last) = new_blocks.last_mut() {
                last.cache_control = Some(CacheControl::ephemeral());
            }
            return ChatContent::Blocks(new_blocks);
        }
    };
    ChatContent::Blocks(vec![ChatTextBlock {
        block_type: "text",
        text,
        cache_control: Some(CacheControl::ephemeral()),
    }])
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
                arguments: canonical_tool_arguments_json(&tc.arguments),
            },
        })
        .collect();

    // Always send content as an explicit string (possibly empty) — never
    // None/null/omitted. Three iterations on this:
    //   1. Original: `Some("")` — failed against some compat shims that
    //      rejected empty content alongside tool_calls.
    //   2. th-e8e15e: `None` (omit) — failed against LiteLLM's strict
    //      deserializer with "400 missing field content".
    //   3. th-a0ed23: `Some("")` (this attempt) — empty string, key
    //      always present. OpenAI / Anthropic-compat / Gemini-compat /
    //      LiteLLM all accept this shape; the providers that historically
    //      rejected empty-string content seem to have relaxed since.
    let content = ChatContent::Text(Some(msg.content.clone()));

    // Tool-result messages must carry the originating tool's `name` so that
    // strict OpenAI-compat upstreams (notably Gemini, when the gateway
    // translates `role: tool` into a Gemini `functionResponse`) can pair the
    // result with the previous `functionCall`. Anthropic infers the tool
    // from the `tool_use_id` already; OpenAI itself ignores `name` on tool
    // messages but accepts it. Sending it always is the safest serialization.
    //
    // The name is recovered from the matching prior assistant tool_call's
    // name, which the conversation pairs in `Message::tool_result_named`.
    // Falls back to None for legacy callers that didn't set it — those
    // continue to work against OpenAI/Anthropic but may surprise Gemini.
    let tool_name = if msg.role == Role::Tool { msg.tool_name.clone() } else { None };

    ChatMessage {
        role: role.into(),
        content,
        tool_call_id: msg.tool_call_id.clone(),
        tool_name,
        tool_calls,
        reasoning_content: if msg.role == Role::Assistant { msg.reasoning_content.clone() } else { None },
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
        assert_eq!(chat.content.as_text(), Some("Hello"));
        assert!(chat.tool_call_id.is_none());
    }

    #[test]
    fn to_chat_message_assistant_with_tool_calls_emits_empty_string_content() {
        // Pearl th-a0ed23: LiteLLM's strict deserializer rejects BOTH
        // omitted content AND `content: null` for assistant messages
        // that carry tool_calls. We send an explicit empty string
        // instead — the field is always present, always a string.
        let mut msg = Message::assistant("");
        msg.tool_calls.push(ToolCall {
            id: "c1".into(),
            name: "foo".into(),
            arguments: serde_json::json!({}),
        });
        let chat = to_chat_message(&msg);
        assert_eq!(chat.content.as_text(), Some(""), "empty content must be Some(\"\"), not None");
        assert_eq!(chat.tool_calls.len(), 1);

        // Critical wire-format assertion: JSON must contain
        // `"content":""` — never `null`, never omitted.
        let json = serde_json::to_string(&chat).expect("serialize");
        assert!(
            json.contains(r#""content":"""#),
            "JSON must serialize empty content as empty string, got: {json}"
        );
        assert!(!json.contains(r#""content":null"#), "JSON must not contain content:null: {json}");

        // Non-empty content should still be passed through
        let mut msg2 = Message::assistant("I'll call a tool.");
        msg2.tool_calls.push(ToolCall {
            id: "c2".into(),
            name: "foo".into(),
            arguments: serde_json::json!({}),
        });
        let chat2 = to_chat_message(&msg2);
        assert_eq!(chat2.content.as_text(), Some("I'll call a tool."));
    }

    #[test]
    fn to_chat_message_tool_result_always_has_string_content() {
        // Tool-result messages always carry the originating tool's
        // output as content. Even an empty result must serialize as
        // `"content": ""` so LiteLLM's deserializer is happy.
        let msg = Message::tool_result("call-1", "");
        let chat = to_chat_message(&msg);
        assert_eq!(chat.content.as_text(), Some(""), "empty tool result must be Some(\"\"), not None");
        let json = serde_json::to_string(&chat).expect("serialize");
        assert!(
            json.contains(r#""content":"""#),
            "tool result JSON must serialize empty content as empty string: {json}"
        );
    }

    #[test]
    fn to_chat_message_tool() {
        let msg = Message::tool_result("call-1", "result");
        let chat = to_chat_message(&msg);
        assert_eq!(chat.role, "tool");
        assert_eq!(chat.tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn tool_result_named_carries_name_through_serialization() {
        // Defensive Gemini-compat: when the OpenAI-compat path goes through
        // a gateway that translates `role: tool` to a Gemini
        // `functionResponse`, the result must include the originating
        // tool's name. `Message::tool_result_named` carries it; verify the
        // ChatMessage serialization preserves it (via the `name` JSON
        // field) and skips it when absent so legacy callers don't see a
        // null name on the wire.
        let named = Message::tool_result_named("call-7", "get_weather", "sunny, 22C");
        let chat = to_chat_message(&named);
        assert_eq!(chat.role, "tool");
        assert_eq!(chat.tool_call_id.as_deref(), Some("call-7"));
        assert_eq!(chat.tool_name.as_deref(), Some("get_weather"));
        let json = serde_json::to_string(&chat).expect("serialize");
        assert!(json.contains(r#""name":"get_weather""#), "json={json}");

        // Legacy `tool_result` without a name still works — the name field
        // is omitted from the JSON entirely (skip_serializing_if).
        let legacy = Message::tool_result("call-8", "result");
        let chat = to_chat_message(&legacy);
        assert!(chat.tool_name.is_none());
        let json = serde_json::to_string(&chat).expect("serialize");
        assert!(!json.contains(r#""name""#), "legacy must not emit name field: {json}");
    }

    #[test]
    fn chat_request_serialization() {
        let req = ChatRequest {
            model: "test-model".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: ChatContent::Text(Some("hello".into())),
                tool_call_id: None,
                tool_name: None,
                tool_calls: vec![],
                reasoning_content: None,
            }],
            max_tokens: 100,
            temperature: 0.0,
            tools: vec![],
            tool_choice: None,
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

    #[test]
    fn chat_response_captures_resolved_model() {
        // Pearl th-a10c2d: LiteLLM rewrites smooth-* aliases to a
        // concrete upstream and echoes it in the response's top-level
        // `model` field. Confirm the parser picks it up so the agent
        // can surface `smooth-coding → qwen3-coder-flash` to the TUI.
        let json = r#"{
            "choices": [{
                "message": {"content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6},
            "model": "qwen3-coder-flash"
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).expect("deserialize");
        assert_eq!(resp.model.as_deref(), Some("qwen3-coder-flash"));
    }

    #[test]
    fn chat_response_model_absent_is_none() {
        // Defensive: providers that omit `model` (rare but possible)
        // must still deserialize cleanly. Pearl th-a10c2d.
        let json = r#"{
            "choices": [{
                "message": {"content": "ok"},
                "finish_reason": "stop"
            }]
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).expect("deserialize");
        assert!(resp.model.is_none());
    }

    #[tokio::test]
    async fn accumulate_stream_captures_resolved_model_once() {
        // Pearl th-a10c2d: when LiteLLM streams an aliased request it
        // echoes the resolved model on every chunk. The accumulator
        // should capture the FIRST non-empty value and ignore later
        // duplicates so consumers see a single canonical name.
        use futures_util::stream;

        let events: Vec<anyhow::Result<StreamEvent>> = vec![
            Ok(StreamEvent::Model {
                name: "qwen3-coder-flash".into(),
            }),
            Ok(StreamEvent::Delta { content: "hello".into() }),
            // Later "Model" event with a different upstream — must be ignored
            // so we don't flap mid-turn.
            Ok(StreamEvent::Model {
                name: "should-not-clobber".into(),
            }),
            Ok(StreamEvent::Done { finish_reason: "stop".into() }),
        ];
        let stream: Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>> = Box::pin(stream::iter(events));
        let resp = accumulate_stream_events(stream).await.expect("accumulate");
        assert_eq!(resp.resolved_model.as_deref(), Some("qwen3-coder-flash"));
        assert_eq!(resp.content, "hello");
    }

    #[tokio::test]
    async fn accumulate_stream_no_model_event_returns_none() {
        // Older providers that don't populate `model` on stream chunks
        // shouldn't synthesize one. Pearl th-a10c2d.
        use futures_util::stream;

        let events: Vec<anyhow::Result<StreamEvent>> = vec![
            Ok(StreamEvent::Delta { content: "hi".into() }),
            Ok(StreamEvent::Done { finish_reason: "stop".into() }),
        ];
        let stream: Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>> = Box::pin(stream::iter(events));
        let resp = accumulate_stream_events(stream).await.expect("accumulate");
        assert!(resp.resolved_model.is_none());
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

    // ─── Pearl th-366aa8: Anthropic-native SSE parser ──────────────

    fn run_anth_blocks(blocks: &[&str]) -> Vec<StreamEvent> {
        let mut kinds = std::collections::HashMap::new();
        let mut pt: u32 = 0;
        let mut ct: u32 = 0;
        let mut sr: Option<String> = None;
        let mut out: Vec<StreamEvent> = Vec::new();
        for b in blocks {
            for ev in parse_anthropic_sse_block(b, &mut kinds, &mut pt, &mut ct, &mut sr) {
                out.push(ev.expect("ok"));
            }
        }
        out
    }

    #[test]
    fn parse_anthropic_message_start_emits_model_and_seeds_usage() {
        let blocks = ["event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-5\",\"usage\":{\"input_tokens\":42,\"output_tokens\":0}}}"];
        let events = run_anth_blocks(&blocks);
        assert!(matches!(events.first(), Some(StreamEvent::Model { name }) if name == "claude-sonnet-4-5"));
    }

    #[test]
    fn parse_anthropic_text_delta_emits_delta() {
        let blocks = [
            r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello, world!"}}"#,
        ];
        let events = run_anth_blocks(&blocks);
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Delta { content } if content == "Hello, world!")));
    }

    #[test]
    fn parse_anthropic_tool_use_emits_start_and_args_delta() {
        let blocks = [
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_42","name":"read_file","input":{}}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"INSTRUCTIONS.md\"}"}}"#,
        ];
        let events = run_anth_blocks(&blocks);
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCallStart { index, id, name } if *index == 1 && id == "toolu_42" && name == "read_file")));
        let chunks: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCallArgumentsDelta { index, arguments_chunk } if *index == 1 => Some(arguments_chunk.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(chunks.concat(), "{\"path\":\"INSTRUCTIONS.md\"}");
    }

    #[test]
    fn parse_anthropic_thinking_delta_emits_reasoning() {
        let blocks = [
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"let me approach this"}}"#,
        ];
        let events = run_anth_blocks(&blocks);
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Reasoning { content } if content == "let me approach this")));
    }

    #[test]
    fn parse_anthropic_message_stop_emits_usage_and_done() {
        let blocks = [
            r#"data: {"type":"message_start","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":25}}"#,
            r#"data: {"type":"message_stop"}"#,
        ];
        let events = run_anth_blocks(&blocks);
        let usage = events
            .iter()
            .find_map(|e| if let StreamEvent::Usage(u) = e { Some(u.clone()) } else { None })
            .expect("usage");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 25);
        assert_eq!(usage.total_tokens, 35);
        let done_reason = events.iter().find_map(|e| {
            if let StreamEvent::Done { finish_reason } = e {
                Some(finish_reason.clone())
            } else {
                None
            }
        });
        assert_eq!(done_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn parse_anthropic_end_turn_normalizes_to_stop() {
        let blocks = [
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            r#"data: {"type":"message_stop"}"#,
        ];
        let events = run_anth_blocks(&blocks);
        let done_reason = events.iter().find_map(|e| {
            if let StreamEvent::Done { finish_reason } = e {
                Some(finish_reason.clone())
            } else {
                None
            }
        });
        // Anthropic's "end_turn" → OpenAI vocab "stop" so downstream
        // gates that key on the string don't have to learn two vocabs.
        assert_eq!(done_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn parse_anthropic_error_event_propagates() {
        let block = r#"data: {"type":"error","error":{"type":"overloaded_error","message":"upstream overloaded"}}"#;
        let mut kinds = std::collections::HashMap::new();
        let mut pt: u32 = 0;
        let mut ct: u32 = 0;
        let mut sr: Option<String> = None;
        let events = parse_anthropic_sse_block(block, &mut kinds, &mut pt, &mut ct, &mut sr);
        assert!(matches!(events.as_slice(), [Err(e)] if e.to_string().contains("upstream overloaded")));
    }

    #[test]
    fn parse_anthropic_ping_is_noop() {
        let block = "event: ping\ndata: {\"type\":\"ping\"}";
        let mut kinds = std::collections::HashMap::new();
        let mut pt: u32 = 0;
        let mut ct: u32 = 0;
        let mut sr: Option<String> = None;
        let events = parse_anthropic_sse_block(block, &mut kinds, &mut pt, &mut ct, &mut sr);
        assert!(events.is_empty());
    }

    #[test]
    fn parse_anthropic_unknown_block_kind_drops_delta_silently() {
        // A content_block of an unknown type (e.g. a future
        // multimodal `image` block) shouldn't break the stream — we
        // record it as Unknown and drop its deltas. Pearl th-366aa8.
        let blocks = [
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"image"}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"image_delta","source":{"data":"<base64>"}}}"#,
            r#"data: {"type":"message_stop"}"#,
        ];
        let events = run_anth_blocks(&blocks);
        // Should have usage + done but no Delta / Reasoning / ToolCall events
        let has_content = events.iter().any(|e| {
            matches!(
                e,
                StreamEvent::Delta { .. } | StreamEvent::Reasoning { .. } | StreamEvent::ToolCallStart { .. } | StreamEvent::ToolCallArgumentsDelta { .. }
            )
        });
        assert!(!has_content, "unknown block deltas must be dropped, not emitted");
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
                cached_tokens: 0,
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

    // --- Pearl th-cb3c2a: cumulative-vs-delta streaming normalizer ---

    #[test]
    fn stream_content_normalizer_passes_true_deltas_through_unchanged() {
        let mut n = StreamContentNormalizer::default();
        assert_eq!(n.normalize("Hello"), Some("Hello"));
        assert_eq!(n.normalize(" world"), Some(" world"));
        assert_eq!(n.accumulated, "Hello world");
    }

    #[test]
    fn stream_content_normalizer_strips_cumulative_chunks() {
        // Simulates a provider that emits cumulative content per chunk —
        // each chunk is "everything-so-far". Without the normalizer, naive
        // push_str on each chunk would yield "II'llI'll help" (the bug we
        // saw in ~/.smooth/coding-sessions/*.json: "II'll help you" etc.).
        let mut n = StreamContentNormalizer::default();
        assert_eq!(n.normalize("I"), Some("I"));
        assert_eq!(n.normalize("I'll"), Some("'ll"));
        assert_eq!(n.normalize("I'll help"), Some(" help"));
        assert_eq!(n.normalize("I'll help you"), Some(" you"));
        assert_eq!(n.accumulated, "I'll help you");
    }

    #[test]
    fn stream_content_normalizer_drops_exact_duplicate_chunks() {
        // Some providers send the cumulative content AGAIN after a `done`-
        // adjacent chunk as a "final state" message. That whole chunk is
        // already in our accumulator — drop it to avoid double-emission.
        let mut n = StreamContentNormalizer::default();
        n.normalize("Hello world");
        assert_eq!(n.normalize("Hello world"), None);
        assert_eq!(n.accumulated, "Hello world");
    }

    #[test]
    fn stream_content_normalizer_handles_word_level_cumulative() {
        // Regression for the exact pattern seen in the bug session:
        //   "LetLet me me first first read read the current file again the current file again"
        // — that's "Let", "Let me", "Let me first", "Let me first read",
        // "Let me first read the current file again" if cumulative were
        // misinterpreted as delta. With the normalizer, the accumulator
        // should be exactly "Let me first read the current file again".
        let mut n = StreamContentNormalizer::default();
        let chunks = ["Let", "Let me", "Let me first", "Let me first read", "Let me first read the current file again"];
        let mut emitted = String::new();
        for c in chunks {
            if let Some(delta) = n.normalize(c) {
                emitted.push_str(delta);
            }
        }
        assert_eq!(emitted, "Let me first read the current file again");
        assert_eq!(n.accumulated, "Let me first read the current file again");
    }

    #[test]
    fn stream_content_normalizer_drops_empty_chunks() {
        let mut n = StreamContentNormalizer::default();
        assert_eq!(n.normalize(""), None);
        assert_eq!(n.normalize("Hello"), Some("Hello"));
        assert_eq!(n.normalize(""), None);
    }

    #[tokio::test]
    async fn accumulate_stream_after_normalizer_yields_clean_content_on_cumulative_provider() {
        // End-to-end: run cumulative chunks through normalize_stream_event
        // then through accumulate_stream_events. The final response content
        // should be the single intended sentence, not the quadratic blowup.
        let mut content_norm = StreamContentNormalizer::default();
        let mut tool_norms = std::collections::HashMap::new();
        let raw: Vec<anyhow::Result<StreamEvent>> = vec![
            Ok(StreamEvent::Delta { content: "I".into() }),
            Ok(StreamEvent::Delta { content: "I'll".into() }),
            Ok(StreamEvent::Delta {
                content: "I'll help you".into(),
            }),
            Ok(StreamEvent::Delta {
                content: "I'll help you read the file".into(),
            }),
            Ok(StreamEvent::Done { finish_reason: "stop".into() }),
        ];
        let normalized: Vec<anyhow::Result<StreamEvent>> = raw
            .into_iter()
            .filter_map(|e| normalize_stream_event(e, &mut content_norm, &mut tool_norms))
            .collect();
        let stream: Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>> = Box::pin(futures_util::stream::iter(normalized));
        let response = accumulate_stream_events(stream).await.expect("accumulate");
        assert_eq!(
            response.content, "I'll help you read the file",
            "cumulative chunks must NOT produce 'II'llI'll help youI'll help you read the file' — see pearl th-cb3c2a"
        );
    }

    #[tokio::test]
    async fn accumulate_stream_after_normalizer_preserves_well_behaved_delta_provider() {
        // The normalizer must be a no-op for well-behaved providers
        // (true deltas). Otherwise we'd break every OpenAI / Anthropic
        // stream we've ever shipped.
        let mut content_norm = StreamContentNormalizer::default();
        let mut tool_norms = std::collections::HashMap::new();
        let raw: Vec<anyhow::Result<StreamEvent>> = vec![
            Ok(StreamEvent::Delta { content: "Hello".into() }),
            Ok(StreamEvent::Delta { content: " ".into() }),
            Ok(StreamEvent::Delta { content: "world".into() }),
            Ok(StreamEvent::Done { finish_reason: "stop".into() }),
        ];
        let normalized: Vec<anyhow::Result<StreamEvent>> = raw
            .into_iter()
            .filter_map(|e| normalize_stream_event(e, &mut content_norm, &mut tool_norms))
            .collect();
        let stream: Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>> = Box::pin(futures_util::stream::iter(normalized));
        let response = accumulate_stream_events(stream).await.expect("accumulate");
        assert_eq!(response.content, "Hello world");
    }

    #[test]
    fn normalize_stream_event_normalizes_tool_call_arguments_independently_per_index() {
        // Cumulative arguments are an even nastier failure mode than
        // cumulative content: malformed JSON breaks the tool dispatcher
        // entirely. Each tool-call index keeps its own accumulator.
        let mut content_norm = StreamContentNormalizer::default();
        let mut tool_norms: std::collections::HashMap<usize, StreamContentNormalizer> = std::collections::HashMap::new();
        let raw: Vec<anyhow::Result<StreamEvent>> = vec![
            Ok(StreamEvent::ToolCallArgumentsDelta {
                index: 0,
                arguments_chunk: r#"{"path""#.into(),
            }),
            Ok(StreamEvent::ToolCallArgumentsDelta {
                index: 0,
                arguments_chunk: r#"{"path":"a.rs"}"#.into(),
            }),
        ];
        let normalized: Vec<_> = raw
            .into_iter()
            .filter_map(|e| normalize_stream_event(e, &mut content_norm, &mut tool_norms))
            .collect();
        let mut total = String::new();
        for ev in normalized {
            if let Ok(StreamEvent::ToolCallArgumentsDelta { arguments_chunk, .. }) = ev {
                total.push_str(&arguments_chunk);
            }
        }
        assert_eq!(total, r#"{"path":"a.rs"}"#, "tool args must not duplicate when cumulative");
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

    #[test]
    fn canonical_args_json_passes_object_through() {
        let v = serde_json::json!({"city": "Tokyo", "unit": "c"});
        let s = canonical_tool_arguments_json(&v);
        let reparsed: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        assert_eq!(reparsed, v);
        assert!(s.starts_with('{') && s.ends_with('}'));
    }

    #[test]
    fn canonical_args_json_coerces_null_to_empty_object() {
        assert_eq!(canonical_tool_arguments_json(&serde_json::Value::Null), "{}");
    }

    #[test]
    fn canonical_args_json_coerces_primitives_to_empty_object() {
        assert_eq!(canonical_tool_arguments_json(&serde_json::json!(42)), "{}");
        assert_eq!(canonical_tool_arguments_json(&serde_json::json!(true)), "{}");
        assert_eq!(canonical_tool_arguments_json(&serde_json::json!("already-a-string")), "{}");
        assert_eq!(canonical_tool_arguments_json(&serde_json::json!([1, 2, 3])), "{}");
    }

    #[test]
    fn canonical_args_json_empty_object_stays_empty_object() {
        assert_eq!(canonical_tool_arguments_json(&serde_json::json!({})), "{}");
    }

    // Pearl th-c65ca3 — qwen3-coder pseudo-XML tool-call recovery.

    #[test]
    fn parse_canonical_qwen_tool_call() {
        let content = r#"<tool_call>
{"name": "host_tool", "arguments": {"tool": "curl", "args": ["-I", "https://x.com"]}}
</tool_call>"#;
        let calls = super::parse_pseudo_tool_xml(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "host_tool");
        assert_eq!(calls[0].arguments["tool"], "curl");
        assert_eq!(calls[0].arguments["args"][0], "-I");
    }

    #[test]
    fn parse_malformed_function_parameter_form() {
        // Exact shape from user repro 2026-05-12.
        let content = r#"<function=host_tool> <parameter=tool> curl <parameter=args> ["-I", "https://smoo-hub.com"]   </tool_call>"#;
        let calls = super::parse_pseudo_tool_xml(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "host_tool");
        assert_eq!(calls[0].arguments["tool"], "curl");
        assert_eq!(calls[0].arguments["args"][0], "-I");
        assert_eq!(calls[0].arguments["args"][1], "https://smoo-hub.com");
    }

    #[test]
    fn parse_no_xml_returns_empty() {
        assert!(super::parse_pseudo_tool_xml("just some prose, no XML.").is_empty());
    }

    #[test]
    fn parse_handles_two_calls() {
        let content = r#"<function=a> <parameter=x> 1 </tool_call><function=b> <parameter=y> 2 </tool_call>"#;
        let calls = super::parse_pseudo_tool_xml(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn content_has_pseudo_tool_xml_detects_both_shapes() {
        assert!(super::content_has_pseudo_tool_xml("<function=foo>"));
        assert!(super::content_has_pseudo_tool_xml("<tool_call>{}</tool_call>"));
        assert!(!super::content_has_pseudo_tool_xml("plain prose"));
        // Standalone <tool_call> without closer shouldn't trigger
        // (would otherwise mangle prose).
        assert!(!super::content_has_pseudo_tool_xml("<tool_call> dangling"));
    }

    // -------- Anthropic prompt-caching tests (th-litellm-caching-client) --------

    fn build_test_request(system: &str, user: &str, tools: &[(&str, &str)]) -> ChatRequest {
        let mut messages: Vec<ChatMessage> = vec![
            ChatMessage {
                role: "system".into(),
                content: ChatContent::Text(Some(system.into())),
                tool_call_id: None,
                tool_name: None,
                tool_calls: vec![],
                reasoning_content: None,
            },
            ChatMessage {
                role: "user".into(),
                content: ChatContent::Text(Some(user.into())),
                tool_call_id: None,
                tool_name: None,
                tool_calls: vec![],
                reasoning_content: None,
            },
        ];
        let mut chat_tools: Vec<ChatTool> = tools
            .iter()
            .map(|(name, desc)| ChatTool {
                r#type: "function".into(),
                function: ChatFunction {
                    name: (*name).to_string(),
                    description: (*desc).to_string(),
                    parameters: serde_json::json!({}),
                },
                cache_control: None,
            })
            .collect();
        apply_cache_control(&mut messages, &mut chat_tools);
        ChatRequest {
            model: "smooth-coding-claude".into(),
            messages,
            max_tokens: 100,
            temperature: 0.0,
            tools: chat_tools,
            tool_choice: None,
        }
    }

    #[test]
    fn cache_control_gate_recognizes_claude_routes() {
        // Claude model id + LiteLLM url → cache it.
        assert!(supports_anthropic_cache_control("claude-sonnet-4-20250514", "https://llm.smoo.ai/v1"));
        // Smooth-coding alias + LiteLLM url → cache it.
        assert!(supports_anthropic_cache_control("smooth-coding-claude", "https://llm.smoo.ai/v1"));
        // Direct Anthropic API + Claude id → cache it.
        assert!(supports_anthropic_cache_control("claude-opus-4", "https://api.anthropic.com/v1"));
        // GPT model on OpenAI → no cache control (would 400).
        assert!(!supports_anthropic_cache_control("gpt-4o", "https://api.openai.com/v1"));
        // Gemini-compat → no cache control.
        assert!(!supports_anthropic_cache_control("gemini-1.5-pro", "https://generativelanguage.googleapis.com"));
        // Claude id but bare OpenAI url (someone mis-configured) — still
        // gated off because the wire isn't LiteLLM/Anthropic.
        assert!(!supports_anthropic_cache_control("claude-3-sonnet", "https://api.openai.com/v1"));
        // smooth-fast routes to Groq/Llama via LiteLLM — must NOT be cached.
        assert!(!supports_anthropic_cache_control("smooth-fast", "https://llm.smoo.ai/v1"));
    }

    #[test]
    fn claude_request_body_has_cache_control_on_system_and_tools() {
        let req = build_test_request("You are smooth.", "Hi", &[("bash", "Run a command"), ("file_write", "Write a file")]);
        let json = serde_json::to_value(&req).expect("serialize");

        // System message must be in the block form with cache_control on
        // its single text block.
        let sys = &json["messages"][0];
        assert_eq!(sys["role"], "system");
        let sys_content = &sys["content"];
        assert!(sys_content.is_array(), "system content must be an array of blocks, got {sys_content}");
        let sys_block = &sys_content[0];
        assert_eq!(sys_block["type"], "text");
        assert_eq!(sys_block["text"], "You are smooth.");
        assert_eq!(sys_block["cache_control"]["type"], "ephemeral");

        // LAST tool must carry top-level cache_control: ephemeral.
        let tools = json["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 2);
        assert!(
            tools[0].get("cache_control").is_none_or(serde_json::Value::is_null),
            "first tool must not carry cache_control"
        );
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");

        // The user (last) message must also be marked so turn-by-turn
        // history caching extends.
        let last = &json["messages"][1];
        assert_eq!(last["role"], "user");
        let last_content = &last["content"];
        assert!(last_content.is_array(), "last message content must be in block form");
        assert_eq!(last_content[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn gpt_request_body_has_no_cache_control() {
        // Simulate the GPT/OpenAI path: we DON'T call apply_cache_control.
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: ChatContent::Text(Some("You are smooth.".into())),
                tool_call_id: None,
                tool_name: None,
                tool_calls: vec![],
                reasoning_content: None,
            },
            ChatMessage {
                role: "user".into(),
                content: ChatContent::Text(Some("Hi".into())),
                tool_call_id: None,
                tool_name: None,
                tool_calls: vec![],
                reasoning_content: None,
            },
        ];
        let chat_tools = vec![ChatTool {
            r#type: "function".into(),
            function: ChatFunction {
                name: "bash".into(),
                description: "Run a command".into(),
                parameters: serde_json::json!({}),
            },
            cache_control: None,
        }];
        let req = ChatRequest {
            model: "gpt-4o".into(),
            messages,
            max_tokens: 100,
            temperature: 0.0,
            tools: chat_tools,
            tool_choice: None,
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(
            !json.contains("cache_control"),
            "GPT/OpenAI request body must NOT contain `cache_control`: {json}"
        );
        // System content must serialize as a plain string for OpenAI compat.
        assert!(json.contains(r#""content":"You are smooth.""#), "system content must be a plain string: {json}");
    }

    #[test]
    fn usage_parses_prompt_tokens_details_cached_tokens() {
        // LiteLLM/Anthropic-compat wire shape: usage carries a nested
        // `prompt_tokens_details.cached_tokens` field. Our ChatUsage must
        // pick it up so CostTracker can aggregate cache hits.
        let json = r#"{
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 50,
                "total_tokens": 1250,
                "prompt_tokens_details": {"cached_tokens": 1000}
            }
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).expect("deserialize");
        let usage = resp.usage.expect("usage present");
        assert_eq!(usage.prompt_tokens, 1200);
        assert_eq!(
            usage.prompt_tokens_details.as_ref().map(|d| d.cached_tokens),
            Some(1000),
            "must capture cached_tokens from nested prompt_tokens_details"
        );

        // Missing prompt_tokens_details: falls through cleanly as None
        // (zero hits, not a deserialization error).
        let json2 = r#"{
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 5, "total_tokens": 105}
        }"#;
        let resp2: ChatResponse = serde_json::from_str(json2).expect("deserialize");
        assert!(resp2.usage.expect("usage").prompt_tokens_details.is_none());
    }
}

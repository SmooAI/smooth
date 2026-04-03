use serde::{Deserialize, Serialize};

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

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }
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
}

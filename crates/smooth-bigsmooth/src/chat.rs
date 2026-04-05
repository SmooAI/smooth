//! Chat — OpenCode Zen API streaming via reqwest.
//!
//! DEPRECATED: This module is hardcoded to OpenCode Zen and should not be used
//! for new code. Use `smooth_operator::LlmClient` + `ProviderRegistry` instead,
//! which supports all configured providers (openrouter, anthropic, openai, etc.).
//!
//! Kept temporarily because `server.rs` routes still reference it.

use std::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const ZEN_API_URL: &str = "https://opencode.ai/zen/v1/chat/completions";

const SYSTEM_PROMPT: &str = "You are Smooth, an AI agent orchestration leader. You help users manage projects, assign work to Smooth Operators (AI agents in sandboxes), review work, and coordinate tasks.

Available commands: th run <bead-id>, th operators, th pause/steer/cancel <bead-id>, th auth status, th status";

/// OpenCode auth entry from `~/.local/share/opencode/auth.json`.
#[derive(Deserialize)]
#[allow(dead_code)]
struct OpenCodeAuth {
    key: String,
}

/// Get the OpenCode Zen API key from the auth store.
#[deprecated(note = "Use smooth-operator ProviderRegistry instead")]
pub fn get_zen_api_key() -> Option<String> {
    let auth_path = dirs_next::home_dir()?.join(".local/share/opencode/auth.json");
    let content = fs::read_to_string(auth_path).ok()?;
    let auth: serde_json::Value = serde_json::from_str(&content).ok()?;
    auth.get("opencode")?.get("key")?.as_str().map(String::from)
}

/// Check if OpenCode Zen is authenticated.
#[deprecated(note = "Use smooth-operator ProviderRegistry instead")]
#[must_use]
pub fn is_authenticated() -> bool {
    #[allow(deprecated)]
    get_zen_api_key().is_some()
}

/// Chat completion request.
#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

/// Stream a chat response from OpenCode Zen.
/// Returns chunks of text as they arrive.
#[deprecated(note = "Use smooth-operator LlmClient + ProviderRegistry instead")]
pub async fn stream_chat(user_message: &str) -> Result<impl futures_core::Stream<Item = Result<String>>> {
    #[allow(deprecated)]
    let api_key = get_zen_api_key().context("OpenCode Zen not authenticated. Run: th auth login opencode-zen")?;

    let client = reqwest::Client::new();
    let response = client
        .post(ZEN_API_URL)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&ChatRequest {
            model: "claude-sonnet-4-6".into(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: SYSTEM_PROMPT.into(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: user_message.into(),
                },
            ],
            stream: true,
        })
        .send()
        .await
        .context("Failed to connect to OpenCode Zen API")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenCode Zen API error ({status}): {}", &body[..body.len().min(200)]);
    }

    // Stream SSE response

    // For now, collect the full response (streaming requires more complex plumbing)
    let full_body = response.text().await?;
    let mut content = String::new();

    for line in full_body.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                break;
            }
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(delta) = json.pointer("/choices/0/delta/content").and_then(|v| v.as_str()) {
                    content.push_str(delta);
                }
            }
        }
    }

    // Return as a single-item stream for now
    Ok(futures_util::stream::once(async move { Ok(content) }))
}

/// Non-streaming chat — returns complete response.
#[deprecated(note = "Use smooth-operator LlmClient + ProviderRegistry instead")]
pub async fn chat(user_message: &str) -> Result<String> {
    #[allow(deprecated)]
    let api_key = get_zen_api_key().context("OpenCode Zen not authenticated. Run: th auth login opencode-zen")?;

    let client = reqwest::Client::new();
    let response = client
        .post(ZEN_API_URL)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&ChatRequest {
            model: "claude-sonnet-4-6".into(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: SYSTEM_PROMPT.into(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: user_message.into(),
                },
            ],
            stream: false,
        })
        .send()
        .await
        .context("Failed to connect to OpenCode Zen API")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenCode Zen API error ({status}): {}", &body[..body.len().min(200)]);
    }

    let json: serde_json::Value = response.json().await?;
    let content = json
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("No response")
        .to_string();

    Ok(content)
}

#[cfg(test)]
mod tests {
    #[allow(deprecated)]
    use super::*;

    #[test]
    fn test_get_zen_api_key() {
        // Just verify it doesn't panic — key may or may not be present
        #[allow(deprecated)]
        let _ = get_zen_api_key();
    }

    #[test]
    fn test_system_prompt_not_empty() {
        assert!(!SYSTEM_PROMPT.is_empty());
        assert!(SYSTEM_PROMPT.contains("Smooth"));
    }
}

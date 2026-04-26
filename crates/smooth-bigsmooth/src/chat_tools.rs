//! Tools for the agentic Big Smooth chat handler.
//!
//! When the user chats with Big Smooth, the chat handler runs an
//! `Agent::run_with_channel` loop with these tools registered. The agent
//! is the team lead — it searches pearls, creates new ones with smooth-
//! summarize-generated titles, dispatches teammates (operators), nudges
//! them with steering messages, and reads back their progress. Every
//! tool maps to existing wiring elsewhere in this crate so the chat
//! path is a thin LLM-driven layer over the same dispatch / pearl /
//! mailbox primitives the rest of Big Smooth uses.
//!
//! See `~/.claude/plans/sorted-orbiting-hummingbird.md` Phase 1.

use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use serde_json::json;
use smooth_operator::llm::LlmClient;
use smooth_operator::providers::{Activity, ProviderRegistry};
use smooth_operator::tool::{Tool, ToolSchema};

use crate::server::{AppState, DispatchOptions};

// ── pearls.search ──────────────────────────────────────────────────────

pub struct PearlsSearchTool {
    pub state: AppState,
}

#[async_trait]
impl Tool for PearlsSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "pearls_search".to_string(),
            description: "Search the project's pearls (work items) by keyword. Returns up to 10 matches with id, title, and status. Use this BEFORE creating a new pearl to check whether the work is already tracked.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "description": "Free-text search query." }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let query = arguments["query"].as_str().unwrap_or("").trim();
        if query.is_empty() {
            return Ok("(no query provided)".to_string());
        }
        let pearls = self.state.pearl_store.search(query)?;
        if pearls.is_empty() {
            return Ok(format!("No pearls match `{query}`."));
        }
        let mut out = String::new();
        for p in pearls.iter().take(10) {
            out.push_str(&format!("[{}] {} P{} {}\n", p.status.as_str(), p.id, p.priority.as_u8(), p.title));
        }
        Ok(out.trim_end().to_string())
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ── pearls.show ────────────────────────────────────────────────────────

pub struct PearlsShowTool {
    pub state: AppState,
}

#[async_trait]
impl Tool for PearlsShowTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "pearls_show".to_string(),
            description: "Get the full details of a single pearl: title, status, description, and the latest 20 comments (which include any teammate progress, questions, or chat replies).".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "string", "description": "Pearl id, e.g. th-abc123." }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let id = arguments["id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;
        let pearl = self.state.pearl_store.get(id)?.ok_or_else(|| anyhow::anyhow!("pearl {id} not found"))?;
        let comments = self.state.pearl_store.get_comments(id).unwrap_or_default();
        let mut out = String::new();
        out.push_str(&format!(
            "{} [{}] P{} — {}\n",
            pearl.id,
            pearl.status.as_str(),
            pearl.priority.as_u8(),
            pearl.title
        ));
        if !pearl.description.is_empty() {
            out.push('\n');
            out.push_str(&pearl.description);
            out.push('\n');
        }
        let recent = comments.iter().rev().take(20).collect::<Vec<_>>();
        if !recent.is_empty() {
            out.push_str("\n--- recent comments (newest first) ---\n");
            for c in recent {
                out.push_str(&format!("- {} {}\n", c.created_at.format("%Y-%m-%d %H:%M:%S"), c.content));
            }
        }
        Ok(out.trim_end().to_string())
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ── pearls.create ──────────────────────────────────────────────────────

pub struct PearlsCreateTool {
    pub state: AppState,
    /// Loaded `ProviderRegistry` used for the smooth-summarize slot to
    /// generate a concise title from the description.
    pub registry: Arc<ProviderRegistry>,
}

#[async_trait]
impl Tool for PearlsCreateTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "pearls_create".to_string(),
            description: "Create a new pearl (work item) from a description. Big Smooth uses the summarize slot to generate a concise title automatically — the caller doesn't supply one. Returns the new pearl id.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["description"],
                "properties": {
                    "description": { "type": "string", "description": "What needs to be done and why. The title is auto-generated from this." },
                    "priority": { "type": "number", "enum": [0,1,2,3,4], "default": 2, "description": "0=critical, 2=medium (default), 4=backlog." }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let description = arguments["description"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'description'"))?;
        let priority_val = arguments.get("priority").and_then(|v| v.as_u64()).unwrap_or(2) as u8;
        let priority = smooth_pearls::Priority::from_u8(priority_val).unwrap_or(smooth_pearls::Priority::Medium);

        let title = generate_title(&self.registry, description).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "pearls_create: title summarize failed, falling back to truncation");
            description.chars().take(60).collect()
        });

        let new = smooth_pearls::NewPearl {
            title: title.clone(),
            description: description.to_string(),
            pearl_type: smooth_pearls::PearlType::Task,
            priority,
            assigned_to: None,
            parent_id: None,
            labels: Vec::new(),
        };
        let pearl = self.state.pearl_store.create(&new).context("creating pearl")?;
        Ok(format!("Created pearl {} — {}", pearl.id, pearl.title))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

/// Ask the summarize-slot LLM for a concise pearl title (≤60 chars). If
/// the LLM is unavailable or returns nothing, the caller falls back to a
/// description truncation.
async fn generate_title(registry: &ProviderRegistry, description: &str) -> anyhow::Result<String> {
    let cfg = registry.llm_config_for(Activity::Summarize).context("resolving summarize slot")?;
    let llm = LlmClient::new(cfg);
    let sys = smooth_operator::conversation::Message::system(
        "Summarize the following work-item description as a concise pearl title (≤60 chars, no trailing period, no quotes). Respond with the title only — no preamble, no explanation.",
    );
    let user = smooth_operator::conversation::Message::user(description);
    let resp = llm.chat(&[&sys, &user], &[]).await.context("title summarize call")?;
    let title = resp.content.trim().trim_matches('"').trim_matches('\'').trim().to_string();
    if title.is_empty() {
        return Err(anyhow::anyhow!("summarize returned empty title"));
    }
    Ok(title.chars().take(60).collect())
}

// ── teammate.spawn ─────────────────────────────────────────────────────

pub struct TeammateSpawnTool {
    pub state: AppState,
}

#[async_trait]
impl Tool for TeammateSpawnTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "teammate_spawn".to_string(),
            description: "Spawn a teammate (operator) on a pearl. The teammate runs in its own sandbox with its own context, picks up the pearl's description as its task, and reports progress as pearl comments. Returns immediately — use teammate_read or pearls_show to follow progress.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pearl_id"],
                "properties": {
                    "pearl_id": { "type": "string", "description": "The pearl to dispatch on. Must already exist (call pearls_create first)." },
                    "extra_prompt": { "type": "string", "description": "Optional extra instruction appended to the pearl description when handing off to the teammate." },
                    "budget_usd": { "type": "number", "description": "Optional cost cap in USD for this dispatch." },
                    "working_dir": { "type": "string", "description": "Optional working directory for the teammate's sandbox. Pass an absolute path; defaults to the project root." },
                    "role": { "type": "string", "description": "Optional cast role to spawn under (e.g. `fixer`, `mapper`, `oracle`, `heckler` — see smooth-operator/src/cast). Affects permissions, prompt, and routing slot." }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let pearl_id = arguments["pearl_id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'pearl_id'"))?.to_string();
        let extra = arguments.get("extra_prompt").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let budget = arguments.get("budget_usd").and_then(|v| v.as_f64());
        let working_dir = arguments.get("working_dir").and_then(|v| v.as_str()).map(String::from);
        let role = arguments.get("role").and_then(|v| v.as_str()).map(String::from);

        let pearl = self
            .state
            .pearl_store
            .get(&pearl_id)?
            .ok_or_else(|| anyhow::anyhow!("pearl {pearl_id} not found"))?;

        let mut message = pearl.description.clone();
        if !extra.is_empty() {
            message.push_str("\n\n");
            message.push_str(&extra);
        }
        // Pass the caller's pearl id through so dispatch reuses it instead
        // of creating a duplicate. The chat-agent's `pearls_create` already
        // produced this pearl; dispatch will flip its status to in_progress
        // (which also keeps the orchestrator from auto-dispatching it).
        let opts = DispatchOptions {
            message,
            model: None,
            budget,
            working_dir,
            image: None,
            keep_alive: false,
            memory_mb: None,
            agent: role,
            pearl_id: Some(pearl_id.clone()),
        };

        // Fire-and-forget: dispatch_ws_task is `async` and runs the whole
        // task end-to-end. From the chat agent we don't want to block —
        // spawn it on the runtime so the chat turn returns promptly.
        let state = self.state.clone();
        tokio::spawn(async move {
            crate::server::dispatch_ws_task(&state, opts).await;
        });

        Ok(format!(
            "Teammate dispatched on pearl {pearl_id}. Use teammate_read or pearls_show to follow progress."
        ))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── teammate.message ───────────────────────────────────────────────────

pub struct TeammateMessageTool {
    pub state: AppState,
}

#[async_trait]
impl Tool for TeammateMessageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "teammate_message".to_string(),
            description: "Send a steering message to a running teammate. The teammate's mailbox poller picks it up within ~1.5 s and the message is injected into its conversation as authoritative redirection. Use this for mid-flight nudges, not for replies to direct chat (those use [CHAT:USER] which the user posts via the chat panel, not the chat agent).".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pearl_id", "message"],
                "properties": {
                    "pearl_id": { "type": "string", "description": "The pearl whose teammate should receive the message." },
                    "message": { "type": "string", "description": "The guidance to inject." }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let pearl_id = arguments["pearl_id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'pearl_id'"))?;
        let message = arguments["message"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'message'"))?;
        let body = format!("[STEERING:GUIDANCE] {message}");
        self.state.pearl_store.add_comment(pearl_id, &body).context("posting steer comment")?;
        Ok(format!("Steering message queued for pearl {pearl_id}."))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

// ── teammate.read ──────────────────────────────────────────────────────

pub struct TeammateReadTool {
    pub state: AppState,
}

#[async_trait]
impl Tool for TeammateReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "teammate_read".to_string(),
            description: "Read recent comments from a teammate's pearl, filtered to teammate-originated traffic ([CHAT:TEAMMATE], [PROGRESS], [QUESTION:TEAMMATE], [IDLE]). Use this when the user asks `what is the teammate doing?` or `did they finish?`.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pearl_id"],
                "properties": {
                    "pearl_id": { "type": "string", "description": "Pearl id." },
                    "max": { "type": "number", "default": 20, "description": "Cap on number of returned comments (most recent)." }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let pearl_id = arguments["pearl_id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'pearl_id'"))?;
        let max = arguments.get("max").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
        let comments = self.state.pearl_store.get_comments(pearl_id).context("reading pearl comments")?;
        let teammate_only: Vec<_> = comments
            .into_iter()
            .filter(|c| {
                let t = c.content.trim_start();
                t.starts_with("[CHAT:TEAMMATE]") || t.starts_with("[PROGRESS]") || t.starts_with("[QUESTION:TEAMMATE") || t.starts_with("[IDLE]")
            })
            .collect();
        if teammate_only.is_empty() {
            return Ok("No teammate output yet.".to_string());
        }
        let mut out = String::new();
        for c in teammate_only.iter().rev().take(max) {
            out.push_str(&format!("- {} {}\n", c.created_at.format("%Y-%m-%d %H:%M:%S"), c.content));
        }
        Ok(out.trim_end().to_string())
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ── Registration ───────────────────────────────────────────────────────

/// Build the `ToolRegistry` for the chat agent. Returns an owned registry
/// suitable for handing to `Agent::new`.
pub fn build_chat_tools(state: AppState, registry: Arc<ProviderRegistry>) -> smooth_operator::tool::ToolRegistry {
    let mut tools = smooth_operator::tool::ToolRegistry::new();
    tools.register(PearlsSearchTool { state: state.clone() });
    tools.register(PearlsShowTool { state: state.clone() });
    tools.register(PearlsCreateTool {
        state: state.clone(),
        registry,
    });
    tools.register(TeammateSpawnTool { state: state.clone() });
    tools.register(TeammateMessageTool { state: state.clone() });
    tools.register(TeammateReadTool { state });
    tools
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_have_stable_names() {
        // Round-trip: every tool's schema name must round-trip stably so
        // the chat-agent's tool-call routing works. Names use snake_case
        // (Anthropic + OpenAI tool-call schema convention).
        let names = [
            "pearls_search",
            "pearls_show",
            "pearls_create",
            "teammate_spawn",
            "teammate_message",
            "teammate_read",
        ];
        for n in &names {
            assert!(n.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{n}");
        }
    }
}

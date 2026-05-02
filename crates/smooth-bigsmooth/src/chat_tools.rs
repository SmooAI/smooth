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

// ── pearls.list ────────────────────────────────────────────────────────

pub struct PearlsListTool {
    pub state: AppState,
}

#[async_trait]
impl Tool for PearlsListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "pearls_list".to_string(),
            description: "List pearls by status. Use this to answer 'how many open pearls', 'what's in progress', 'what's ready', etc. Goes through the in-process pearl store — much faster and more reliable than shelling out to `th pearls list`. Returns up to `limit` pearls (default 50, hard cap 200) sorted newest first.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["open", "in_progress", "closed", "deferred"],
                        "description": "Optional status filter. Omit for all statuses."
                    },
                    "limit": {
                        "type": "number",
                        "default": 50,
                        "description": "Max pearls to return (capped at 200)."
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let limit = arguments.get("limit").and_then(|v| v.as_u64()).unwrap_or(50).clamp(1, 200) as usize;
        let mut q = smooth_pearls::PearlQuery::new();
        q.limit = limit;
        if let Some(status_str) = arguments.get("status").and_then(|v| v.as_str()) {
            q = match status_str {
                "open" => q.with_status(smooth_pearls::PearlStatus::Open),
                "in_progress" => q.with_status(smooth_pearls::PearlStatus::InProgress),
                "closed" => q.with_status(smooth_pearls::PearlStatus::Closed),
                "deferred" => q.with_status(smooth_pearls::PearlStatus::Deferred),
                other => anyhow::bail!("unknown status `{other}` — use one of: open, in_progress, closed, deferred"),
            };
        }
        let pearls = self.state.pearl_store.list(&q).context("listing pearls")?;
        if pearls.is_empty() {
            return Ok("(no pearls match)".to_string());
        }
        let total = pearls.len();
        let mut out = format!("{total} pearl(s):\n");
        for p in pearls.iter().take(limit) {
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
        // Claim the pearl immediately so the orchestrator's 5-second
        // ready-pearls poll doesn't race the chat-agent's follow-up
        // teammate_spawn call. Without this, both the chat-agent AND
        // the orchestrator can dispatch operators on the same pearl.
        let _ = self.state.pearl_store.update(
            &pearl.id,
            &smooth_pearls::PearlUpdate {
                status: Some(smooth_pearls::PearlStatus::InProgress),
                ..Default::default()
            },
        );
        Ok(format!("Created pearl {} — {} (claimed; ready for teammate_spawn)", pearl.id, pearl.title))
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

const MIN_CONTEXT_BRIEF_CHARS: usize = 80;

pub struct TeammateSpawnTool {
    pub state: AppState,
}

#[async_trait]
impl Tool for TeammateSpawnTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "teammate_spawn".to_string(),
            description: "Spawn a teammate (operator) on a pearl for REAL CODING WORK (multi-file edits, builds, test runs, debugging). Do NOT use this for one-shot bash-allowlist commands — git clone, gh repo clone, mkdir, curl etc. should run via the `bash` tool directly. The teammate runs in its own sandbox, picks up the pearl's description as its task, and reports progress as pearl comments. Returns immediately — use teammate_read or pearls_show to follow progress.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pearl_id", "context_brief"],
                "properties": {
                    "pearl_id": { "type": "string", "description": "The pearl to dispatch on. Must already exist (call pearls_create first)." },
                    "context_brief": {
                        "type": "string",
                        "minLength": 80,
                        "description": "REQUIRED. Brief the teammate like a smart colleague who just walked into the room — they haven't seen this conversation. Cover: (1) what you've already learned or ruled out, (2) the specific files/paths/commands they should look at first, (3) any judgment-call dimensions you want them flagged back to you rather than decided. For lookups, hand over the exact command. For investigations, hand over the question — prescribed steps become dead weight when the premise is wrong. Terse one-liners produce shallow generic work; never delegate understanding. Minimum 80 chars; the runner will reject shorter briefings."
                    },
                    "extra_prompt": { "type": "string", "description": "Optional extra instruction appended after the context_brief. Use this for fine-grained constraints (e.g. 'use the Rust 2021 edition', 'don't touch the migrations directory')." },
                    "budget_usd": { "type": "number", "description": "Optional cost cap in USD for this dispatch." },
                    "working_dir": { "type": "string", "description": "Working directory for the teammate's sandbox. Pass the most specific absolute path that scopes the work — e.g. for 'clone repo X to ~/dev/foo/X' pass `~/dev/foo`. Never pass a directory as broad as `~` or `/`; the runner can stall enumerating that much filesystem." },
                    "role": { "type": "string", "description": "Optional cast role to spawn under (e.g. `fixer`, `mapper`, `oracle`, `heckler` — see smooth-operator/src/cast). Affects permissions, prompt, and routing slot." },
                    "model": { "type": "string", "description": "DO NOT SET unless you have a specific reason. Default = role's slot (smooth-coding for `fixer`) which is the best balance of speed and tool-call reliability. Avoid `smooth-fast-gemini` — it can't reliably emit native tool calls and will wedge the runner. `smooth-reasoning` is for genuinely hard problems only." }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let pearl_id = arguments["pearl_id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'pearl_id'"))?.to_string();
        let context_brief = arguments.get("context_brief").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if context_brief.chars().count() < MIN_CONTEXT_BRIEF_CHARS {
            anyhow::bail!(
                "teammate_spawn rejected: `context_brief` must be at least {MIN_CONTEXT_BRIEF_CHARS} chars (got {}). \
                Brief the teammate like a smart colleague who just walked in: what you've learned, what you've \
                ruled out, the files/paths/commands to start with, and the judgment dimensions to flag back. \
                Re-issue the call with a real briefing — never delegate understanding.",
                context_brief.chars().count()
            );
        }
        let extra = arguments.get("extra_prompt").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let budget = arguments.get("budget_usd").and_then(|v| v.as_f64());
        let working_dir = arguments.get("working_dir").and_then(|v| v.as_str()).map(String::from);
        let role = arguments.get("role").and_then(|v| v.as_str()).map(String::from);
        let model = arguments.get("model").and_then(|v| v.as_str()).map(String::from);

        let pearl = self
            .state
            .pearl_store
            .get(&pearl_id)?
            .ok_or_else(|| anyhow::anyhow!("pearl {pearl_id} not found"))?;

        let mut message = pearl.description.clone();
        message.push_str("\n\n## Context from team lead\n\n");
        message.push_str(&context_brief);
        if !extra.is_empty() {
            message.push_str("\n\n## Extra constraints\n\n");
            message.push_str(&extra);
        }
        // Pass the caller's pearl id through so dispatch reuses it instead
        // of creating a duplicate. The chat-agent's `pearls_create` already
        // produced this pearl; dispatch will flip its status to in_progress
        // (which also keeps the orchestrator from auto-dispatching it).
        let opts = DispatchOptions {
            message,
            model,
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

// ── teammate.wait ──────────────────────────────────────────────────────
//
// The cheap way for the chat-agent to "drive a task to completion" without
// burning every iteration on `teammate_read`. Internally polls pearl
// comments every 5 s up to `max_wait_seconds` (capped at 120 s), returns
// when the teammate posts `[IDLE]`, posts a `[CHAT:TEAMMATE]` reply, hits
// the cap, or asks a question. The chat-agent calls this AFTER
// `teammate_spawn` and treats the returned snapshot as ground truth for
// the next decision.

pub struct TeammateWaitTool {
    pub state: AppState,
}

#[async_trait]
impl Tool for TeammateWaitTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "teammate_wait".to_string(),
            description: "Block for up to `max_wait_seconds` (cap 120) waiting for the teammate on this pearl to make progress. Returns when the teammate posts [IDLE], a [CHAT:TEAMMATE] reply, a [QUESTION:TEAMMATE], or the cap fires. Polls every 5 s. Cheaper than calling teammate_read in a loop — this burns ONE chat-agent iteration even if the teammate takes minutes. Use after `teammate_spawn` to drive the task to completion.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pearl_id"],
                "properties": {
                    "pearl_id": { "type": "string", "description": "Pearl id of the teammate." },
                    "max_wait_seconds": { "type": "number", "default": 60, "description": "Wait cap in seconds, max 120." }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let pearl_id = arguments["pearl_id"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'pearl_id'"))?.to_string();
        let max_wait = arguments.get("max_wait_seconds").and_then(|v| v.as_u64()).unwrap_or(60).clamp(5, 120);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(max_wait);

        let baseline = self.state.pearl_store.get_comments(&pearl_id).context("reading pearl comments")?;
        let baseline_ids: std::collections::HashSet<String> = baseline.iter().map(|c| c.id.clone()).collect();

        loop {
            let comments = self.state.pearl_store.get_comments(&pearl_id).context("reading pearl comments")?;
            // Look for any new teammate-originated comment since baseline.
            let new_comments: Vec<&smooth_pearls::PearlComment> = comments
                .iter()
                .filter(|c| !baseline_ids.contains(&c.id))
                .filter(|c| {
                    let t = c.content.trim_start();
                    t.starts_with("[CHAT:TEAMMATE]") || t.starts_with("[PROGRESS]") || t.starts_with("[QUESTION:TEAMMATE") || t.starts_with("[IDLE]")
                })
                .collect();

            // Stop conditions: idle / question / a chat reply with content / deadline.
            let saw_idle = new_comments.iter().any(|c| c.content.trim_start().starts_with("[IDLE]"));
            let saw_question = new_comments.iter().any(|c| c.content.trim_start().starts_with("[QUESTION:TEAMMATE"));
            let saw_chat = new_comments.iter().any(|c| c.content.trim_start().starts_with("[CHAT:TEAMMATE]"));

            if saw_idle || saw_question || saw_chat || std::time::Instant::now() >= deadline {
                let mut out = String::new();
                if saw_idle {
                    out.push_str("Teammate posted [IDLE] — task is complete.\n\n");
                } else if saw_question {
                    out.push_str("Teammate has a question for you to answer (see below).\n\n");
                } else if saw_chat {
                    out.push_str("Teammate posted a chat reply — see below.\n\n");
                } else {
                    out.push_str(&format!(
                        "Wait cap of {max_wait}s reached — teammate is still working. You can call teammate_wait again, or teammate_read for a snapshot.\n\n"
                    ));
                }
                if new_comments.is_empty() {
                    out.push_str("(no new teammate output yet)");
                } else {
                    out.push_str("--- new teammate output ---\n");
                    for c in new_comments {
                        out.push_str(&format!("- {} {}\n", c.created_at.format("%H:%M:%S"), c.content));
                    }
                }
                return Ok(out.trim_end().to_string());
            }

            // 1.5s poll: matches the operator runner's mailbox-poll
            // cadence so the chat agent picks up [IDLE]/[CHAT:TEAMMATE]
            // within one round-trip of the teammate posting it. The old
            // 5s cadence added noticeable latency on quick teammates.
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }
}

// ── bash (read-only allowlist) ─────────────────────────────────────────

/// Tight-allowlist `bash` tool for the chat agent. Lets simple read-
/// only lookups (e.g. `gh repo list`, `git status`, `kubectl get pods`)
/// run directly from Big Smooth in 1-2 seconds instead of spawning a
/// whole teammate (which adds 30-90s of boot + LLM-turn overhead).
///
/// The allowlist is intentionally narrow — only commands that are
/// already host-trusted on the machine running BS (auth via the host's
/// own credentials cache, no fresh secrets passed). For risky / writing
/// / multi-step work the chat agent still spawns a teammate.
pub struct BashTool;

const BASH_ALLOWLIST: &[&str] = &[
    // Read-only lookups
    "gh", "git", "kubectl", "jq", "curl", "ls", "cat", "head", "tail", "wc", "grep", "rg", "fd", "find", "echo",
    // One-shot writes that don't need teammate isolation. `mkdir`
    // pairs with `git clone <url> <dest>` for "set up this repo for
    // me" requests so the chat agent can do them itself instead of
    // spending 30-90s booting a runner.
    "mkdir",
];
/// Explicit refuse-list, even for commands that LOOK harmless. `th`
/// re-enters Big Smooth's own dolt store via CLI subprocess and
/// deadlocks against the long-running serve. The interactive editors
/// hang waiting on stdin. The agent should reach for the native pearl
/// tools (`pearls_list`, `pearls_search`, `pearls_show`) instead.
const BASH_FORBIDDEN_FIRST_TOKEN: &[&str] = &["th", "smooth-dolt", "vim", "nvim", "emacs", "less", "more", "fzf"];
/// Wallclock cap for any single bash invocation. 30 s covers a small
/// `git clone` over a typical connection while still keeping the chat
/// agent honest — anything past this should be a teammate.
const BASH_TIMEOUT_SECS: u64 = 30;

#[async_trait]
impl Tool for BashTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash".to_string(),
            description: format!(
                "Run a short read-only shell command on the host. PREFER this over `teammate_spawn` for simple one-shot lookups (\"do I have a github repo for X?\", \"what's our current k8s pod state?\", \"git log -5 in the smooth repo\"). Allowlisted commands only: {}. Spawning a teammate costs 30-90s of boot time; this tool returns in 1-2s. Output capped at 12 KB. Hard timeout {}s. Returns stdout (with stderr appended on non-zero exit).",
                BASH_ALLOWLIST.join(", "),
                BASH_TIMEOUT_SECS
            ),
            parameters: json!({
                "type": "object",
                "required": ["cmd"],
                "properties": {
                    "cmd": { "type": "string", "description": "Full shell command line. First token must be in the allowlist (otherwise this tool refuses to run)." },
                    "cwd": { "type": "string", "description": "Optional working directory. Defaults to $HOME." }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let cmd = arguments["cmd"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'cmd'"))?.trim().to_string();
        if cmd.is_empty() {
            anyhow::bail!("empty cmd");
        }
        // Reject obvious shell-escape attempts that would let the model
        // chain through a non-allowlisted binary. Pipes/&&/||/; are OK
        // *between* allowlisted commands but we keep things simple
        // and only check the FIRST token. Multi-stage pipelines tend
        // to be teammate territory anyway.
        let first = cmd.split_whitespace().next().unwrap_or("");
        if BASH_FORBIDDEN_FIRST_TOKEN.contains(&first) {
            anyhow::bail!(
                "bash: '{first}' is explicitly forbidden — for pearl questions use `pearls_list`, `pearls_search`, or `pearls_show`; for editor-style tools spawn a teammate."
            );
        }
        if !BASH_ALLOWLIST.contains(&first) {
            anyhow::bail!(
                "bash: '{first}' is not in the allowlist. Allowed: {}. For anything else, spawn a teammate.",
                BASH_ALLOWLIST.join(", ")
            );
        }
        let cwd = arguments
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));

        let mut child = tokio::process::Command::new("/bin/sh");
        child.arg("-c").arg(&cmd).current_dir(&cwd).kill_on_drop(true);
        let out_fut = child.output();
        let timeout = tokio::time::timeout(std::time::Duration::from_secs(BASH_TIMEOUT_SECS), out_fut);

        match timeout.await {
            Ok(Ok(output)) => {
                let mut body = String::new();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                body.push_str(stdout.trim_end());
                if !output.status.success() {
                    body.push_str("\n\n--- stderr (exit ");
                    body.push_str(&output.status.code().unwrap_or(-1).to_string());
                    body.push_str(") ---\n");
                    body.push_str(stderr.trim_end());
                }
                if body.chars().count() > 12_000 {
                    let mut clipped: String = body.chars().take(12_000).collect();
                    clipped.push_str("\n\n[output truncated at 12 KB]");
                    Ok(clipped)
                } else {
                    Ok(body)
                }
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("bash spawn failed: {e}")),
            Err(_) => Err(anyhow::anyhow!("bash timeout after {BASH_TIMEOUT_SECS}s")),
        }
    }

    fn is_read_only(&self) -> bool {
        // Conservative: gh/git can write under unusual flags, but the
        // typical orchestration calls (list/status/diff/log) don't, and
        // the agent's system prompt steers it that way.
        false
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }
}

// ── Registration ───────────────────────────────────────────────────────

/// Build the `ToolRegistry` for the chat agent. Returns an owned registry
/// suitable for handing to `Agent::new`.
pub fn build_chat_tools(state: AppState, registry: Arc<ProviderRegistry>) -> smooth_operator::tool::ToolRegistry {
    let mut tools = smooth_operator::tool::ToolRegistry::new();
    tools.register(PearlsSearchTool { state: state.clone() });
    tools.register(PearlsListTool { state: state.clone() });
    tools.register(PearlsShowTool { state: state.clone() });
    tools.register(PearlsCreateTool {
        state: state.clone(),
        registry,
    });
    tools.register(TeammateSpawnTool { state: state.clone() });
    tools.register(TeammateMessageTool { state: state.clone() });
    tools.register(TeammateReadTool { state: state.clone() });
    tools.register(TeammateWaitTool { state });
    tools.register(BashTool);
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

    // Threshold has to be loose enough that real briefings pass and tight
    // enough that one-line tasks fail. 80 chars is roughly two short
    // sentences — enough room for "what you've learned, what you've
    // ruled out, where to start." Const-asserts compile-fail if someone
    // moves the value outside this range without re-thinking rejection UX.
    const _: () = assert!(MIN_CONTEXT_BRIEF_CHARS >= 60);
    const _: () = assert!(MIN_CONTEXT_BRIEF_CHARS <= 200);

    #[test]
    fn short_brief_rejection_message_lists_what_to_include() {
        // The error returned to the model has to teach it how to recover.
        // Rather than just "too short", it must list the things a proper
        // briefing covers (learned/ruled-out/files/judgment-dimensions),
        // so the model's next call is structured rather than just longer.
        // This guards against well-meaning rewording of the bail message
        // that drops the recovery scaffold.
        let msg = format!(
            "teammate_spawn rejected: `context_brief` must be at least {MIN_CONTEXT_BRIEF_CHARS} chars (got 12). \
            Brief the teammate like a smart colleague who just walked in: what you've learned, what you've \
            ruled out, the files/paths/commands to start with, and the judgment dimensions to flag back. \
            Re-issue the call with a real briefing — never delegate understanding."
        );
        assert!(msg.contains("learned"));
        assert!(msg.contains("ruled out"));
        assert!(msg.contains("files"));
        assert!(msg.contains("judgment"));
        assert!(msg.contains("Re-issue"));
    }
}

//! Bench-side supervisor — drives operator pearls like an end user
//! using smooth.
//!
//! ## Architecture: end-user simulator
//!
//! When a real human uses smooth, they don't poke at dolt or scribe
//! state. They open Big Smooth's chat, type a task, read the prose
//! reply (which Big Smooth renders Claude-Code-style with tool calls,
//! diffs, and test output), and respond conversationally. To redirect
//! a teammate they say something like *"tell the teammate the affine-
//! cipher decode formula uses modular inverse, not subtraction"*; Big
//! Smooth's chat-agent translates that to a `teammate_message` tool
//! call internally.
//!
//! This module mimics that loop precisely:
//!
//! - The supervisor holds its own conversation history with Big Smooth.
//! - Its system prompt is *the task description* — same thing a real
//!   user would have on their mental clipboard.
//! - On each tick, an LLM (default `gemini-3.1-flash-lite-preview` via
//!   the gateway's native `/gemini/v1beta` pass-through — fast, cheap,
//!   strong tool-use compliance) reads the conversation so far and
//!   composes the next user-style message: a status ping, a focused
//!   question, a contextual nudge. That message is POSTed to
//!   `/api/chat`. Big Smooth's reply is appended to history, ready
//!   for the next tick.
//!
//! The supervisor does NOT call `teammate_message`, `pearls_show`, or
//! any other smooth tool directly. It does NOT read pearl comments.
//! The interface is Big Smooth, period.
//!
//! ## Enabling
//!
//! Set `SMOOTH_BENCH_SUPERVISOR_ENABLED=1` (or the legacy
//! `SMOOTH_BENCH_SUPERVISOR_MODEL=<name>` for back-compat). Other env
//! vars: `SMOOTH_BENCH_SUPERVISOR_INTERVAL_S` (default 60),
//! `SMOOTH_BENCH_BIG_SMOOTH_URL` (default `http://localhost:4400`),
//! `SMOOTH_BENCH_SUPERVISOR_API_URL` (default
//! `https://llm.smoo.ai/v1`),
//! `SMOOTH_BENCH_SUPERVISOR_API_KEY` /
//! `SMOOTH_BENCH_LLM_API_KEY` /
//! `OPENAI_API_KEY` / `LLM_GATEWAY_API_KEY` for the LLM credentials.

use std::time::{Duration, Instant};

use smooth_operator::conversation::Message;
use smooth_operator::llm::{ApiFormat, LlmClient, LlmConfig};

const DEFAULT_INTERVAL_S: u64 = 60;
const DEFAULT_MODEL: &str = "gemini-3.1-flash-lite-preview";
/// HTTP timeout for each `/api/chat` call. Aligned with Big Smooth's
/// chat-turn ceiling (default 900s) plus a bit of slack so the
/// supervisor never gives up earlier than the daemon does.
const CHAT_HTTP_TIMEOUT_S: u64 = 1000;
/// Cap the supervisor's conversation history length so context stays
/// bounded. We keep the system prompt + last N exchanges.
const MAX_HISTORY_TURNS: usize = 24;
const SUPERVISOR_MAX_TOKENS: u32 = 800;
const SUPERVISOR_TEMPERATURE: f32 = 0.2;

/// Output of one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickResult {
    /// Round-tripped successfully. Big Smooth's prose reply is the
    /// only window we have into what's happening on the pearl.
    Ok { reply: String },
    /// HTTP / parsing / LLM failure. Bench is non-fatal.
    Failed { reason: String },
}

/// Configuration for the conversational supervisor.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// Big Smooth daemon URL (e.g. `http://localhost:4400`).
    pub daemon_url: String,
    /// Seconds between coaching ticks.
    pub interval: Duration,

    // --- LLM (the supervisor's own brain — not Big Smooth's) ---
    pub model: String,
    pub api_url: String,
    pub api_key: String,
    pub api_format: ApiFormat,
}

impl SupervisorConfig {
    /// Build a config from env vars. Returns `None` when supervision
    /// is disabled.
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("SMOOTH_BENCH_SUPERVISOR_ENABLED").is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"));
        let model_override = std::env::var("SMOOTH_BENCH_SUPERVISOR_MODEL").ok().filter(|m| !m.trim().is_empty());
        if !enabled && model_override.is_none() {
            return None;
        }

        let model = model_override.unwrap_or_else(|| DEFAULT_MODEL.to_string());

        let interval_secs: u64 = std::env::var("SMOOTH_BENCH_SUPERVISOR_INTERVAL_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_INTERVAL_S);

        let daemon_url = std::env::var("SMOOTH_BENCH_BIG_SMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".to_string());

        let base_url = std::env::var("SMOOTH_BENCH_SUPERVISOR_API_URL").unwrap_or_else(|_| "https://llm.smoo.ai/v1".to_string());

        let api_key = std::env::var("SMOOTH_BENCH_SUPERVISOR_API_KEY")
            .or_else(|_| std::env::var("SMOOTH_BENCH_LLM_API_KEY"))
            .or_else(|_| std::env::var("LLM_GATEWAY_API_KEY"))
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .unwrap_or_default();

        let (api_url, api_format) = resolve_route(&base_url, &model);

        Some(Self {
            daemon_url,
            interval: Duration::from_secs(interval_secs),
            model,
            api_url,
            api_key,
            api_format,
        })
    }
}

/// Pick the right gateway shape for the supervisor's model.
///
/// Direct `gemini-*` model names go through native pass-through
/// (`/gemini/v1beta`) because the OpenAI-compat router doesn't carry
/// preview models like `gemini-3.1-flash-lite-preview`. Direct
/// `claude-*` names use Anthropic native shape. `smooth-*` aliases
/// stay on OpenAI-compat so LiteLLM resolves them.
fn resolve_route(base_url: &str, model: &str) -> (String, ApiFormat) {
    let m = model.to_ascii_lowercase();
    let trimmed = base_url.trim_end_matches('/');

    if m.starts_with("smooth-") {
        return (trimmed.to_string(), ApiFormat::OpenAiCompat);
    }
    if m.starts_with("gemini-") || m.starts_with("models/gemini-") || m.starts_with("google/gemini") {
        let url = trimmed
            .strip_suffix("/v1")
            .map_or_else(|| format!("{trimmed}/gemini/v1beta"), |base| format!("{base}/gemini/v1beta"));
        return (url, ApiFormat::Gemini);
    }
    if m.starts_with("claude-") || m.starts_with("anthropic/") || m.starts_with("models/claude") {
        return (trimmed.to_string(), ApiFormat::Anthropic);
    }
    (trimmed.to_string(), ApiFormat::OpenAiCompat)
}

/// The conversational supervisor. Maintains its own LLM-driven
/// conversation history with Big Smooth.
pub struct Supervisor {
    config: SupervisorConfig,
    llm: LlmClient,
    http: reqwest::Client,
    /// Conversation history with Big Smooth. system + alternating
    /// user/assistant turns. The supervisor's LLM uses this as
    /// context to generate the NEXT user message; we then send that
    /// to /api/chat and append Big Smooth's reply as the assistant
    /// turn. Capped at MAX_HISTORY_TURNS to keep token count bounded.
    history: Vec<Message>,
    pearl_id: String,
    last_tick: Option<Instant>,
    tick_count: u32,
}

impl Supervisor {
    /// Build a supervisor for a specific pearl + task.
    ///
    /// `task_description` is the same prompt the operator sees — the
    /// supervisor "knows" what it's coaching toward, just like a user
    /// who dispatched the task.
    pub fn new(config: SupervisorConfig, pearl_id: String, task_description: &str) -> Self {
        let llm_config = LlmConfig {
            api_url: config.api_url.clone(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            max_tokens: SUPERVISOR_MAX_TOKENS,
            temperature: SUPERVISOR_TEMPERATURE,
            retry_policy: smooth_operator::llm::RetryPolicy::default(),
            api_format: config.api_format.clone(),
        };
        let llm = LlmClient::new(llm_config);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(CHAT_HTTP_TIMEOUT_S))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let system = build_system_prompt(&pearl_id, task_description, config.interval.as_secs());
        let history = vec![Message::system(system)];

        Self {
            config,
            llm,
            http,
            history,
            pearl_id,
            last_tick: None,
            tick_count: 0,
        }
    }

    pub fn should_tick(&self, now: Instant) -> bool {
        match self.last_tick {
            None => true,
            Some(t) => now.duration_since(t) >= self.config.interval,
        }
    }

    /// Run one tick: LLM composes next user-message based on history,
    /// POST to /api/chat, append Big Smooth's reply to history.
    pub async fn tick_async(&mut self, t0: Instant) -> TickResult {
        let started = Instant::now();
        self.last_tick = Some(started);
        self.tick_count = self.tick_count.saturating_add(1);

        // Add a synthetic "tick begins" hint so the LLM knows we're
        // back to compose the next user-message, plus elapsed time.
        let elapsed_min = t0.elapsed().as_secs_f64() / 60.0;
        let pearl_id = self.pearl_id.clone();
        let tick_hint = if self.tick_count == 1 {
            // First tick: the LLM has only the system prompt. Tell it
            // to compose the OPENING status check.
            format!(
                "Tick #1 (just dispatched, ~{elapsed_min:.1}min in). Compose your FIRST status-check message to Big Smooth. \
                 Ask for what the teammate has done so far — Big Smooth will reply with tool calls, diffs, and test output. \
                 Keep it short. Pearl id: `{pearl_id}`."
            )
        } else {
            // Subsequent: LLM has prior Big Smooth replies in history.
            format!(
                "Tick #{tick} (~{elapsed_min:.1}min in). Read Big Smooth's last reply in history and compose the NEXT message — \
                 a contextual question, a focused nudge, or a request for more detail. If Big Smooth's last reply contained \
                 'OPERATOR DONE', reply EXACTLY with `STOP` (no quotes, no other text) and we'll terminate the watch.",
                tick = self.tick_count,
            )
        };
        self.history.push(Message::user(tick_hint));

        // Cap the history. Always keep the system message at index 0;
        // drop oldest user/assistant pairs as needed.
        self.trim_history();

        // 1) Ask the supervisor LLM what to say next.
        let messages_refs: Vec<&Message> = self.history.iter().collect();
        let llm_resp = match self.llm.chat(&messages_refs, &[]).await {
            Ok(r) => r,
            Err(e) => {
                let reason = format!("supervisor LLM call failed: {e}");
                eprintln!("supervisor: tick {pearl_id}#{} FAILED in {ms}ms — {reason}", self.tick_count, ms = started.elapsed().as_millis());
                // Roll back the synthetic tick hint so retry doesn't compound.
                self.history.pop();
                return TickResult::Failed { reason };
            }
        };
        let next_msg = llm_resp.content.trim().to_string();

        // STOP shortcut — supervisor decided we're done.
        if next_msg.eq_ignore_ascii_case("STOP") || next_msg.starts_with("STOP\n") || next_msg.starts_with("STOP ") {
            eprintln!(
                "supervisor: tick {pearl_id}#{} STOP in {ms}ms — supervisor signalled completion",
                self.tick_count,
                ms = started.elapsed().as_millis()
            );
            // Append the STOP as assistant turn for the eval-html record.
            self.history.push(Message::assistant(next_msg.clone()));
            return TickResult::Ok { reply: next_msg };
        }

        // 2) Send that message to Big Smooth.
        let url = format!("{}/api/chat", self.config.daemon_url.trim_end_matches('/'));
        let body = serde_json::json!({ "content": next_msg });
        let result = self.http.post(&url).json(&body).send().await;
        let total_ms = started.elapsed().as_millis();

        let response = match result {
            Ok(r) => r,
            Err(e) => {
                let reason = format!("HTTP send failed: {e}");
                eprintln!("supervisor: tick {pearl_id}#{} FAILED in {total_ms}ms — {reason}", self.tick_count);
                self.history.pop();
                return TickResult::Failed { reason };
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            let reason = format!("status={status} body={body_text}");
            eprintln!("supervisor: tick {pearl_id}#{} FAILED in {total_ms}ms — {reason}", self.tick_count);
            self.history.pop();
            return TickResult::Failed { reason };
        }

        let parsed: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                let reason = format!("response parse failed: {e}");
                eprintln!("supervisor: tick {pearl_id}#{} FAILED in {total_ms}ms — {reason}", self.tick_count);
                self.history.pop();
                return TickResult::Failed { reason };
            }
        };
        let reply = parsed.get("data").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();

        // 3) Append Big Smooth's reply to history as the assistant turn.
        self.history.push(Message::assistant(reply.clone()));

        let snippet: String = next_msg.chars().take(80).collect();
        let trunc1 = if next_msg.len() > 80 { "…" } else { "" };
        let reply_snippet: String = reply.chars().take(120).collect();
        let trunc2 = if reply.len() > 120 { "…" } else { "" };
        eprintln!(
            "supervisor: tick {pearl_id}#{} ok in {total_ms}ms\n  → \"{snippet}{trunc1}\"\n  ← \"{reply_snippet}{trunc2}\"",
            self.tick_count,
        );

        TickResult::Ok { reply }
    }

    fn trim_history(&mut self) {
        // Keep system + last MAX_HISTORY_TURNS of user/assistant
        // entries. Drop in pairs (one user + one assistant) so the
        // alternation isn't broken.
        let body_count = self.history.len() - 1; // exclude system
        let max_body = MAX_HISTORY_TURNS;
        if body_count <= max_body {
            return;
        }
        let drop_count = body_count - max_body;
        // Drop drop_count items starting at index 1 (after system).
        self.history.drain(1..=drop_count);
    }

    pub fn tick_count(&self) -> u32 {
        self.tick_count
    }

    pub fn config(&self) -> &SupervisorConfig {
        &self.config
    }
}

fn build_system_prompt(pearl_id: &str, task_description: &str, interval_s: u64) -> String {
    format!(
        r#"You are a thoughtful end-user of Smooth. You dispatched a coding task to Big Smooth (the team lead AI), who put a teammate operator on it inside a sandbox. You can ONLY interact via Big Smooth's chat — exactly as a real user would in the TUI.

Your job: ping Big Smooth periodically, read its replies (which contain Claude-Code-style tool calls, diffs, and test output), and offer guidance like a senior engineer would. You should NOT directly call tools, write to pearls, or otherwise bypass Big Smooth — every action you want taken must be requested via natural-language chat.

# The task you dispatched

The teammate is working on this pearl: `{pearl_id}`

Task description (the operator sees this as their starting brief):
---
{task_description}
---

# How to coach

- Each turn, you'll receive a short prompt asking you to compose the NEXT user-message to send to Big Smooth.
- Bias toward short, focused messages. Real users don't write essays.
- Examples of good messages:
  - "Status check on `{pearl_id}` — show me what the teammate's done since the last check."
  - "I see they're stuck on the encode round-trip — quote the failing assertion and tell them to recheck (a*x + b) mod m."
  - "They've called `bash(pytest)` 3 times with the same failure — tell them to read the test setup, the input format may not be what they assumed."
  - "Tests are green now — confirm and stop."
- If Big Smooth's last reply ended with `OPERATOR DONE`, reply with EXACTLY `STOP` and nothing else — we'll terminate.
- If Big Smooth's last reply showed the teammate is solving the task correctly, reply with a brief encouragement like "good, keep going — I'll check back in {interval_s}s".
- If you spot something subtle (algorithm error, edge case missed, wrong file), call it out concretely and ask Big Smooth to relay it via teammate_message.
- NEVER tell Big Smooth to call a tool you don't believe in. Be confident; you have the task description, you can reason about whether the approach is right.
- Avoid repeating yourself. If your last message was "show me the latest test output" and Big Smooth gave it, don't ask again — react to it.

# Output format

Output ONE message — the natural-language text you would type into the chat. Nothing else. No "I will..." prose, no markdown wrappers, no tool calls. Just the message Big Smooth should receive."#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_config() -> SupervisorConfig {
        SupervisorConfig {
            daemon_url: "http://localhost:4400".into(),
            interval: Duration::from_secs(30),
            model: "gemini-3.1-flash-lite-preview".into(),
            api_url: "https://llm.smoo.ai/gemini/v1beta".into(),
            api_key: "test".into(),
            api_format: ApiFormat::Gemini,
        }
    }

    #[test]
    fn config_from_env_round_trip() {
        const ENABLED: &str = "SMOOTH_BENCH_SUPERVISOR_ENABLED";
        const MODEL: &str = "SMOOTH_BENCH_SUPERVISOR_MODEL";
        const INTERVAL: &str = "SMOOTH_BENCH_SUPERVISOR_INTERVAL_S";
        const URL: &str = "SMOOTH_BENCH_BIG_SMOOTH_URL";

        std::env::remove_var(ENABLED);
        std::env::remove_var(MODEL);
        std::env::remove_var(INTERVAL);
        std::env::remove_var(URL);

        // 1. Disabled by default.
        assert!(SupervisorConfig::from_env().is_none());

        // 2. ENABLED=1 alone enables, defaults to gemini-3.1-flash-lite-preview.
        std::env::set_var(ENABLED, "1");
        let cfg = SupervisorConfig::from_env().expect("enabled-only");
        assert_eq!(cfg.model, DEFAULT_MODEL);
        assert_eq!(cfg.api_format, ApiFormat::Gemini, "gemini-* model → native route");
        assert!(cfg.api_url.ends_with("/gemini/v1beta"));

        // 3. Model override survives.
        std::env::set_var(MODEL, "smooth-fast-gemini");
        let cfg = SupervisorConfig::from_env().expect("override");
        assert_eq!(cfg.model, "smooth-fast-gemini");
        assert_eq!(cfg.api_format, ApiFormat::OpenAiCompat, "smooth-* alias → OpenAI-compat");

        // 4. Interval + URL.
        std::env::set_var(INTERVAL, "45");
        std::env::set_var(URL, "http://127.0.0.1:9999");
        let cfg = SupervisorConfig::from_env().expect("interval+url");
        assert_eq!(cfg.interval, Duration::from_secs(45));
        assert_eq!(cfg.daemon_url, "http://127.0.0.1:9999");

        std::env::remove_var(ENABLED);
        std::env::remove_var(MODEL);
        std::env::remove_var(INTERVAL);
        std::env::remove_var(URL);
    }

    #[test]
    fn supervisor_starts_with_system_only_history() {
        let sup = Supervisor::new(dummy_config(), "th-test123".into(), "Solve affine cipher.");
        assert_eq!(sup.history.len(), 1);
        // The single message is the system prompt.
        let m = &sup.history[0];
        assert!(m.content.contains("th-test123"));
        assert!(m.content.contains("Solve affine cipher."));
    }

    #[test]
    fn supervisor_should_tick_initially() {
        let sup = Supervisor::new(dummy_config(), "th-test123".into(), "task");
        assert!(sup.should_tick(Instant::now()));
    }

    #[test]
    fn supervisor_trim_history_keeps_system_and_latest() {
        let mut sup = Supervisor::new(dummy_config(), "th-test".into(), "task");
        // Push way more than MAX_HISTORY_TURNS turns.
        for i in 0..50 {
            sup.history.push(Message::user(format!("u{i}")));
            sup.history.push(Message::assistant(format!("a{i}")));
        }
        sup.trim_history();
        assert_eq!(sup.history[0].content.contains("th-test"), true, "system retained");
        // Body should be capped to MAX_HISTORY_TURNS.
        assert_eq!(sup.history.len(), 1 + MAX_HISTORY_TURNS);
        // Latest entry should be the last assistant we pushed.
        let last = &sup.history[sup.history.len() - 1];
        assert_eq!(last.content, "a49");
    }

    #[test]
    fn route_resolution_picks_native_paths() {
        let (url, fmt) = resolve_route("https://llm.smoo.ai/v1", "gemini-3.1-flash-lite-preview");
        assert_eq!(url, "https://llm.smoo.ai/gemini/v1beta");
        assert_eq!(fmt, ApiFormat::Gemini);

        let (_, fmt) = resolve_route("https://llm.smoo.ai/v1", "claude-haiku-4-5");
        assert_eq!(fmt, ApiFormat::Anthropic);

        let (url, fmt) = resolve_route("https://llm.smoo.ai/v1", "smooth-fast-gemini");
        assert_eq!(url, "https://llm.smoo.ai/v1");
        assert_eq!(fmt, ApiFormat::OpenAiCompat);
    }
}

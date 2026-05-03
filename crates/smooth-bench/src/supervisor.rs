//! Bench-side supervisor agent — drives operator pearls like a coach.
//!
//! Pre-supervisor bench: dispatches a task → polls the pearl for `[IDLE]`
//! or status=closed → returns. The operator runs alone for 25-30 minutes
//! whether it's making progress or stuck.
//!
//! With supervisor: every `interval_s`, an LLM (default Gemini 3.1 Flash
//! Lite via the new `/gemini/v1beta` native pass-through) reads recent
//! pearl comments + pearl status, decides whether the operator is making
//! progress, and posts a `[STEERING:GUIDANCE]` comment when it's time to
//! nudge. The runner's mailbox poller picks the steering up and injects
//! it as a system message in the agent loop — the same path the daemon's
//! `teammate_message` tool already uses.
//!
//! Disabled by default. Enable by setting `SMOOTH_BENCH_SUPERVISOR_MODEL`
//! to a routable model name. `SMOOTH_BENCH_SUPERVISOR_INTERVAL_S` controls
//! the LLM call cadence (default 90 s — supervisor decides ~10 times per
//! 15-min run, not every 5-second poll tick).
//!
//! The supervisor does NOT decide the operator is done — that signal still
//! comes from `[IDLE]` / pearl status. It only injects coaching when the
//! operator is mid-flight.

use std::time::{Duration, Instant};

use smooth_operator::conversation::Message;
use smooth_operator::llm::{ApiFormat, LlmClient, LlmConfig};
use smooth_pearls::{PearlComment, PearlStatus, PearlStore};

const DEFAULT_INTERVAL_S: u64 = 90;
const DEFAULT_SUPERVISOR_MAX_TOKENS: u32 = 1024;
const DEFAULT_SUPERVISOR_TEMPERATURE: f32 = 0.0;
/// Lower bound on time between successive steering posts. Even if the
/// supervisor decides to steer twice in a row, we cool off to avoid
/// flooding the runner's mailbox.
const STEERING_COOLDOWN_S: u64 = 60;

/// Decision returned by the supervisor LLM for a given pearl state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorDecision {
    /// Keep watching — operator is making progress, no steering needed.
    Continue,
    /// Operator needs a nudge. Post the message verbatim as a
    /// `[STEERING:GUIDANCE]` comment.
    Steer(String),
    /// Operator is done — supervisor saw a clean exit before the bench's
    /// own quiescence heuristic fired. Bench can stop polling early.
    /// (Reserved for future use; today the bench's own [IDLE] / Closed
    /// detection still drives termination.)
    Stop,
}

/// Configuration for the supervisor.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub model: String,
    pub api_url: String,
    pub api_key: String,
    pub api_format: ApiFormat,
    pub interval: Duration,
    pub max_tokens: u32,
    pub temperature: f32,
}

impl SupervisorConfig {
    /// Build a config from env vars. Returns `None` when supervision is
    /// disabled (no `SMOOTH_BENCH_SUPERVISOR_MODEL` set).
    ///
    /// Env vars:
    /// - `SMOOTH_BENCH_SUPERVISOR_MODEL` — model name (e.g.
    ///   `smooth-fast-gemini`, `gemini-3.1-flash-lite-preview`,
    ///   `claude-haiku-4-5`). Required to enable.
    /// - `SMOOTH_BENCH_SUPERVISOR_API_URL` — gateway base URL. Defaults to
    ///   `https://llm.smoo.ai/v1`. The supervisor auto-rewrites to
    ///   `<base>/gemini/v1beta` for Gemini-family models, parallel to the
    ///   operator-runner's family-aware routing.
    /// - `SMOOTH_BENCH_SUPERVISOR_API_KEY` / `LLM_GATEWAY_API_KEY` /
    ///   `OPENAI_API_KEY` — bearer credential, in priority order.
    /// - `SMOOTH_BENCH_SUPERVISOR_INTERVAL_S` — seconds between supervisor
    ///   ticks (default 90).
    pub fn from_env() -> Option<Self> {
        let model = std::env::var("SMOOTH_BENCH_SUPERVISOR_MODEL").ok()?;
        if model.trim().is_empty() {
            return None;
        }

        let base_url = std::env::var("SMOOTH_BENCH_SUPERVISOR_API_URL").unwrap_or_else(|_| "https://llm.smoo.ai/v1".to_string());

        let api_key = std::env::var("SMOOTH_BENCH_SUPERVISOR_API_KEY")
            .or_else(|_| std::env::var("LLM_GATEWAY_API_KEY"))
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .unwrap_or_default();

        let interval_secs: u64 = std::env::var("SMOOTH_BENCH_SUPERVISOR_INTERVAL_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_INTERVAL_S);

        let (api_url, api_format) = resolve_supervisor_route(&base_url, &model);

        Some(Self {
            model,
            api_url,
            api_key,
            api_format,
            interval: Duration::from_secs(interval_secs),
            max_tokens: DEFAULT_SUPERVISOR_MAX_TOKENS,
            temperature: DEFAULT_SUPERVISOR_TEMPERATURE,
        })
    }
}

/// Family-aware route selection — parallel to operator-runner's logic.
/// Gemini family → native pass-through. Anthropic family → /v1/messages
/// (LiteLLM resolves smooth-* aliases on this route). Other → OpenAI-compat.
fn resolve_supervisor_route(base_url: &str, model: &str) -> (String, ApiFormat) {
    let m = model.to_ascii_lowercase();
    let trimmed = base_url.trim_end_matches('/');

    if is_gemini_family(&m) {
        let url = trimmed
            .strip_suffix("/v1")
            .map_or_else(|| format!("{trimmed}/gemini/v1beta"), |base| format!("{base}/gemini/v1beta"));
        return (url, ApiFormat::Gemini);
    }
    if is_anthropic_family(&m) {
        return (trimmed.to_string(), ApiFormat::Anthropic);
    }
    (trimmed.to_string(), ApiFormat::OpenAiCompat)
}

fn is_gemini_family(m: &str) -> bool {
    if m.contains("gemini") {
        return true;
    }
    matches!(m, "smooth-fast-gemini" | "smooth-judge-gemini" | "smooth-summarize")
}

fn is_anthropic_family(m: &str) -> bool {
    if m.contains("claude") || m.contains("anthropic") || m.contains("haiku") || m.contains("sonnet") || m.contains("opus") {
        return true;
    }
    matches!(m, "smooth-judge" | "smooth-fast-haiku" | "smooth-reviewing-haiku" | "smooth-judge-haiku")
}

/// The supervisor agent — wraps an LLM client and decides when to steer.
pub struct Supervisor {
    config: SupervisorConfig,
    client: LlmClient,
    last_tick: Option<Instant>,
    last_steer: Option<Instant>,
    steer_count: u32,
    last_seen_comment_count: usize,
}

impl Supervisor {
    /// Build a supervisor from config. Failure to construct the LLM client
    /// is non-fatal — the supervisor falls back to a no-op (returns
    /// Continue on every tick). Bench supervision is best-effort by design.
    pub fn new(config: SupervisorConfig) -> Self {
        let llm_config = LlmConfig {
            api_url: config.api_url.clone(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            retry_policy: smooth_operator::llm::RetryPolicy::default(),
            api_format: config.api_format.clone(),
        };
        let client = LlmClient::new(llm_config);
        Self {
            config,
            client,
            last_tick: None,
            last_steer: None,
            steer_count: 0,
            last_seen_comment_count: 0,
        }
    }

    /// Whether this supervisor would tick now if `tick_async` were called.
    /// Splits the time-gate from the LLM call so the bench's poll loop can
    /// skip the borrow & async tax on most ticks.
    pub fn should_tick(&self, now: Instant) -> bool {
        match self.last_tick {
            None => true,
            Some(t) => now.duration_since(t) >= self.config.interval,
        }
    }

    /// Run one supervisor tick. Reads pearl state, calls the LLM, and
    /// posts a `[STEERING:GUIDANCE]` comment when the LLM decides to
    /// steer. Returns the decision so the caller can log / count.
    ///
    /// Errors (LLM failures, store failures) are logged via `tracing` and
    /// converted to `Continue` — bench supervision is non-fatal.
    pub async fn tick_async(&mut self, store: &PearlStore, pearl_id: &str, t0: Instant) -> SupervisorDecision {
        self.last_tick = Some(Instant::now());

        // Cool off: even if the LLM wants to steer twice in a row, don't
        // flood the runner's mailbox.
        if let Some(last) = self.last_steer {
            if last.elapsed() < Duration::from_secs(STEERING_COOLDOWN_S) {
                return SupervisorDecision::Continue;
            }
        }

        let comments = match store.get_comments(pearl_id) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("supervisor: failed to read comments on {pearl_id}: {e}");
                return SupervisorDecision::Continue;
            }
        };
        let pearl_status = store.get(pearl_id).ok().flatten().map(|p| p.status);

        // Don't supervise terminal states.
        if matches!(pearl_status, Some(PearlStatus::Closed)) {
            return SupervisorDecision::Stop;
        }

        let new_comments = comments.len().saturating_sub(self.last_seen_comment_count);
        self.last_seen_comment_count = comments.len();

        let context = build_context(pearl_id, &comments, pearl_status, t0.elapsed(), new_comments);
        let messages = vec![Message::system(SUPERVISOR_SYSTEM_PROMPT), Message::user(context)];
        let messages_refs: Vec<&Message> = messages.iter().collect();

        let response = match self.client.chat(&messages_refs, &[]).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("supervisor: LLM call failed for {pearl_id}: {e}");
                return SupervisorDecision::Continue;
            }
        };

        let decision = parse_supervisor_response(&response.content);
        if let SupervisorDecision::Steer(ref msg) = decision {
            let body = format!("[STEERING:GUIDANCE] {msg}");
            match store.add_comment(pearl_id, &body) {
                Ok(_) => {
                    self.last_steer = Some(Instant::now());
                    self.steer_count += 1;
                    eprintln!("supervisor: steered {pearl_id} ({}#{}): {msg}", self.config.model, self.steer_count);
                }
                Err(e) => {
                    eprintln!("supervisor: failed to write steering on {pearl_id}: {e}");
                    return SupervisorDecision::Continue;
                }
            }
        }
        decision
    }

    pub fn steer_count(&self) -> u32 {
        self.steer_count
    }

    pub fn config(&self) -> &SupervisorConfig {
        &self.config
    }
}

/// Compose the user-facing context that the supervisor LLM sees on every
/// tick. Includes the most recent comments (capped to last 30 to fit the
/// 5-min cache window of cheap supervisor models), pearl status, elapsed
/// time, and a delta count since the last tick.
fn build_context(pearl_id: &str, comments: &[PearlComment], pearl_status: Option<PearlStatus>, elapsed: Duration, new_since_last_tick: usize) -> String {
    let total = comments.len();
    let tail_count = 30.min(total);
    let tail = &comments[total.saturating_sub(tail_count)..];

    let mut buf = String::new();
    buf.push_str(&format!("Pearl: {pearl_id}\n"));
    buf.push_str(&format!("Status: {:?}\n", pearl_status));
    buf.push_str(&format!("Elapsed: {:.0}s\n", elapsed.as_secs_f64()));
    buf.push_str(&format!(
        "Total comments: {total} (last {tail_count} shown, +{new_since_last_tick} since last tick)\n"
    ));
    buf.push_str("---\n");
    for c in tail {
        // Strip leading whitespace; cap each comment at 800 chars so the
        // context stays well below 8K total even on tool-result-heavy runs.
        let body = c.content.trim_start();
        let snippet: String = body.chars().take(800).collect();
        buf.push_str(&format!("[{}] {}\n", c.created_at.format("%H:%M:%S"), snippet));
        if body.len() > 800 {
            buf.push_str("    …(truncated)\n");
        }
    }
    buf
}

/// Parse the supervisor LLM's response into a decision.
///
/// Expected formats (case-insensitive on the keyword):
///   `CONTINUE` (anywhere on the first non-empty line) → `Continue`
///   `STOP` → `Stop`
///   `STEER: <message>` → `Steer(message)` (multi-line OK)
///
/// Defensive: a model that drifts and emits prose without the keyword
/// gets parsed as `Continue` — better to miss a steer than to inject
/// noise into the operator's mailbox.
pub fn parse_supervisor_response(text: &str) -> SupervisorDecision {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return SupervisorDecision::Continue;
    }
    // Find the first non-empty line.
    let first_line = trimmed.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let upper = first_line.to_ascii_uppercase();

    if upper.starts_with("STEER:") {
        let rest = first_line[6..].trim().to_string();
        // Allow multi-line messages: take everything from after STEER:
        // through the rest of the response.
        let after_first_line = trimmed.split_once('\n').map(|(_, r)| r).unwrap_or("");
        let combined = if after_first_line.trim().is_empty() {
            rest
        } else {
            format!("{rest}\n{}", after_first_line.trim())
        };
        if combined.is_empty() {
            return SupervisorDecision::Continue;
        }
        return SupervisorDecision::Steer(combined);
    }
    if upper.starts_with("STOP") {
        return SupervisorDecision::Stop;
    }
    // Default: Continue. (Includes "CONTINUE", drifting prose, etc.)
    SupervisorDecision::Continue
}

const SUPERVISOR_SYSTEM_PROMPT: &str = r#"You are a supervisor agent watching a coding operator solve a benchmark task.

The operator runs autonomously inside a microVM. You see the pearl's recent
comments (PROGRESS heartbeats, CHAT, STEERING you've already posted, METRICS,
IDLE). You can post one short steering message per tick. Do not micromanage
— the operator is competent and your nudges interrupt its flow.

Decide one of three actions per tick:

1. CONTINUE — operator is making progress, no nudge needed. This is the
   default. Use it when:
   - PROGRESS heartbeats are landing and tool calls are advancing
   - Test output shows shrinking failure count or new green tests
   - Operator is mid-edit on a file related to the task

2. STEER: <one-or-two-sentence message> — operator is stuck or off-track.
   Reasonable triggers:
   - Same tool call repeated 3+ times with same args (suggest an alternative)
   - Bash returned "command not found" (suggest install / apk add / mise)
   - 90+ s elapsed with no PROGRESS after a heartbeat-emitting period
   - Test output shows a specific failure that hints at a fix
     ("import error in line N — read the file, the symbol is misspelled")
   - Operator wrote tests instead of fixing source (gentle reminder: bench
     scores green only when the original tests pass against the operator's
     source changes)
   Keep the message concrete and actionable. No prose. No "great job".

3. STOP — operator clearly finished (test output 100% green, IDLE posted,
   METRICS reported). Bench will detect this on its own; only emit STOP
   when you're 100% sure to save the bench a poll cycle.

Respond on a single line:
    CONTINUE
    STOP
    STEER: <your message here>

Anything else parses as CONTINUE."#;

#[cfg(test)]
mod tests {
    use super::*;

    // The env-driven config tests share process-global state
    // (`std::env::set_var` is not thread-safe and collides under cargo's
    // default parallel test runner). Consolidated into one serial test
    // rather than pulling in serial_test as a dep.
    #[test]
    fn config_from_env_round_trip() {
        const MODEL: &str = "SMOOTH_BENCH_SUPERVISOR_MODEL";
        const URL: &str = "SMOOTH_BENCH_SUPERVISOR_API_URL";
        const INTERVAL: &str = "SMOOTH_BENCH_SUPERVISOR_INTERVAL_S";

        // 1. Disabled when unset.
        std::env::remove_var(MODEL);
        std::env::remove_var(URL);
        std::env::remove_var(INTERVAL);
        assert!(SupervisorConfig::from_env().is_none(), "expected disabled when MODEL unset");

        // 2. Disabled when whitespace-only.
        std::env::set_var(MODEL, "   ");
        assert!(SupervisorConfig::from_env().is_none(), "expected disabled when MODEL is whitespace");

        // 3. Gemini native route picked up.
        std::env::set_var(MODEL, "smooth-fast-gemini");
        std::env::set_var(URL, "https://llm.smoo.ai/v1");
        let cfg = SupervisorConfig::from_env().expect("gemini config");
        assert_eq!(cfg.api_url, "https://llm.smoo.ai/gemini/v1beta");
        assert_eq!(cfg.api_format, ApiFormat::Gemini);
        // Default interval when INTERVAL unset.
        assert_eq!(cfg.interval, Duration::from_secs(DEFAULT_INTERVAL_S));

        // 4. Custom interval.
        std::env::set_var(INTERVAL, "30");
        let cfg = SupervisorConfig::from_env().expect("custom interval");
        assert_eq!(cfg.interval, Duration::from_secs(30));

        // 5. Anthropic route — chat_anthropic appends /messages itself, so
        // we keep /v1 as the base.
        std::env::set_var(MODEL, "claude-haiku-4-5");
        let cfg = SupervisorConfig::from_env().expect("anthropic config");
        assert_eq!(cfg.api_format, ApiFormat::Anthropic);
        assert_eq!(cfg.api_url, "https://llm.smoo.ai/v1");

        // Clean up.
        std::env::remove_var(MODEL);
        std::env::remove_var(URL);
        std::env::remove_var(INTERVAL);
    }

    #[test]
    fn parse_continue_default() {
        assert_eq!(parse_supervisor_response("CONTINUE"), SupervisorDecision::Continue);
        assert_eq!(parse_supervisor_response("continue"), SupervisorDecision::Continue);
        assert_eq!(parse_supervisor_response(""), SupervisorDecision::Continue);
        // Drifting prose without keyword → Continue (defensive default).
        assert_eq!(parse_supervisor_response("hmm let me think"), SupervisorDecision::Continue);
    }

    #[test]
    fn parse_stop() {
        assert_eq!(parse_supervisor_response("STOP"), SupervisorDecision::Stop);
        assert_eq!(parse_supervisor_response("stop\n"), SupervisorDecision::Stop);
    }

    #[test]
    fn parse_steer_single_line() {
        let r = parse_supervisor_response("STEER: try running pytest with -v");
        assert_eq!(r, SupervisorDecision::Steer("try running pytest with -v".to_string()));
    }

    #[test]
    fn parse_steer_multi_line() {
        let r = parse_supervisor_response("STEER: try this approach\n  - read foo.py first\n  - then patch bar.py");
        match r {
            SupervisorDecision::Steer(msg) => {
                assert!(msg.starts_with("try this approach"));
                assert!(msg.contains("read foo.py first"));
                assert!(msg.contains("patch bar.py"));
            }
            _ => panic!("expected Steer"),
        }
    }

    #[test]
    fn parse_steer_empty_message_is_continue() {
        // "STEER:" with nothing after it shouldn't post empty steering.
        assert_eq!(parse_supervisor_response("STEER:"), SupervisorDecision::Continue);
    }

    #[test]
    fn parse_skips_leading_blanks() {
        let r = parse_supervisor_response("\n\n\nSTEER: install jq");
        assert_eq!(r, SupervisorDecision::Steer("install jq".to_string()));
    }

    #[test]
    fn family_helpers_match_real_models() {
        assert!(is_gemini_family("gemini-3.1-flash-lite-preview"));
        assert!(is_gemini_family("smooth-fast-gemini"));
        assert!(is_anthropic_family("claude-haiku-4-5"));
        assert!(is_anthropic_family("smooth-judge"));
        assert!(!is_gemini_family("smooth-coding"));
        assert!(!is_anthropic_family("kimi-k2-thinking"));
    }

    #[test]
    fn build_context_caps_comments() {
        // Make 50 comments; build_context should only show the last 30.
        let now = chrono::Utc::now();
        let comments: Vec<PearlComment> = (0..50)
            .map(|i| PearlComment {
                id: format!("c{i}"),
                pearl_id: "th-aaaaaa".into(),
                content: format!("comment number {i}"),
                created_at: now,
            })
            .collect();
        let ctx = build_context("th-aaaaaa", &comments, Some(PearlStatus::InProgress), Duration::from_secs(120), 5);
        // Should reference total + tail size; should include "comment number 49" but not "comment number 0".
        assert!(ctx.contains("Total comments: 50"));
        assert!(ctx.contains("comment number 49"));
        assert!(!ctx.contains("comment number 0\n"));
    }

    #[test]
    fn build_context_truncates_long_comments() {
        let now = chrono::Utc::now();
        let huge = "x".repeat(2000);
        let comments = vec![PearlComment {
            id: "c1".into(),
            pearl_id: "th-aaaaaa".into(),
            content: huge,
            created_at: now,
        }];
        let ctx = build_context("th-aaaaaa", &comments, None, Duration::from_secs(0), 0);
        assert!(ctx.contains("…(truncated)"));
    }
}

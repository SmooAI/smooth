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

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use smooth_operator::conversation::Message;
use smooth_operator::llm::{ApiFormat, LlmClient, LlmConfig};
use smooth_pearls::{PearlComment, PearlStatus, PearlStore};
use tokio::sync::Mutex as AsyncMutex;

/// Global lock serialising all supervisor [STEERING] writes across
/// parallel pearls. The dolt server is single-writer; under
/// SMOOTH_BENCH_PARALLELISM=3 the 3 concurrent supervisors used to
/// race the manifest lock and 5/10 of them would fail to write.
/// Holding this around `add_comment` lets at most one supervisor
/// touch dolt at a time — a few hundred ms of serialisation per tick,
/// well within the 30s interval.
static STEER_WRITE_LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
fn steer_write_lock() -> &'static AsyncMutex<()> {
    STEER_WRITE_LOCK.get_or_init(|| AsyncMutex::new(()))
}

/// Tick cadence. 30s strikes a balance: short enough that a stuck
/// operator gets unstuck inside a single iteration window (operator
/// iterations are ~30-60s on warm models), long enough that we don't
/// burn LLM cost spamming CONTINUEs. Was 90s in the first version,
/// dropped after take 9 showed zero steers across 10 tasks — supervisor
/// was waking up after the operator had already moved on.
const DEFAULT_INTERVAL_S: u64 = 30;
const DEFAULT_SUPERVISOR_MAX_TOKENS: u32 = 1024;
const DEFAULT_SUPERVISOR_TEMPERATURE: f32 = 0.0;
/// Lower bound on time between successive steering posts. Was 60s; cut
/// to 20s after take 9. Operator iterations cycle every 30-60s, so a
/// 60s cooldown often means a steer can't land until 2 iterations
/// later — too late on dispatches that wedge fast. 20s lets a follow-
/// up steer chase the first one if the LLM keeps deciding STEER.
const STEERING_COOLDOWN_S: u64 = 20;

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

/// Route selection for the supervisor's LLM calls.
///
/// **Important asymmetry vs the operator-runner**: the supervisor doesn't
/// use tools (`tools: &[]` on every chat call). The whole reason
/// operator-runner forces native Anthropic / Gemini routes is that LiteLLM's
/// OpenAI-compat translation mangles multi-turn tool calls. With no tools,
/// the OpenAI-compat path works fine — and it's the only route where
/// LiteLLM resolves `smooth-*` aliases. The native pass-through routes
/// (`/gemini/v1beta`, `/anthropic/v1`) forward dumbly to upstream and 404
/// on smooth-* names.
///
/// So: the supervisor only flips to native shape when the caller has
/// supplied a *direct* upstream model name (e.g. `gemini-3.1-flash-lite-
/// preview`, `claude-haiku-4-5`). For smooth-* aliases (`smooth-fast-gemini`,
/// `smooth-judge`) we stay on OpenAI-compat so LiteLLM can do the lookup.
fn resolve_supervisor_route(base_url: &str, model: &str) -> (String, ApiFormat) {
    let m = model.to_ascii_lowercase();
    let trimmed = base_url.trim_end_matches('/');

    // smooth-* aliases must go through LiteLLM's OpenAI-compat router so
    // the alias resolves. Tools-free supervisor calls don't hit the
    // tool-translation bug.
    if m.starts_with("smooth-") {
        return (trimmed.to_string(), ApiFormat::OpenAiCompat);
    }

    // Direct upstream model names: prefer native shape so the supervisor
    // can use Gemini 3.x preview models that LiteLLM's DB doesn't yet
    // expose via /v1/chat/completions.
    if is_direct_gemini(&m) {
        let url = trimmed
            .strip_suffix("/v1")
            .map_or_else(|| format!("{trimmed}/gemini/v1beta"), |base| format!("{base}/gemini/v1beta"));
        return (url, ApiFormat::Gemini);
    }
    if is_direct_anthropic(&m) {
        return (trimmed.to_string(), ApiFormat::Anthropic);
    }
    (trimmed.to_string(), ApiFormat::OpenAiCompat)
}

fn is_direct_gemini(m: &str) -> bool {
    m.starts_with("gemini-") || m.starts_with("models/gemini-") || m.starts_with("google/gemini")
}

fn is_direct_anthropic(m: &str) -> bool {
    m.starts_with("claude-") || m.starts_with("anthropic/") || m.starts_with("models/claude")
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
        let tick_started = Instant::now();
        self.last_tick = Some(tick_started);

        // Cool off: even if the LLM wants to steer twice in a row, don't
        // flood the runner's mailbox.
        if let Some(last) = self.last_steer {
            if last.elapsed() < Duration::from_secs(STEERING_COOLDOWN_S) {
                eprintln!(
                    "supervisor: tick {pearl_id} cooldown ({}s left)",
                    STEERING_COOLDOWN_S.saturating_sub(last.elapsed().as_secs())
                );
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
            eprintln!("supervisor: tick {pearl_id} STOP (pearl closed)");
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
        let elapsed_ms = tick_started.elapsed().as_millis();
        match &decision {
            SupervisorDecision::Continue => {
                // One-line per-tick log so silent CONTINUE runs are visible
                // in the bench log. Kept terse — ~20 bytes per tick.
                eprintln!("supervisor: tick {pearl_id} CONTINUE ({elapsed_ms}ms, +{new_comments} comments)");
            }
            SupervisorDecision::Stop => {
                eprintln!("supervisor: tick {pearl_id} STOP ({elapsed_ms}ms)");
            }
            SupervisorDecision::Steer(msg) => {
                let body = format!("[STEERING:GUIDANCE] {msg}");
                // Hold the global steer-write lock across the whole
                // retry loop so two parallel supervisors can't fight
                // for the dolt manifest at the same moment.
                let _write_guard = steer_write_lock().lock().await;
                // Dolt manifest contention can still leak through
                // (daemon-side heartbeat writes don't go through this
                // lock). Retry with jittered exponential backoff before
                // giving up — contention windows are typically sub-second.
                let mut attempt = 0_u32;
                let result = loop {
                    match store.add_comment(pearl_id, &body) {
                        Ok(_pearl) => break Ok(()),
                        Err(e) => {
                            let m = e.to_string();
                            // Only retry on the manifest/read-only class
                            // of error. Real problems (missing pearl,
                            // perm error, disk full) shouldn't burn retries.
                            let retryable = m.contains("read only") || m.contains("manifest") || m.contains("Error 1105");
                            if !retryable || attempt >= 10 {
                                break Err(e);
                            }
                            // Backoff with jitter so the 3 concurrent
                            // supervisors don't lock-step their retries.
                            // Base: 50, 100, 200, 400, 800, 1600, 3200,
                            // 6400, 12800, 25600 ms (capped). Jitter:
                            // ±50% via the LSB of an Instant nanosecond.
                            let base = (50_u64).checked_shl(attempt).unwrap_or(25_600).min(25_600);
                            let nanos = Instant::now().elapsed().subsec_nanos() as u64;
                            let jitter = nanos % (base / 2 + 1);
                            let backoff = base / 2 + jitter; // 50%..150% of base
                            tokio::time::sleep(Duration::from_millis(backoff)).await;
                            attempt += 1;
                        }
                    }
                };
                match result {
                    Ok(()) => {
                        self.last_steer = Some(Instant::now());
                        self.steer_count += 1;
                        let retry_note = if attempt > 0 { format!(", {attempt} retries") } else { String::new() };
                        eprintln!(
                            "supervisor: steered {pearl_id} ({}#{}, {elapsed_ms}ms{retry_note}): {msg}",
                            self.config.model, self.steer_count
                        );
                    }
                    Err(e) => {
                        eprintln!("supervisor: failed to write steering on {pearl_id} after {attempt} retries: {e}");
                        return SupervisorDecision::Continue;
                    }
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

const SUPERVISOR_SYSTEM_PROMPT: &str = r#"You are a supervisor coaching a coding operator solve a benchmark task. The
operator wastes time when left alone — your job is to STEER it toward
convergence as often as you can justify. Bias toward steering.

The operator runs autonomously inside a microVM. You see the pearl's recent
comments (PROGRESS heartbeats, CHAT, STEERING you've already posted, METRICS,
IDLE). Each STEER you post is injected as a system message in the operator's
next iteration — operators tend to act on them. Use that.

Tasks are scored on whether the FULL test suite passes. The operator has
~10-15 minutes wall-clock to converge before the run is considered failed.
Treat that as a hard ceiling: if the operator is at 8+ minutes elapsed and
not yet showing green tests, it's behind schedule and you should push.

Decide one of three actions per tick:

1. STEER: <one or two sharp sentences> — DEFAULT WHEN IN DOUBT. Triggers
   you should act on (not exhaustive — if you spot something not on this
   list, steer anyway):
   - Same tool / file edited 2+ times in the last few comments → quote
     the failing test back and tell the operator to read it carefully
   - Bash returned "command not found", non-zero exit, or compile error →
     name the fix ("apk add jq", "use go.mod 1.21", "import is misspelled
     on line N")
   - Test output shows a specific failure that hints at the fix → quote
     the failure verbatim and point at the file/line
   - 60+ seconds since the last PROGRESS comment → ask "what are you
     working on right now?" — this nudges a stalled operator to emit
     more state
   - Operator wrote test files instead of fixing source → reminder: bench
     scores green only when the original tests pass; delete the new
     tests and fix the source
   - 8+ minutes elapsed with no green tests → pressure: "you're at 8
     min, focus on making the existing tests pass; don't refactor"
   - Operator says "I'll continue" or "let me think" without a tool call
     → push: "stop deliberating; run the tests and fix the first failure"
   Keep the message concrete, imperative, and short. Quote real test
   output when you have it. No prose. No "great job".

2. CONTINUE — only when the operator is BOTH executing tool calls each
   iteration AND has visibly advanced toward green tests since the last
   tick (e.g. failure count is dropping, new test files are passing).
   Plain heartbeats without progress = STEER, not CONTINUE.

3. STOP — operator clearly finished (test output 100% green, IDLE posted,
   METRICS reported). Bench will detect this on its own; only emit STOP
   when you're 100% sure to save the bench a poll cycle.

Respond on a single line:
    STEER: <your message here>
    CONTINUE
    STOP

Bias toward STEER. The bench's failure mode is operators drifting alone
for 30 minutes and timing out without solving — your job is to break that
pattern. Anything ambiguous parses as CONTINUE, so be explicit when you
mean to steer."#;

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

        // 3. smooth-* alias stays on OpenAI-compat so LiteLLM can resolve it.
        std::env::set_var(MODEL, "smooth-fast-gemini");
        std::env::set_var(URL, "https://llm.smoo.ai/v1");
        let cfg = SupervisorConfig::from_env().expect("smooth alias config");
        assert_eq!(cfg.api_url, "https://llm.smoo.ai/v1");
        assert_eq!(cfg.api_format, ApiFormat::OpenAiCompat);
        assert_eq!(cfg.interval, Duration::from_secs(DEFAULT_INTERVAL_S));

        // 4. Custom interval picks up.
        std::env::set_var(INTERVAL, "30");
        let cfg = SupervisorConfig::from_env().expect("custom interval");
        assert_eq!(cfg.interval, Duration::from_secs(30));
        std::env::remove_var(INTERVAL);

        // 5. Direct gemini-* model name → native pass-through.
        std::env::set_var(MODEL, "gemini-3.1-flash-lite-preview");
        let cfg = SupervisorConfig::from_env().expect("direct gemini config");
        assert_eq!(cfg.api_url, "https://llm.smoo.ai/gemini/v1beta");
        assert_eq!(cfg.api_format, ApiFormat::Gemini);

        // 6. Direct claude-* model name → Anthropic native shape.
        std::env::set_var(MODEL, "claude-haiku-4-5");
        let cfg = SupervisorConfig::from_env().expect("direct anthropic config");
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
    fn direct_helpers_match_only_upstream_names() {
        // Direct upstream names → match.
        assert!(is_direct_gemini("gemini-3.1-flash-lite-preview"));
        assert!(is_direct_gemini("gemini-2.5-flash"));
        assert!(is_direct_gemini("models/gemini-3-flash-preview"));
        assert!(is_direct_anthropic("claude-haiku-4-5"));
        assert!(is_direct_anthropic("anthropic/claude-haiku-4-5"));

        // smooth-* aliases must NOT match — they have to go through
        // LiteLLM's OpenAI-compat router so the alias resolves.
        assert!(!is_direct_gemini("smooth-fast-gemini"));
        assert!(!is_direct_gemini("smooth-judge-gemini"));
        assert!(!is_direct_anthropic("smooth-judge"));
        assert!(!is_direct_anthropic("smooth-fast-haiku"));

        // Other families never match either helper.
        assert!(!is_direct_gemini("kimi-k2-thinking"));
        assert!(!is_direct_anthropic("kimi-k2-thinking"));
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

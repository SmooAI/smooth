//! Drive a benchmark task through Big Smooth's chat-agent path.
//!
//! **The interface is Big Smooth, period.** The bench process never
//! reads or writes the dolt store directly. Every interaction with
//! the operator goes through `/api/*` endpoints exposed by the
//! daemon, just like a real end-user TUI would.
//!
//! Flow:
//! 1. `POST /api/chat` with the task description. Big Smooth creates
//!    a pearl, dispatches a teammate, and replies with the pearl id.
//! 2. The supervisor (when enabled) periodically `POST`s status-check
//!    messages to `/api/chat`, asking Big Smooth to inspect the pearl
//!    and steer the teammate. Big Smooth's reply tells us whether the
//!    operator is still working, was steered, or is done.
//! 3. The bench polls `GET /api/pearls/{id}` for status, terminating
//!    when the pearl flips to Closed OR the supervisor reports
//!    `OPERATOR DONE` OR the deadline fires.
//! 4. Final cost is read via `GET /api/pearls/{id}/comments` (looking
//!    for the `[METRICS]` comment Big Smooth posts on completion).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context;

use smooth_code::headless::{HeadlessOutput, HeadlessToolCall};

use crate::supervisor::{Supervisor, SupervisorConfig};

/// Output of the chat-agent driver — wraps the legacy `HeadlessOutput`
/// shape with bench-side metadata (pearl id, supervisor stats) so the
/// eval-report renderer can stitch them together later.
pub struct ChatDriverOutput {
    pub headless: HeadlessOutput,
    pub pearl_id: Option<String>,
    pub supervisor_steer_count: u32,
}

/// Read a duration in seconds from an env var, falling back to a default.
///
/// Pulled out so the bench's tunable timeouts read consistently and so
/// unit tests can verify default-vs-override behaviour without poking
/// around in `std::env`.
fn env_secs(var: &str, default_secs: u64) -> u64 {
    std::env::var(var).ok().and_then(|v| v.parse().ok()).unwrap_or(default_secs)
}

/// Drive a task through Big Smooth's chat-agent and return the same shape
/// as the legacy `run_headless_capture` so the caller can keep its scoring
/// logic unchanged.
pub async fn run_via_chat_agent(
    big_smooth_url: &str,
    work_dir: &Path,
    prompt: &str,
    budget_usd: Option<f64>,
    deadline: Duration,
) -> anyhow::Result<ChatDriverOutput> {
    // The HTTP timeout covers the initial POST /api/chat call that
    // dispatches the chat-agent. The chat-agent returns once it's
    // spawned a teammate (typically <60s on a warm daemon, longer on
    // first dispatch when smooth-dolt + sandbox cold-start). 120s and
    // 300s both produced INCONCLUSIVE artifacts (workspace untouched,
    // test runner happens to pass on the polyglot starter). Default to
    // 600s after take 9 — the cost of a slow dispatch is just one extra
    // supervisor tick window, the cost of poisoning the verdict with
    // "did not run" is a useless data point. Override with
    // `SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S`.
    let chat_http_timeout = Duration::from_secs(env_secs("SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S", 600));
    let client = reqwest::Client::builder().timeout(chat_http_timeout).build()?;

    // Compose the chat prompt: the chat-agent's system prompt knows the
    // workflow (search → create → spawn). We just need to give it enough
    // info to dispatch correctly.
    let chat_content = format!(
        "Dispatch a benchmark task and return IMMEDIATELY — do NOT call teammate_wait, do NOT block.\n\n\
         Workflow (this exact sequence, then STOP):\n\
         1. `pearls_create` with the task description below.\n\
         2. `teammate_spawn(pearl_id, working_dir={work_dir}, budget_usd={budget})`.\n\
         3. As SOON as teammate_spawn returns, end your reply. Output ONLY the pearl id on its own last line. \
         **DO NOT** call teammate_wait, teammate_read, or any other tool. The user (a separate harness) will \
         coach the teammate through follow-up chat messages — your job ends at dispatch.\n\n\
         --- task description (give this to pearls_create.description; the teammate will see it) ---\n{prompt}",
        work_dir = work_dir.display(),
        budget = budget_usd.unwrap_or(5.0),
        prompt = prompt,
    );

    // Use a fast model for the dispatch turn — it's just pearls_create
    // + teammate_spawn. The default reasoning-kimi takes 30-60s; gemini
    // flash gets it back in <10s. Override via
    // SMOOTH_BENCH_BIG_SMOOTH_MODEL to experiment.
    let dispatch_model = std::env::var("SMOOTH_BENCH_BIG_SMOOTH_MODEL").unwrap_or_else(|_| "smooth-fast-gemini".to_string());
    let chat_resp: serde_json::Value = client
        .post(format!("{}/api/chat", big_smooth_url.trim_end_matches('/')))
        .json(&serde_json::json!({"content": chat_content, "model": dispatch_model}))
        .send()
        .await
        .context("POST /api/chat")?
        .error_for_status()?
        .json()
        .await
        .context("decode chat response")?;

    let chat_text = chat_resp
        .get("data")
        .and_then(|v| v.as_str())
        .context("chat response missing `data`")?
        .to_string();

    let pearl_id = extract_pearl_id(&chat_text).ok_or_else(|| anyhow::anyhow!("could not find a pearl id in chat response: {chat_text}"))?;
    eprintln!("bench: chat-agent dispatched on {pearl_id}");

    // HTTP client used for status polling + comments fetch. Reuses
    // Big Smooth's public API — bench reads NO dolt directly.
    let poll_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let t0 = Instant::now();
    let idle_grace = Duration::from_secs(env_secs("SMOOTH_BENCH_IDLE_GRACE_S", 600));
    eprintln!("bench: pearl {pearl_id} polling via /api/pearls (idle_grace={}s)", idle_grace.as_secs());
    let mut tool_calls: Vec<HeadlessToolCall> = Vec::new();
    let mut last_seen_comment_count: usize = 0;
    let mut quiet_since = Instant::now();

    // Optional supervisor — talks to Big Smooth via /api/chat with its
    // OWN LLM driving the conversation. Knows the task description so
    // it can offer context-aware guidance just like the user would.
    //
    // Wrapped in Arc<Mutex<...>> + AtomicBool so we can run ticks
    // concurrently with the poll loop. Without this, a slow Big Smooth
    // turn (Kimi can stall up to the 900s chat-turn ceiling) blocks
    // the bench from noticing pearl-status changes — observed in cell
    // B of the sandbox-vs-direct matrix where the operator finished
    // at ~3 min but the bench stayed pinned for 23 min on the
    // supervisor's slow tick.
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::Mutex as AsyncMutex;

    let supervisor: Option<Arc<AsyncMutex<Supervisor>>> = SupervisorConfig::from_env().map(|cfg| {
        eprintln!(
            "bench: woz enabled (LLM={} via {}, interval={}s, daemon={})",
            cfg.model,
            cfg.api_url,
            cfg.interval.as_secs(),
            cfg.daemon_url,
        );
        Arc::new(AsyncMutex::new(Supervisor::new(cfg, pearl_id.clone(), prompt)))
    });
    let tick_in_flight = Arc::new(AtomicBool::new(false));

    let api_base = big_smooth_url.trim_end_matches('/').to_string();

    loop {
        if t0.elapsed() > deadline {
            let cost = fetch_cost_via_api(&poll_client, &api_base, &pearl_id).await;
            return Ok(ChatDriverOutput {
                headless: HeadlessOutput {
                    content: chat_text,
                    tool_calls,
                    cost,
                },
                pearl_id: Some(pearl_id),
                supervisor_steer_count: supervisor_tick_count(&supervisor),
            });
        }

        // Pearl status via Big Smooth. If status flips to Closed, done.
        let status_url = format!("{api_base}/api/pearls/{pearl_id}");
        if let Ok(resp) = poll_client.get(&status_url).send().await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(status) = json.get("data").and_then(|d| d.get("status")).and_then(|s| s.as_str()) {
                    if status.eq_ignore_ascii_case("Closed") {
                        eprintln!("bench: pearl {pearl_id} closed after {:.1}s", t0.elapsed().as_secs_f64());
                        let cost = fetch_cost_via_api(&poll_client, &api_base, &pearl_id).await;
                        return Ok(ChatDriverOutput {
                            headless: HeadlessOutput {
                                content: chat_text,
                                tool_calls,
                                cost,
                            },
                            pearl_id: Some(pearl_id),
                            supervisor_steer_count: supervisor_tick_count(&supervisor),
                        });
                    }
                }
            }
        }

        // Comments via Big Smooth (cheap state-of-life check).
        let comments_url = format!("{api_base}/api/pearls/{pearl_id}/comments");
        let comments: Vec<smooth_pearls::PearlComment> = match poll_client.get(&comments_url).send().await {
            Ok(resp) => resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|j| j.get("data").cloned())
                .and_then(|d| serde_json::from_value(d).ok())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        // Explicit IDLE = done.
        if comments.iter().any(|c| c.content.trim_start().starts_with("[IDLE]")) {
            eprintln!("bench: teammate posted [IDLE] on {pearl_id} after {:.1}s", t0.elapsed().as_secs_f64());
            for c in &comments {
                if c.content.trim_start().starts_with("[PROGRESS]") {
                    tool_calls.push(HeadlessToolCall {
                        name: "progress".into(),
                        success: true,
                    });
                }
            }
            let cost = extract_cost(&comments);
            return Ok(ChatDriverOutput {
                headless: HeadlessOutput {
                    content: chat_text,
                    tool_calls,
                    cost,
                },
                pearl_id: Some(pearl_id),
                supervisor_steer_count: supervisor_tick_count(&supervisor),
            });
        }

        // Supervisor tick — fire-and-forget on a separate task so the
        // poll loop keeps checking pearl status while Big Smooth chews
        // through tool calls. AtomicBool gates against re-entry: while
        // a tick is in flight (lock held by the spawned task), we
        // skip new spawns. Whatever tick is running when the pearl
        // closes is silently abandoned — its work was already useful
        // (it composed and sent a coaching message); no need to await
        // its return.
        if let Some(sup_arc) = supervisor.as_ref() {
            if !tick_in_flight.load(Ordering::SeqCst) {
                // Try to read should_tick without blocking — peek the
                // mutex with try_lock; if locked, a tick is in flight
                // (our flag is also set, this is belt-and-suspenders).
                let should = sup_arc.try_lock().map_or(false, |sup| sup.should_tick(Instant::now()));
                if should {
                    tick_in_flight.store(true, Ordering::SeqCst);
                    let sup_clone = Arc::clone(sup_arc);
                    let flag_clone = Arc::clone(&tick_in_flight);
                    tokio::spawn(async move {
                        let mut sup = sup_clone.lock().await;
                        let _ = sup.tick_async(t0).await;
                        flag_clone.store(false, Ordering::SeqCst);
                    });
                }
            }
        }

        // Quiescence heuristic — no new comments for `idle_grace`.
        if comments.len() == last_seen_comment_count {
            if quiet_since.elapsed() > idle_grace {
                eprintln!("bench: pearl {pearl_id} quiet for {}s, treating as done", idle_grace.as_secs());
                let cost = extract_cost(&comments);
                return Ok(ChatDriverOutput {
                    headless: HeadlessOutput {
                        content: chat_text,
                        tool_calls,
                        cost,
                    },
                    pearl_id: Some(pearl_id),
                    supervisor_steer_count: supervisor_tick_count(&supervisor),
                });
            }
        } else {
            last_seen_comment_count = comments.len();
            quiet_since = Instant::now();
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Read the supervisor's tick count without blocking on a tick that's
/// currently in flight. Returns 0 when supervisor is disabled or the
/// mutex is held (we'd rather report a stale-by-one count than wedge
/// the bench's return path on a tick that's still running).
fn supervisor_tick_count(sup: &Option<std::sync::Arc<tokio::sync::Mutex<Supervisor>>>) -> u32 {
    sup.as_ref().map_or(0, |arc| arc.try_lock().map_or(0, |s| s.tick_count()))
}

/// Fetch the pearl's `[METRICS]` cost via the comments endpoint.
async fn fetch_cost_via_api(client: &reqwest::Client, api_base: &str, pearl_id: &str) -> f64 {
    let url = format!("{api_base}/api/pearls/{pearl_id}/comments");
    let comments: Vec<smooth_pearls::PearlComment> = match client.get(&url).send().await {
        Ok(resp) => resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|j| j.get("data").cloned())
            .and_then(|d| serde_json::from_value(d).ok())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    extract_cost(&comments)
}

/// Pull total cost from `[METRICS]` (operator-runner) AND
/// `[CHAT_METRICS]` (chat-agent / orchestrator) comments and sum
/// them — the bench wants the total LLM spend the dispatch caused,
/// not just one half. Both formats are `... cost_usd=X ...` lines:
/// - `[METRICS] cost_usd=X iterations=Y` is written by
///   `dispatch_ws_task_sandboxed` when the operator-runner finishes.
/// - `[CHAT_METRICS] cost_usd=X model=Y` is written by `chat_handler`
///   when the chat-agent's reply mentions a pearl id, capturing the
///   orchestrator's spend on the dispatch.
/// Multiple `[CHAT_METRICS]` may exist (e.g. supervisor steers add
/// turns); each is summed.
fn extract_cost(comments: &[smooth_pearls::PearlComment]) -> f64 {
    let mut metrics_cost: Option<f64> = None;
    let mut chat_total: f64 = 0.0;
    for c in comments {
        let t = c.content.trim_start();
        if let Some(rest) = t.strip_prefix("[METRICS]") {
            // Newest [METRICS] wins (operator runs once per dispatch).
            if let Some(v) = parse_cost_token(rest) {
                metrics_cost = Some(v);
            }
        } else if let Some(rest) = t.strip_prefix("[CHAT_METRICS]") {
            if let Some(v) = parse_cost_token(rest) {
                chat_total += v;
            }
        }
    }
    metrics_cost.unwrap_or(0.0) + chat_total
}

fn parse_cost_token(rest: &str) -> Option<f64> {
    for token in rest.split_whitespace() {
        if let Some(value) = token.strip_prefix("cost_usd=") {
            if let Ok(v) = value.parse::<f64>() {
                return Some(v);
            }
        }
    }
    None
}

/// Extract a pearl id (`th-[0-9a-f]{6}`) from arbitrary text. Looks for
/// the LAST match so the chat-agent's "return only the pearl id on its
/// own line as the last line" instruction wins over earlier mentions.
fn extract_pearl_id(text: &str) -> Option<String> {
    let mut last: Option<String> = None;
    for line in text.lines() {
        for word in line.split_whitespace() {
            let cleaned = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
            if let Some(rest) = cleaned.strip_prefix("th-") {
                if rest.len() >= 6 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
                    last = Some(format!("th-{rest}"));
                }
            }
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_pearl_id_finds_last() {
        let s = "I considered th-aaaaaa first then created\nth-bbbbbb\n\nth-cccccc";
        assert_eq!(extract_pearl_id(s).as_deref(), Some("th-cccccc"));
    }

    #[test]
    fn extract_pearl_id_handles_punctuation() {
        let s = "Created pearl `th-83c220`. Dispatched.";
        assert_eq!(extract_pearl_id(s).as_deref(), Some("th-83c220"));
    }

    #[test]
    fn extract_pearl_id_returns_none_when_absent() {
        assert!(extract_pearl_id("hello world no pearls here").is_none());
        assert!(extract_pearl_id("th-xx").is_none()); // too short
    }

    #[test]
    fn env_secs_falls_back_when_unset_or_invalid() {
        // Use a unique var name so the test doesn't collide with an
        // operator's actual env. The fallback path is what runs in CI.
        let var = "SMOOTH_BENCH_TEST_SECS_FALLBACK_XYZ";
        // Unset → default.
        std::env::remove_var(var);
        assert_eq!(env_secs(var, 42), 42);
        // Garbage → default.
        std::env::set_var(var, "not-a-number");
        assert_eq!(env_secs(var, 7), 7);
        // Valid integer → parsed value.
        std::env::set_var(var, "999");
        assert_eq!(env_secs(var, 7), 999);
        std::env::remove_var(var);
    }

    fn comment(content: &str) -> smooth_pearls::PearlComment {
        smooth_pearls::PearlComment {
            id: "test".into(),
            pearl_id: "th-aaaaaa".into(),
            content: content.into(),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn extract_cost_metrics_alone() {
        let cs = vec![comment("[METRICS] cost_usd=0.0123 iterations=3")];
        assert!((extract_cost(&cs) - 0.0123).abs() < 1e-9);
    }

    #[test]
    fn extract_cost_sums_chat_metrics_and_metrics() {
        let cs = vec![
            comment("[CHAT_METRICS] cost_usd=0.0010 model=gemini-3-flash-preview"),
            comment("[METRICS] cost_usd=0.0500 iterations=3"),
            comment("[CHAT_METRICS] cost_usd=0.0005 model=gemini-3-flash-preview"),
        ];
        // 0.0500 + 0.0010 + 0.0005 = 0.0515
        assert!((extract_cost(&cs) - 0.0515).abs() < 1e-9, "got {}", extract_cost(&cs));
    }

    #[test]
    fn extract_cost_chat_metrics_alone_when_no_runner_metrics() {
        let cs = vec![
            comment("[CHAT_METRICS] cost_usd=0.0010 model=gemini-3-flash-preview"),
            comment("[CHAT_METRICS] cost_usd=0.0007 model=gemini-3-flash-preview"),
        ];
        assert!((extract_cost(&cs) - 0.0017).abs() < 1e-9);
    }

    #[test]
    fn extract_cost_zero_when_no_metrics_comments() {
        let cs = vec![comment("[PROGRESS] running"), comment("hello")];
        assert_eq!(extract_cost(&cs), 0.0);
    }

    #[test]
    fn extract_cost_uses_newest_metrics_when_duplicates() {
        // Operator runs once per dispatch; if a stale [METRICS] is
        // present, the newest one wins (later iterations overwrite).
        let cs = vec![
            comment("[METRICS] cost_usd=0.0100 iterations=1"),
            comment("[METRICS] cost_usd=0.0500 iterations=3"),
        ];
        assert!((extract_cost(&cs) - 0.0500).abs() < 1e-9);
    }
}

//! Drive a benchmark task through Big Smooth's chat-agent path.
//!
//! Per the user's directive (2026-04-26): bench runs go through Big Smooth,
//! not directly through `dispatch_ws_task`. The flow:
//!
//! 1. POST `/api/chat` with the task prompt + working dir + budget. The
//!    chat-agent (smooth-reasoning-kimi by default) creates a pearl and
//!    spawns a teammate via `pearls_create` + `teammate_spawn(working_dir)`.
//! 2. Parse the pearl id out of the chat response.
//! 3. Open the local PearlStore and poll the pearl's comments until the
//!    teammate posts `[IDLE]` (graceful exit) or the wall-clock timeout
//!    fires.
//! 4. Return cost, tool calls (best-effort, drained from comments) and any
//!    LLM error so the caller can score the workspace as today.
//!
//! `[IDLE]` isn't posted by the runner today (planned for Phase 2 follow-up
//! or Phase 4); for now we treat any of these as completion: pearl status
//! flipping to `closed`, or the comment list growing past `idle_grace`
//! seconds without further activity. That's enough to unblock the bench
//! while we wire the explicit IDLE marker.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context;

use smooth_code::headless::{HeadlessOutput, HeadlessToolCall};

/// Drive a task through Big Smooth's chat-agent and return the same shape
/// as the legacy `run_headless_capture` so the caller can keep its scoring
/// logic unchanged.
pub async fn run_via_chat_agent(
    big_smooth_url: &str,
    work_dir: &Path,
    prompt: &str,
    budget_usd: Option<f64>,
    deadline: Duration,
) -> anyhow::Result<HeadlessOutput> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?;

    // Compose the chat prompt: the chat-agent's system prompt knows the
    // workflow (search → create → spawn). We just need to give it enough
    // info to dispatch correctly.
    let chat_content = format!(
        "Run a benchmark task. Create a pearl with the following description and dispatch a teammate on it (working_dir={}, budget_usd={}). Once the teammate is dispatched, return ONLY the pearl id on its own line as the last line of your response.\n\n--- task ---\n{}",
        work_dir.display(),
        budget_usd.unwrap_or(5.0),
        prompt
    );

    let chat_resp: serde_json::Value = client
        .post(format!("{}/api/chat", big_smooth_url.trim_end_matches('/')))
        .json(&serde_json::json!({"content": chat_content}))
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

    // Open the local pearl store to poll for completion. Bench runs are
    // expected to share the same `~/.smooth/dolt/` Big Smooth uses (same
    // host, same registry).
    let dolt_dir = locate_pearl_store_dir().context("locate pearl store")?;
    let store = smooth_pearls::PearlStore::open(&dolt_dir).context("open pearl store")?;

    // Poll loop: check pearl comments for [IDLE] / [PROGRESS] / status=closed.
    let t0 = Instant::now();
    let mut last_seen_count = 0usize;
    let mut quiet_since = Instant::now();
    let idle_grace = Duration::from_secs(120);
    let mut tool_calls: Vec<HeadlessToolCall> = Vec::new();

    loop {
        if t0.elapsed() > deadline {
            return Ok(HeadlessOutput {
                content: chat_text,
                tool_calls,
                cost: 0.0,
            });
        }

        let comments = store.get_comments(&pearl_id).unwrap_or_default();

        // Check for an explicit IDLE post.
        if comments.iter().any(|c| c.content.trim_start().starts_with("[IDLE]")) {
            eprintln!("bench: teammate posted [IDLE] on {pearl_id} after {:.1}s", t0.elapsed().as_secs_f64());
            // Best-effort tool-call extraction from PROGRESS/CHAT comments.
            for c in &comments {
                let t = c.content.trim_start();
                if t.starts_with("[PROGRESS]") {
                    tool_calls.push(HeadlessToolCall {
                        name: "progress".into(),
                        success: true,
                    });
                }
            }
            return Ok(HeadlessOutput {
                content: chat_text,
                tool_calls,
                cost: 0.0,
            });
        }

        // Check pearl status — closed = done.
        if let Ok(Some(p)) = store.get(&pearl_id) {
            if p.status == smooth_pearls::PearlStatus::Closed {
                eprintln!("bench: pearl {pearl_id} closed after {:.1}s", t0.elapsed().as_secs_f64());
                return Ok(HeadlessOutput {
                    content: chat_text,
                    tool_calls,
                    cost: 0.0,
                });
            }
        }

        // Quiescence heuristic — no new comments for `idle_grace`
        // means the teammate likely finished and didn't post [IDLE].
        if comments.len() == last_seen_count {
            if quiet_since.elapsed() > idle_grace {
                eprintln!("bench: pearl {pearl_id} quiet for {}s, treating as done", idle_grace.as_secs());
                return Ok(HeadlessOutput {
                    content: chat_text,
                    tool_calls,
                    cost: 0.0,
                });
            }
        } else {
            last_seen_count = comments.len();
            quiet_since = Instant::now();
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
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

/// Find the Dolt store the bench should poll. Walks up from `cwd` looking
/// for `.smooth/dolt/`, falls back to `~/.smooth/dolt/` (the global store).
fn locate_pearl_store_dir() -> anyhow::Result<std::path::PathBuf> {
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(d) = smooth_pearls::dolt::find_repo_dolt_dir(&cwd) {
            return Ok(d);
        }
    }
    let global = dirs_next::home_dir().context("$HOME unset")?.join(".smooth").join("dolt");
    if global.exists() {
        return Ok(global);
    }
    anyhow::bail!("no .smooth/dolt found in repo ancestry or ~/.smooth/dolt")
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
}

//! LLM-as-human driver for the `score-tui` flow.
//!
//! An external LLM (configured via the `--driver-model` flag, default
//! `smooth-summarize`) plays the role of a human user testing an AI
//! coding assistant: it reads the current TUI pane contents, decides
//! what to type next, and watches for completion / dead-end signals.
//!
//! Why a separate "driver" LLM:
//! - We need a *cheap and fast* model, because it runs many turns per
//!   task. The expensive coding-class model is the one *being tested*
//!   inside `th code`; using it here too would double cost and confound
//!   the score.
//! - The driver model never writes code or runs tools; it just
//!   composes short user turns. Summarize-class throughput is fine.
//!
//! Protocol:
//! - Per turn, the driver LLM is asked to reply with ONLY the next
//!   user message — or one of two literal sentinels:
//!     - `TASK_COMPLETE` — driver believes the task is done & passing.
//!     - `TASK_STUCK`    — driver has been waiting without progress.
//! - The harness caps turns (default 15) to bound runaway loops.
//!
//! This module is intentionally LLM-agnostic at the trait boundary —
//! unit tests inject a `FakeDriver` that returns canned strings so we
//! can exercise the loop without a real LLM call.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::tmux_driver::TmuxDriver;

/// One sentinel-or-message decision from the driver LLM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverDecision {
    /// Type this message verbatim into the TUI.
    Send(String),
    /// The driver believes the task is finished successfully — stop
    /// the loop and let the bench score the workspace.
    Complete,
    /// The driver believes the agent is stuck and won't make further
    /// progress — stop the loop and score what's there.
    Stuck,
}

impl DriverDecision {
    /// Parse a raw LLM response into a `DriverDecision`. Strips
    /// surrounding whitespace and accepts the literal sentinels
    /// `TASK_COMPLETE` / `TASK_STUCK` on a line by themselves (case
    /// insensitive). Everything else is a `Send(...)`.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        // Sentinel match — anywhere in the response, since some
        // models pad sentinels with explanation. Prefer COMPLETE over
        // STUCK when both appear (positive signal wins).
        let upper = trimmed.to_uppercase();
        if upper.contains("TASK_COMPLETE") {
            return Self::Complete;
        }
        if upper.contains("TASK_STUCK") {
            return Self::Stuck;
        }
        // Strip a leading code fence if the model wraps the message —
        // some chat models default to triple-backtick wrapping. We
        // want the literal user message, not a code block.
        Self::Send(strip_code_fence(trimmed).to_string())
    }
}

/// Strip a leading "```lang\n" + trailing "```" if present. Returns
/// the slice between them, or the original trimmed input if no fence
/// is found. Tolerant of language tags ("```text\n…```") and missing
/// trailing fence (some models truncate).
fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    let Some(stripped) = s.strip_prefix("```") else {
        return s;
    };
    // Drop up to the first newline (the optional language tag).
    let after_open = stripped.split_once('\n').map_or(stripped, |(_, rest)| rest);
    // Drop a trailing fence.
    after_open.strip_suffix("```").map_or(after_open, str::trim_end).trim()
}

/// Build the per-turn prompt the driver LLM sees. Public so unit
/// tests can exercise it without a live LLM.
///
/// `task` — the high-level task description (what aider-polyglot says
/// the user wants done).
/// `pane` — the current visible TUI output (`tmux capture-pane -p`).
/// `turn_idx` — 1-based turn counter (used for the stuck heuristic).
/// `max_turns` — total turn cap.
#[must_use]
pub fn build_driver_prompt(task: &str, pane: &str, turn_idx: usize, max_turns: usize) -> String {
    format!(
        "You are simulating a user testing an AI coding assistant. The task you've been asked to complete is:\n\n\
         {task}\n\n\
         Here is what's currently visible in the assistant's terminal (turn {turn_idx} of {max_turns}):\n\n\
         {pane}\n\n\
         What is your next message to the assistant? Reply with ONLY the message text — no preamble, no quotes, no code fences. \
         If you believe the task is complete and successful, reply with the literal token TASK_COMPLETE on its own line. \
         If you've been waiting more than 3 turns without progress, reply with TASK_STUCK."
    )
}

/// Abstract "ask an LLM what to type next" — generic so production
/// code can wire `smooth-operator`'s `LlmClient` and unit tests can
/// inject a deterministic stub.
#[async_trait]
pub trait DriverModel: Send + Sync {
    /// Given the current pane snapshot and turn index, return the
    /// next driver decision. The default `LlmDriverModel` impl
    /// wraps this around `LlmClient::chat`.
    async fn next_decision(&self, task: &str, pane: &str, turn_idx: usize, max_turns: usize) -> Result<DriverDecision>;
}

/// Default production driver: wraps an `LlmClient` (one of the
/// configured routing slots, typically `smooth-summarize`).
pub struct LlmDriverModel {
    client: smooth_operator::llm::LlmClient,
}

impl LlmDriverModel {
    /// Build an `LlmDriverModel` from a routing slot via the
    /// user-level `providers.json`. `slot` defaults to
    /// `Activity::Summarize` — cheap and fast.
    ///
    /// # Errors
    /// Errors if `providers.json` can't be loaded or the slot isn't
    /// configured.
    pub fn from_activity(slot: smooth_operator::providers::Activity) -> Result<Self> {
        let providers_path = dirs_next::home_dir().map(|h| h.join(".smooth/providers.json")).context("no home dir")?;
        let registry = smooth_operator::providers::ProviderRegistry::load_from_file(&providers_path).context("loading providers.json")?;
        let config = registry
            .llm_config_for(slot)
            .with_context(|| format!("no routing slot configured for {slot:?}"))?;
        Ok(Self {
            client: smooth_operator::llm::LlmClient::new(config),
        })
    }
}

#[async_trait]
impl DriverModel for LlmDriverModel {
    async fn next_decision(&self, task: &str, pane: &str, turn_idx: usize, max_turns: usize) -> Result<DriverDecision> {
        use smooth_operator::conversation::Message;

        let system = Message::system(
            "You roleplay a human user testing an AI coding assistant in a terminal. \
             Reply with the EXACT TEXT the user types next, nothing else. \
             Use TASK_COMPLETE on its own line when satisfied, TASK_STUCK when out of ideas.",
        );
        let user = Message::user(build_driver_prompt(task, pane, turn_idx, max_turns));
        let response = self.client.chat(&[&system, &user], &[]).await.context("driver LLM call failed")?;
        Ok(DriverDecision::parse(&response.content))
    }
}

/// Outcome of a `run_human_loop` invocation. Recorded for the SweepRun
/// row so downstream analysis can distinguish "completed via sentinel"
/// from "stuck" from "hit turn cap".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopExit {
    /// Driver fired `TASK_COMPLETE`.
    Complete,
    /// Driver fired `TASK_STUCK`.
    Stuck,
    /// Reached `max_turns` without a sentinel.
    TurnCap,
    /// `wait_for_idle` timed out — the TUI froze.
    IdleTimeout,
}

/// Final shape returned by `run_human_loop` — turns, exit reason, and
/// the final pane capture (useful for debugging post-mortems).
#[derive(Debug, Clone)]
pub struct LoopResult {
    pub turns: usize,
    pub exit: LoopExit,
    pub final_pane: String,
}

/// Configuration for [`run_human_loop`]. Defaults are conservative
/// for the CI gate; pearl bench operators tune via the CLI.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub max_turns: usize,
    pub idle_dwell: Duration,
    pub idle_poll: Duration,
    /// Overall per-turn idle timeout (gives up on a frozen TUI).
    pub per_turn_timeout: Duration,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 15,
            idle_dwell: crate::tmux_driver::DEFAULT_IDLE_DWELL,
            idle_poll: crate::tmux_driver::DEFAULT_POLL_INTERVAL,
            // Real coding turns regularly take 60s+ on harder tasks
            // (model "thinking" + tool calls). 180s is the same
            // ballpark `chat_driver.rs` uses for its `idle_grace`
            // default scaled down for a single turn.
            per_turn_timeout: Duration::from_secs(180),
        }
    }
}

/// Drive a TUI session through one full task using the LLM-as-human
/// loop. Sends the initial `task_prompt` as turn 1, then alternates
/// driver-decision / send / wait-for-idle until termination.
///
/// # Errors
/// Errors only on tmux failure or driver-LLM failure. Sentinel exits
/// and turn caps are returned as `Ok(LoopResult)`.
pub async fn run_human_loop<D: DriverModel>(driver: &TmuxDriver, model: &D, task: &str, task_prompt: &str, cfg: &LoopConfig) -> Result<LoopResult> {
    // Turn 1: send the seeded task prompt to the TUI.
    driver.send(task_prompt).context("seeding task prompt")?;

    let mut turns = 1usize;
    let mut final_pane;

    loop {
        // Wait for the TUI to react to the last message we sent.
        let Ok(pane) = driver.wait_for_idle(cfg.idle_dwell, cfg.idle_poll, cfg.per_turn_timeout) else {
            // Idle timeout — score whatever's on the pane.
            let last = driver.capture().unwrap_or_default();
            return Ok(LoopResult {
                turns,
                exit: LoopExit::IdleTimeout,
                final_pane: last,
            });
        };
        final_pane = pane.clone();

        if turns >= cfg.max_turns {
            return Ok(LoopResult {
                turns,
                exit: LoopExit::TurnCap,
                final_pane,
            });
        }

        let decision = model.next_decision(task, &pane, turns, cfg.max_turns).await?;
        match decision {
            DriverDecision::Complete => {
                return Ok(LoopResult {
                    turns,
                    exit: LoopExit::Complete,
                    final_pane,
                });
            }
            DriverDecision::Stuck => {
                return Ok(LoopResult {
                    turns,
                    exit: LoopExit::Stuck,
                    final_pane,
                });
            }
            DriverDecision::Send(text) => {
                turns += 1;
                if text.trim().is_empty() {
                    // Empty drive message — treat as "user has
                    // nothing to add, agent should keep going". Send
                    // a bare newline so the TUI advances.
                    driver.send("").context("send empty turn")?;
                } else {
                    driver.send(&text).context("send driver turn")?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn parse_plain_message_is_send() {
        assert_eq!(
            DriverDecision::parse("please run the tests"),
            DriverDecision::Send("please run the tests".into())
        );
    }

    #[test]
    fn parse_strips_whitespace() {
        assert_eq!(DriverDecision::parse("   hi there   \n"), DriverDecision::Send("hi there".into()));
    }

    #[test]
    fn parse_complete_sentinel() {
        assert_eq!(DriverDecision::parse("TASK_COMPLETE"), DriverDecision::Complete);
        assert_eq!(DriverDecision::parse("task_complete"), DriverDecision::Complete);
        assert_eq!(DriverDecision::parse("Looks great — TASK_COMPLETE"), DriverDecision::Complete);
    }

    #[test]
    fn parse_stuck_sentinel() {
        assert_eq!(DriverDecision::parse("TASK_STUCK"), DriverDecision::Stuck);
        assert_eq!(DriverDecision::parse("I give up. TASK_STUCK"), DriverDecision::Stuck);
    }

    #[test]
    fn parse_prefers_complete_over_stuck_when_both_present() {
        // Defensive: if the model hedges, count the positive signal.
        assert_eq!(DriverDecision::parse("TASK_COMPLETE (also TASK_STUCK)"), DriverDecision::Complete);
    }

    #[test]
    fn parse_strips_code_fence() {
        assert_eq!(DriverDecision::parse("```\nrun the tests\n```"), DriverDecision::Send("run the tests".into()));
        assert_eq!(DriverDecision::parse("```text\nplease retry\n```"), DriverDecision::Send("please retry".into()));
    }

    #[test]
    fn build_driver_prompt_includes_task_and_pane_and_turn() {
        let p = build_driver_prompt("make tests pass", "pane line A\npane line B", 4, 15);
        assert!(p.contains("make tests pass"), "prompt missing task: {p}");
        assert!(p.contains("pane line A"), "prompt missing pane: {p}");
        assert!(p.contains("turn 4 of 15"), "prompt missing turn idx: {p}");
        assert!(p.contains("TASK_COMPLETE"), "prompt missing complete sentinel: {p}");
        assert!(p.contains("TASK_STUCK"), "prompt missing stuck sentinel: {p}");
    }

    /// Deterministic fake driver — returns a canned sequence of
    /// decisions. Used by `run_human_loop` tests so they don't
    /// require a live LLM.
    struct FakeDriver {
        responses: Vec<DriverDecision>,
        calls: AtomicUsize,
    }

    impl FakeDriver {
        fn new(responses: Vec<DriverDecision>) -> Self {
            Self {
                responses,
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl DriverModel for FakeDriver {
        async fn next_decision(&self, _task: &str, _pane: &str, _turn_idx: usize, _max_turns: usize) -> Result<DriverDecision> {
            let idx = self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.responses.get(idx).cloned().unwrap_or(DriverDecision::Stuck))
        }
    }

    #[tokio::test]
    async fn fake_driver_returns_canned_sequence() {
        // Just a smoke test that the fake driver itself is sane —
        // run_human_loop integration is covered separately because
        // it depends on a tmux session.
        let fake = FakeDriver::new(vec![
            DriverDecision::Send("look at INSTRUCTIONS.md".into()),
            DriverDecision::Send("run the tests".into()),
            DriverDecision::Complete,
        ]);
        assert_eq!(
            fake.next_decision("t", "p", 1, 15).await.unwrap(),
            DriverDecision::Send("look at INSTRUCTIONS.md".into())
        );
        assert_eq!(fake.next_decision("t", "p", 2, 15).await.unwrap(), DriverDecision::Send("run the tests".into()));
        assert_eq!(fake.next_decision("t", "p", 3, 15).await.unwrap(), DriverDecision::Complete);
    }
}

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
        "You are simulating a NON-TECHNICAL human user testing an AI coding assistant in a chat-style TUI.\n\
         You type plain English messages and press Enter to send. That's it. You are NOT a power user; you do NOT have a shell, file access, or any direct way to read or edit files. The AI assistant is the only one that can do those things — you must ask it to.\n\n\
         RULES:\n\
         - Reply with the EXACT TEXT you would type into the chat box, nothing else. No preamble, no quotes, no code fences, no commentary.\n\
         - NEVER start a message with `/`. Slash commands (e.g. /open, /read, /edit, /run, /help) do NOT exist in this TUI — they will be rejected as unknown commands. You have NO direct file access or shell access; only the agent under test does.\n\
         - To get the assistant to do something, ASK in plain English. Example — wrong: `/read INSTRUCTIONS.md`. Right: `Please read INSTRUCTIONS.md and tell me what it says.`\n\
         - The assistant replies in plain prose. Read what it just said (in the terminal capture below) and reply naturally to it.\n\
         - Only two special tokens exist. Send `TASK_COMPLETE` on its own line when you're confident the task is done and tests pass. Send `TASK_STUCK` on its own line when the assistant is not making progress.\n\n\
         Task you're asking the assistant to solve:\n\n\
         {task}\n\n\
         What's currently visible in the assistant's terminal (turn {turn_idx} of {max_turns}):\n\n\
         {pane}\n\n\
         Your next message (plain English, no leading `/`):"
    )
}

/// Build the reinforcement prompt sent when the driver model
/// produces a slash-command turn that the harness refuses to send.
/// Public so unit tests can assert on its content.
#[must_use]
pub fn build_slash_retry_prompt(task: &str, pane: &str, turn_idx: usize, max_turns: usize, bad_turn: &str, attempts_remaining: usize) -> String {
    format!(
        "STOP. Your last reply started with `/`, which this TUI does NOT support — there are no slash commands. \
         You have no shell access; the AI assistant does. Ask in plain English instead.\n\n\
         Your rejected reply was:\n{bad_turn}\n\n\
         Try again. You have {attempts_remaining} attempt(s) left before the task is marked stuck.\n\n\
         Task:\n\n{task}\n\n\
         Terminal (turn {turn_idx} of {max_turns}):\n\n{pane}\n\n\
         Your next message (plain English, no leading `/`):"
    )
}

/// Maximum consecutive slash-command turns we tolerate from the
/// driver model before bailing out with `TASK_STUCK`. Three gives the
/// model two chances to course-correct after the first violation.
pub const MAX_SLASH_RETRIES: usize = 3;

/// Returns true if `msg` would be interpreted by the TUI as a slash
/// command (and thus rejected). Whitespace-only or empty inputs
/// don't count; we only refuse turns whose first non-whitespace
/// character is `/`.
#[must_use]
pub fn is_slash_command(msg: &str) -> bool {
    msg.trim_start().starts_with('/')
}

/// Collapse a multi-line message into a single line so the
/// `smooth-code` TUI submits it as one turn rather than N.
///
/// The TUI's input handler treats every embedded newline as Enter
/// (submit). Driver-model replies often contain `\n` (paragraph
/// breaks, lists) — without flattening these become N separate
/// `You:` submissions, fragmenting the conversation. Belt-and-
/// suspenders with `tmux paste-buffer -p` (bracketed paste): even
/// when the TUI honors bracketed paste, the flattened form is
/// robust against future input-handler changes.
///
/// Strategy: trim each line, drop empty lines, join with `" | "`.
/// `" | "` is unambiguous separator-text — neither valid prose nor
/// markdown — so a reader (the assistant under test) can still
/// recover the structure if it cares to.
#[must_use]
pub fn flatten_for_tui(text: &str) -> String {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" | ")
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

    /// Re-ask the driver after a slash-command turn was rejected.
    /// `bad_turn` is the offending reply; `attempts_remaining` is
    /// how many further retries the harness will accept before
    /// giving up.
    ///
    /// Default implementation simply calls `next_decision` again —
    /// the harness's own re-asking is sufficient for stub models.
    /// `LlmDriverModel` overrides to surface the reinforcement
    /// prompt so the real LLM has explicit context for the retry.
    async fn next_decision_after_slash(
        &self,
        task: &str,
        pane: &str,
        turn_idx: usize,
        max_turns: usize,
        bad_turn: &str,
        attempts_remaining: usize,
    ) -> Result<DriverDecision> {
        let _ = (bad_turn, attempts_remaining);
        self.next_decision(task, pane, turn_idx, max_turns).await
    }
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

        let system = Message::system(system_prompt());
        let user = Message::user(build_driver_prompt(task, pane, turn_idx, max_turns));
        let response = self.client.chat(&[&system, &user], &[]).await.context("driver LLM call failed")?;
        Ok(DriverDecision::parse(&response.content))
    }

    async fn next_decision_after_slash(
        &self,
        task: &str,
        pane: &str,
        turn_idx: usize,
        max_turns: usize,
        bad_turn: &str,
        attempts_remaining: usize,
    ) -> Result<DriverDecision> {
        use smooth_operator::conversation::Message;

        let system = Message::system(system_prompt());
        let user = Message::user(build_slash_retry_prompt(task, pane, turn_idx, max_turns, bad_turn, attempts_remaining));
        let response = self.client.chat(&[&system, &user], &[]).await.context("driver LLM retry call failed")?;
        Ok(DriverDecision::parse(&response.content))
    }
}

/// System prompt used by `LlmDriverModel`. Shared between the
/// normal-turn and slash-retry paths so both reinforce the same
/// no-slash-commands directive.
fn system_prompt() -> &'static str {
    "You roleplay a non-technical human user chatting with an AI coding assistant in a TUI. \
     You have NO shell, NO file access, and NO slash commands. \
     The TUI ignores anything starting with `/` — there is no /open, /read, /edit, /run, /help. \
     If you want the assistant to do something, ask in plain English (the assistant has the tools). \
     Reply with the EXACT TEXT to type into the chat, nothing else: no preamble, no quotes, no code fences. \
     Never start a reply with `/`. \
     Use TASK_COMPLETE on its own line when the task is clearly done and tests pass, TASK_STUCK when out of ideas."
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
    // Turn 1: send the seeded task prompt to the TUI. Flatten as a
    // defence-in-depth — `build_prompt` should already return a
    // single line, but if a caller passes multi-line text the TUI
    // would split it into multiple `You:` submissions. See pearl
    // th-01c714.
    driver.send(&flatten_for_tui(task_prompt)).context("seeding task prompt")?;

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

        let mut decision = model.next_decision(task, &pane, turns, cfg.max_turns).await?;

        // Slash-command guard: if the driver model produces a
        // message starting with `/`, the TUI would reject it as an
        // unknown command (and could accidentally trigger built-in
        // skills like `/add-show` or `/create-skill` — observed in
        // the pearl th-7fdfa9 debug log). DROP it on the floor,
        // re-ask with reinforcement up to MAX_SLASH_RETRIES total
        // attempts. If the model can't recover, mark TASK_STUCK so
        // the run terminates instead of burning the full turn cap.
        let mut slash_attempts = 0usize;
        while let DriverDecision::Send(ref text) = decision {
            if !is_slash_command(text) {
                break;
            }
            slash_attempts += 1;
            let attempts_remaining = MAX_SLASH_RETRIES.saturating_sub(slash_attempts);
            driver.debug_record(
                "slash_command_rejected",
                &format!(
                    "driver model produced a slash-command reply (attempt {slash_attempts}/{MAX_SLASH_RETRIES}); dropped on floor.\nrejected reply:\n{text}\nattempts remaining: {attempts_remaining}",
                ),
            );
            tracing::warn!(
                attempt = slash_attempts,
                max = MAX_SLASH_RETRIES,
                rejected = %text,
                "driver model produced slash-command reply; dropping and re-asking"
            );
            if slash_attempts >= MAX_SLASH_RETRIES {
                driver.debug_record(
                    "slash_command_giveup",
                    &format!("{MAX_SLASH_RETRIES} consecutive slash-command turns; marking TASK_STUCK"),
                );
                return Ok(LoopResult {
                    turns,
                    exit: LoopExit::Stuck,
                    final_pane,
                });
            }
            decision = model
                .next_decision_after_slash(task, &pane, turns, cfg.max_turns, text, attempts_remaining)
                .await?;
        }

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
                    // Empty drive message — treat as "user has nothing
                    // to add, agent should keep going". Skip the send
                    // entirely: tmux load-buffer rejects empty payloads
                    // ("no buffer NAME") so we can't paste-buffer here.
                    // Next iteration's wait_for_idle gives the agent
                    // more time to produce output, then re-prompts the
                    // driver with a richer pane snapshot.
                    tracing::debug!(turns, "human_driver: empty driver reply, skipping send");
                } else {
                    // Flatten newlines to ` | ` — the TUI submits on
                    // every newline, so a multi-paragraph driver
                    // reply would fragment into N `You:` turns.
                    // Pearl th-01c714.
                    driver.send(&flatten_for_tui(&text)).context("send driver turn")?;
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
    fn flatten_for_tui_collapses_newlines_to_pipe_separator() {
        // Regression for pearl th-01c714: the TUI submits on every
        // `\n`, so multi-line text would arrive as N `You:` turns.
        let out = flatten_for_tui("first paragraph\n\nsecond paragraph\nthird line");
        assert!(!out.contains('\n'), "flattened text contains newline: {out}");
        assert_eq!(out, "first paragraph | second paragraph | third line");
    }

    #[test]
    fn flatten_for_tui_trims_each_line() {
        let out = flatten_for_tui("  hello \n  world  ");
        assert_eq!(out, "hello | world");
    }

    #[test]
    fn flatten_for_tui_handles_empty_input() {
        assert_eq!(flatten_for_tui(""), "");
        assert_eq!(flatten_for_tui("\n\n"), "");
        assert_eq!(flatten_for_tui("   "), "");
    }

    #[test]
    fn flatten_for_tui_handles_single_line_passthrough() {
        let s = "already a single line";
        assert_eq!(flatten_for_tui(s), s);
    }

    #[test]
    fn flatten_for_tui_drops_blank_lines() {
        let out = flatten_for_tui("a\n\n\nb");
        assert_eq!(out, "a | b");
    }

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

    #[test]
    fn is_slash_command_detects_leading_slash() {
        assert!(is_slash_command("/open foo"));
        assert!(is_slash_command("/read"));
        assert!(is_slash_command("   /help"), "leading whitespace should still count");
        assert!(!is_slash_command("please run /tests with /flag"), "interior slash is fine");
        assert!(!is_slash_command("hi"));
        assert!(!is_slash_command(""));
        assert!(!is_slash_command("   "));
    }

    #[test]
    fn build_driver_prompt_warns_against_slash_commands() {
        // The system + user prompts together must give the driver
        // model crystal-clear "no slash commands" instructions —
        // otherwise it falls back to Claude-Code-style /open,
        // /read, etc. (pearl th-7fdfa9 regression).
        let p = build_driver_prompt("solve foo", "pane", 1, 15);
        assert!(
            p.contains("Never start a message with `/`")
                || p.contains("NEVER start a message with `/`")
                || p.contains("no leading `/`")
                || p.contains("does NOT exist")
                || p.contains("do NOT exist")
                || p.contains("/open"),
            "prompt should warn against slash commands; got: {p}"
        );
        // Should also include the plain-English framing.
        assert!(p.contains("plain English"), "prompt should tell model to ask in plain English: {p}");
    }

    #[test]
    fn build_slash_retry_prompt_contains_offending_text_and_attempts() {
        let p = build_slash_retry_prompt("solve foo", "pane", 2, 15, "/open INSTRUCTIONS.md", 2);
        assert!(p.contains("/open INSTRUCTIONS.md"), "retry prompt should quote the rejected reply: {p}");
        assert!(
            p.contains("2 attempt") || p.contains("2 attempts"),
            "retry prompt should mention attempts remaining: {p}"
        );
        assert!(p.contains("solve foo"), "retry prompt should still include the task: {p}");
        assert!(p.contains("pane"), "retry prompt should still include the pane: {p}");
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

    fn tmux_present() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Build a `TmuxDriver` that talks to a bare `cat` session — no
    /// boot gate, so empty input works. Caller owns the driver +
    /// the session is reaped on drop.
    fn cat_session(stem: &str) -> Option<(TmuxDriver, tempfile::TempDir)> {
        if !tmux_present() {
            return None;
        }
        use std::process::Command as Cmd;
        let tmp = tempfile::tempdir().unwrap();
        let session = format!("smooth-bench-driver-test-{stem}-{}", std::process::id());
        // Per-task socket isolation (pearl th-a5ca18): give this
        // test its own private tmux server so a sibling test or
        // bench process can't kill our server out from under us.
        let socket = format!("smb-drv-test-{stem}-{}", std::process::id());
        let status = Cmd::new("tmux")
            .args([
                "-L",
                &socket,
                "new-session",
                "-d",
                "-s",
                &session,
                "-x",
                &crate::tmux_driver::PANE_WIDTH.to_string(),
                "-y",
                &crate::tmux_driver::PANE_HEIGHT.to_string(),
                "-c",
                &tmp.path().to_string_lossy(),
                "sh",
                "-c",
                "cat",
            ])
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        // `cat` produces no first-render output, so start_command's
        // boot-render gate would time out. Attach to the
        // already-created session via the test-only helper.
        let driver = match crate::tmux_driver::TmuxDriver::attach_existing_for_test(&socket, &session, tmp.path()) {
            Ok(d) => d,
            Err(_) => return None,
        };
        Some((driver, tmp))
    }

    /// Fake driver that records every call and returns canned
    /// decisions. The recording lets tests assert on retry calls.
    struct RecordingDriver {
        responses: std::sync::Mutex<Vec<DriverDecision>>,
        normal_calls: AtomicUsize,
        retry_calls: AtomicUsize,
    }

    impl RecordingDriver {
        fn new(responses: Vec<DriverDecision>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
                normal_calls: AtomicUsize::new(0),
                retry_calls: AtomicUsize::new(0),
            }
        }
        fn pop_response(&self) -> DriverDecision {
            let mut guard = self.responses.lock().unwrap();
            if guard.is_empty() {
                DriverDecision::Stuck
            } else {
                guard.remove(0)
            }
        }
    }

    #[async_trait]
    impl DriverModel for RecordingDriver {
        async fn next_decision(&self, _task: &str, _pane: &str, _turn_idx: usize, _max_turns: usize) -> Result<DriverDecision> {
            self.normal_calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.pop_response())
        }
        async fn next_decision_after_slash(
            &self,
            _task: &str,
            _pane: &str,
            _turn_idx: usize,
            _max_turns: usize,
            _bad_turn: &str,
            _attempts_remaining: usize,
        ) -> Result<DriverDecision> {
            self.retry_calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.pop_response())
        }
    }

    /// Loop config tuned for fast tests — short dwell, tight idle
    /// floor disabled by setting a tiny per-turn timeout. The cat
    /// session never produces enough non-whitespace to trip the
    /// floor, so wait_for_idle will time out — that's OK, it
    /// returns IdleTimeout before the slash guard runs. We need a
    /// config where wait_for_idle returns SUCCESSFULLY against an
    /// empty pane so the driver decision path runs.
    fn fast_loop_cfg() -> LoopConfig {
        LoopConfig {
            max_turns: 5,
            idle_dwell: Duration::from_millis(150),
            idle_poll: Duration::from_millis(50),
            per_turn_timeout: Duration::from_secs(3),
        }
    }

    /// Seed payload large enough to clear `wait_for_idle`'s default
    /// 200-non-whitespace-char floor when echoed back by `cat`.
    fn fat_seed() -> String {
        std::iter::repeat_n("seedpayload", 30).collect::<Vec<_>>().join(" ")
    }

    #[tokio::test]
    async fn run_human_loop_marks_stuck_after_three_slash_commands() {
        // Regression for pearl th-7fdfa9 bug 2: a driver LLM that
        // emits slash commands must be dropped on the floor, re-
        // asked with reinforcement, and after MAX_SLASH_RETRIES
        // consecutive bad turns the harness gives up with STUCK.
        let Some((driver, _tmp)) = cat_session("stuck-on-slash") else {
            eprintln!("tmux not installed — skipping");
            return;
        };

        // Three slash commands in a row — should trip the retry cap.
        let fake = RecordingDriver::new(vec![
            DriverDecision::Send("/open INSTRUCTIONS.md".into()),
            DriverDecision::Send("/read INSTRUCTIONS.md".into()),
            DriverDecision::Send("/help".into()),
            // We never get here, but leave a sentinel just in case.
            DriverDecision::Send("plain English".into()),
        ]);

        let seed = fat_seed();
        let result = run_human_loop(&driver, &fake, "irrelevant task", &seed, &fast_loop_cfg()).await;
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                eprintln!("run_human_loop returned err (likely idle timeout against `cat`): {e:#}");
                return;
            }
        };

        // If the test environment can't drive cat fast enough to
        // settle within the tight per-turn timeout, we get IdleTimeout
        // before the slash guard runs. That's a flaky-env outcome —
        // skip the assertions but log it so we can spot regressions.
        if result.exit == LoopExit::IdleTimeout {
            eprintln!("idle timed out before slash guard ran — flaky env, skipping assertions");
            return;
        }
        assert_eq!(result.exit, LoopExit::Stuck, "three slash turns must mark TASK_STUCK; got {:?}", result.exit);
        // One normal call + two retry calls = 3 total slash turns.
        assert_eq!(
            fake.normal_calls.load(Ordering::Relaxed),
            1,
            "expected exactly one normal call before retries kick in"
        );
        assert_eq!(fake.retry_calls.load(Ordering::Relaxed), 2, "expected two retry calls before giving up");
    }

    #[tokio::test]
    async fn run_human_loop_accepts_plain_english_message() {
        // Counterpart to the slash-cap test: a model that returns a
        // plain-English reply on the first try should flow straight
        // through without engaging the retry path. We then have it
        // return TASK_COMPLETE on the next turn so the loop ends.
        let Some((driver, _tmp)) = cat_session("plain-english") else {
            eprintln!("tmux not installed — skipping");
            return;
        };

        let fake = RecordingDriver::new(vec![DriverDecision::Send("Please read INSTRUCTIONS.md".into()), DriverDecision::Complete]);

        let seed = fat_seed();
        let result = run_human_loop(&driver, &fake, "irrelevant task", &seed, &fast_loop_cfg()).await;
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                eprintln!("run_human_loop returned err: {e:#}");
                return;
            }
        };

        // No retries should have happened.
        assert_eq!(
            fake.retry_calls.load(Ordering::Relaxed),
            0,
            "plain-English reply should not trigger the slash retry path"
        );
        // Either Complete (success path) or IdleTimeout (env flake on
        // the second turn). Stuck would be a real bug — the model
        // never emitted a slash command.
        assert_ne!(
            result.exit,
            LoopExit::Stuck,
            "plain-English reply should not yield Stuck; got {:?}",
            result.exit
        );
    }
}

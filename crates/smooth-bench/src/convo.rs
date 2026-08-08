//! Multi-turn agentic conversation harness (pearl th-f19853).
//!
//! The [`agentic`](crate::agentic) suite scores ONE turn against a
//! workspace: did the agent take the right actions? That misses the class
//! of failure that actually bites users — the "ical" mess, where Big
//! Smooth gave three mutually contradictory calendar answers in one
//! conversation. No single turn was obviously wrong; the CONVERSATION was.
//!
//! So this module holds a real multi-turn conversation instead:
//!
//! 1. Open ONE canonical-protocol session against a live Big Smooth
//!    ([`CanonicalSession`]) — same session id for every turn, because the
//!    agent's own memory of the thread is what's under test.
//! 2. An LLM **driver** plays a realistic user turn after turn, given a
//!    persona and the transcript so far. It stops early when it decides
//!    the goal is met (or hopeless).
//! 3. An LLM **judge** scores the whole thread against the scenario's
//!    rubric on four axes — helpfulness, correctness, tool use, and
//!    CONSISTENCY across turns — and returns a PASS/FAIL.
//!
//! Two things make it a regression test rather than a demo:
//!
//! - **A judge failure is never a PASS.** Same invariant as
//!   [`crate::judge`]: transport error, garbage verdict, missing score
//!   line → `INCONCLUSIVE`.
//! - **`expect_fail` scenarios.** `rapid-correction` documents the
//!   th-3a912a interrupt gap: a correction sent before the first turn
//!   finishes. It is EXPECTED to fail today (recorded `XFAIL`, suite still
//!   green) and will report `XPASS` — loudly, non-zero exit — the day
//!   interrupts land, which is the signal to drop the flag and keep it as
//!   a real test.
//!
//! These runs are slow, networked and cost money, so they live behind the
//! `smooth-bench convo` subcommand, never in `cargo test`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::canonical_driver::CanonicalSession;
use crate::judge::{parse_verdict, truncate, JudgeVerdict};

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// One multi-turn conversation scenario.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConvoScenario {
    pub id: String,
    /// One-line human description, echoed in the summary table.
    #[serde(default)]
    pub description: String,
    /// The first user message. Fixed rather than LLM-generated so every
    /// run starts from the same place and results stay comparable.
    pub opening: String,
    /// Who the simulated user is and what they want — the driver's brief.
    pub persona: String,
    /// Hard cap on user turns (including the opening).
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    /// What the judge grades the conversation against.
    pub rubric: String,
    /// Fire a second user message before the first turn completes.
    #[serde(default)]
    pub barge_in: Option<BargeIn>,
    /// This scenario documents a known gap: FAIL is the expected result
    /// and keeps the suite green; PASS is the news.
    #[serde(default)]
    pub expect_fail: bool,
}

/// A correction fired mid-turn — the concurrent-turn case.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BargeIn {
    /// The correcting message.
    pub message: String,
    /// How long to wait after the opening before sending it. Long enough
    /// that the agent has started work, short enough that it can't have
    /// finished.
    pub after_ms: u64,
}

const fn default_max_turns() -> usize {
    3
}

/// Raw TOML wrapper: `[[scenario]]` array.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConvoFile {
    #[serde(default)]
    scenario: Vec<ConvoScenario>,
}

/// Load the conversation scenarios baked into the binary.
///
/// # Errors
/// Errors when the embedded TOML fails to parse or validate — caught by
/// the unit tests before merge.
pub fn default_convo_scenarios() -> Result<Vec<ConvoScenario>> {
    parse_convo_scenarios(include_str!("../convo-scenarios.toml"))
}

/// Parse + validate a conversation scenario document.
///
/// Validation is the point: an empty rubric or a zero turn cap is a
/// silently-useless benchmark, so it fails loud at load.
///
/// # Errors
/// Errors on malformed TOML or any validation failure.
pub fn parse_convo_scenarios(toml_str: &str) -> Result<Vec<ConvoScenario>> {
    let file: ConvoFile = toml::from_str(toml_str).context("parsing convo scenario TOML")?;
    anyhow::ensure!(!file.scenario.is_empty(), "scenario file contains no [[scenario]] entries");

    let mut seen = std::collections::HashSet::new();
    for s in &file.scenario {
        anyhow::ensure!(!s.id.trim().is_empty(), "scenario has an empty id");
        anyhow::ensure!(seen.insert(s.id.clone()), "duplicate scenario id: {}", s.id);
        anyhow::ensure!(!s.opening.trim().is_empty(), "scenario {} has an empty opening", s.id);
        anyhow::ensure!(!s.persona.trim().is_empty(), "scenario {} has an empty persona", s.id);
        anyhow::ensure!(!s.rubric.trim().is_empty(), "scenario {} has an empty rubric", s.id);
        anyhow::ensure!(s.max_turns >= 1, "scenario {} has max_turns = 0", s.id);
        if let Some(b) = &s.barge_in {
            anyhow::ensure!(!b.message.trim().is_empty(), "scenario {}: barge_in has an empty message", s.id);
        }
    }
    Ok(file.scenario)
}

// ---------------------------------------------------------------------------
// Transcript
// ---------------------------------------------------------------------------

/// One user message and the reply it got.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ConvoTurn {
    /// 0-based turn number within the conversation.
    pub index: usize,
    pub user: String,
    pub assistant: String,
    /// `name -> ok|error`, in call order.
    pub tools: Vec<String>,
    /// False when the turn never reached `eventual_response` (dropped,
    /// timed out, or errored) — itself a finding worth judging.
    pub completed: bool,
    /// Set when the turn ended in a transport/protocol error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: u64,
    pub cost_usd: f64,
}

/// Render the thread the way both the driver and the judge read it.
#[must_use]
pub fn render_transcript(turns: &[ConvoTurn]) -> String {
    use std::fmt::Write;
    if turns.is_empty() {
        return "(the conversation has not started)".to_string();
    }
    let mut out = String::new();
    for t in turns {
        let _ = writeln!(out, "### TURN {} — USER\n{}", t.index + 1, t.user.trim());
        if t.tools.is_empty() {
            let _ = writeln!(out, "\n[tool calls: none]");
        } else {
            let _ = writeln!(out, "\n[tool calls: {}]", t.tools.join(", "));
        }
        let reply = if t.assistant.trim().is_empty() {
            match &t.error {
                Some(e) => format!("(no reply — {e})"),
                None => "(the assistant said nothing)".to_string(),
            }
        } else {
            truncate(t.assistant.trim(), 4_000)
        };
        let _ = writeln!(out, "### TURN {} — ASSISTANT\n{reply}\n", t.index + 1);
        if !t.completed {
            let _ = writeln!(out, "[!] this turn never completed\n");
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The LLM driver — plays the user
// ---------------------------------------------------------------------------

/// Marker the driver emits when it considers the conversation over.
const DRIVER_DONE: &str = "DONE";

const DRIVER_SYSTEM: &str = "You are role-playing a HUMAN user talking to a personal AI assistant. \
You are given your persona/goal and the conversation so far. Write ONLY the next message you would send — no narration, no quotes, no labels. \
Keep it short and natural, the way a person types. Do not be a helpful assistant; you are the one being helped. \
If your goal has been met, or the assistant has clearly told you it cannot help and you have nothing left to try, reply with exactly: DONE";

/// Ask the driver model for the next user message.
///
/// Returns `None` when the driver decides the conversation is over
/// (`DONE`), which is also how an empty reply is treated.
///
/// # Errors
/// Errors on transport failure, a non-2xx gateway response, or a missing
/// message body — the caller ends the conversation and records why.
pub async fn driver_next_message(
    gateway_url: &str,
    gateway_key: Option<&str>,
    model: &str,
    scenario: &ConvoScenario,
    turns: &[ConvoTurn],
) -> Result<Option<String>> {
    let prompt = format!(
        "## YOUR PERSONA AND GOAL\n{}\n\n## CONVERSATION SO FAR\n{}\n\n## YOUR NEXT MESSAGE",
        scenario.persona.trim(),
        render_transcript(turns)
    );
    let content = chat(
        gateway_url,
        gateway_key,
        model,
        DRIVER_SYSTEM,
        &prompt,
        smooth_policy::llm_params::AGENT_TEMPERATURE,
    )
    .await?;
    let msg = content.trim().trim_matches('"').trim().to_string();
    if msg.is_empty() || msg.eq_ignore_ascii_case(DRIVER_DONE) || msg.starts_with(DRIVER_DONE) {
        return Ok(None);
    }
    Ok(Some(msg))
}

// ---------------------------------------------------------------------------
// The LLM judge — scores the thread
// ---------------------------------------------------------------------------

const JUDGE_SYSTEM: &str = "You grade a multi-turn conversation between a user and an AI assistant against a rubric. \
You see the rubric, the persona the user was playing, and the full transcript including which tools the assistant called each turn. \
Grade the CONVERSATION, not any single message: contradictions between turns, stale answers to superseded questions, and claims unsupported by a tool call are the failures that matter most. \
Be strict — an unmet rubric point is a FAIL. Score each axis 1 (terrible) to 5 (excellent).\n\
Reply with EXACTLY these lines and nothing else:\n\
HELPFULNESS: <1-5>\n\
CORRECTNESS: <1-5>\n\
TOOL_USE: <1-5>\n\
CONSISTENCY: <1-5>\n\
TURN <n>: <1-5> <short note>   (one line per turn)\n\
VERDICT: PASS\n\
REASON: <one sentence>";

/// Per-axis scores for one conversation. 1–5, higher is better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ConvoScores {
    pub helpfulness: u8,
    pub correctness: u8,
    pub tool_use: u8,
    /// The axis that exists for the ical case: does the thread agree with
    /// itself?
    pub consistency: u8,
}

/// The judge's full reading of one conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvoGrade {
    pub scores: ConvoScores,
    /// One `<n> -> <score> <note>` line per turn the judge commented on.
    pub per_turn: Vec<String>,
    pub verdict: JudgeVerdict,
}

/// Grade one finished conversation.
///
/// # Errors
/// Errors on transport failure, a non-2xx gateway response, or a reply
/// that doesn't match the contract. Callers map every one of those to
/// `INCONCLUSIVE`, never to PASS.
pub async fn judge_conversation(
    gateway_url: &str,
    gateway_key: Option<&str>,
    model: &str,
    scenario: &ConvoScenario,
    turns: &[ConvoTurn],
) -> Result<ConvoGrade> {
    let prompt = format!(
        "## RUBRIC\n{}\n\n## USER PERSONA\n{}\n\n## TRANSCRIPT\n{}",
        scenario.rubric.trim(),
        scenario.persona.trim(),
        truncate(&render_transcript(turns), 24_000)
    );
    let content = chat(
        gateway_url,
        gateway_key,
        model,
        JUDGE_SYSTEM,
        &prompt,
        smooth_policy::llm_params::AGENT_TEMPERATURE,
    )
    .await?;
    parse_grade(&content).ok_or_else(|| anyhow::anyhow!("judge reply did not match the contract: {}", truncate(&content, 400)))
}

/// Parse the judge's line contract.
///
/// Returns `None` unless all four axes AND a PASS/FAIL verdict are
/// present — a partially-parsed grade would be a silent pass.
#[must_use]
pub fn parse_grade(text: &str) -> Option<ConvoGrade> {
    let verdict = parse_verdict(text)?;
    let (mut helpfulness, mut correctness, mut tool_use, mut consistency) = (None, None, None, None);
    let mut per_turn = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['*', '#', '-', ' ']).trim();
        let Some((key, rest)) = line.split_once(':') else { continue };
        let key = key.trim().trim_matches(['*', '_']).trim().to_ascii_uppercase();
        let rest = rest.trim().trim_start_matches(['*', '_', ' ']);
        let slot = match key.as_str() {
            "HELPFULNESS" => &mut helpfulness,
            "CORRECTNESS" => &mut correctness,
            "TOOL_USE" | "TOOL USE" | "TOOLUSE" => &mut tool_use,
            "CONSISTENCY" => &mut consistency,
            k if k.starts_with("TURN ") => {
                per_turn.push(format!("{} -> {}", k.trim_start_matches("TURN ").trim(), rest));
                continue;
            }
            _ => continue,
        };
        *slot = parse_score(rest);
    }
    Some(ConvoGrade {
        scores: ConvoScores {
            helpfulness: helpfulness?,
            correctness: correctness?,
            tool_use: tool_use?,
            consistency: consistency?,
        },
        per_turn,
        verdict,
    })
}

/// Read a 1–5 score off the front of a line. Out-of-range or non-numeric
/// is `None`, which fails the whole grade rather than defaulting.
fn parse_score(s: &str) -> Option<u8> {
    let digits: String = s.trim().chars().take_while(char::is_ascii_digit).collect();
    match digits.parse::<u8>() {
        Ok(n) if (1..=5).contains(&n) => Some(n),
        _ => None,
    }
}

/// Resolve the driver/judge gateway from `~/.smooth/providers.json` —
/// the same credential store the daemon falls back to — so the suite runs
/// without exporting `SMOOAI_GATEWAY_*` by hand.
///
/// Returns `(api_url, api_key)` of the first OpenAI-compatible provider
/// carrying a key. Anthropic-format providers are skipped: the driver and
/// judge speak `/chat/completions`.
#[must_use]
pub fn gateway_from_providers_json() -> Option<(String, String)> {
    let path = dirs_next::home_dir()?.join(".smooth").join("providers.json");
    let root = smooth_cast::providers::load_value(&path).ok()?;
    root.get("providers")?.as_array()?.iter().find_map(|p| {
        let format = p.get("api_format").and_then(Value::as_str).unwrap_or("OpenAiCompat");
        if !format.eq_ignore_ascii_case("OpenAiCompat") {
            return None;
        }
        let url = p.get("api_url").and_then(Value::as_str)?;
        let key = p.get("api_key").and_then(Value::as_str).filter(|k| !k.is_empty())?;
        Some((url.to_string(), key.to_string()))
    })
}

/// One OpenAI-compatible chat completion against the gateway, returning
/// the assistant message content.
async fn chat(gateway_url: &str, gateway_key: Option<&str>, model: &str, system: &str, user: &str, temperature: f32) -> Result<String> {
    let url = format!("{}/chat/completions", gateway_url.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "temperature": temperature,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });
    let mut req = reqwest::Client::new().post(&url).json(&body);
    if let Some(k) = gateway_key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.with_context(|| format!("gateway request to {url}"))?;
    let status = resp.status();
    // This call is OUR spend, not the agent's. Record it so the
    // leaderboard can subtract it (pearl th-adf614) — otherwise a cheap
    // agent graded by an expensive judge reads as expensive.
    crate::spend::record_harness_response(resp.headers());
    let text = resp.text().await.context("reading gateway response body")?;
    anyhow::ensure!(status.is_success(), "gateway returned {status}: {}", truncate(&text, 400));
    let v: Value = serde_json::from_str(&text).with_context(|| format!("gateway response was not JSON: {}", truncate(&text, 400)))?;
    v.pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow::anyhow!("gateway response carried no message content: {}", truncate(&text, 400)))
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

/// How one conversation trial ended.
///
/// `XFail` / `XPass` exist so a scenario that documents a known gap can
/// live in the suite: while the gap is open it fails and the suite stays
/// green; the day it starts passing, `XPass` says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ConvoStatus {
    Pass,
    Fail,
    /// Expected failure — the documented gap is still there.
    XFail,
    /// Expected to fail but passed — drop `expect_fail`.
    XPass,
    /// Judge or transport failure. Never treated as a pass.
    Inconclusive,
}

impl ConvoStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::XFail => "XFAIL",
            Self::XPass => "XPASS",
            Self::Inconclusive => "INCONCLUSIVE",
        }
    }

    /// Whether this outcome lets the suite exit zero. An unexpected pass
    /// does not: it means the scenario's `expect_fail` flag is now a lie.
    #[must_use]
    pub const fn suite_ok(self) -> bool {
        matches!(self, Self::Pass | Self::XFail)
    }

    /// Fold a judge verdict and the scenario's expectation into a status.
    #[must_use]
    pub const fn from_verdict(passed: bool, expect_fail: bool) -> Self {
        match (passed, expect_fail) {
            (true, false) => Self::Pass,
            (true, true) => Self::XPass,
            (false, false) => Self::Fail,
            (false, true) => Self::XFail,
        }
    }
}

/// One trial's result — one JSON-lines record, transcript included.
#[derive(Debug, Clone, Serialize)]
pub struct ConvoOutcome {
    pub id: String,
    pub trial_index: usize,
    pub model: String,
    pub driver_model: String,
    pub judge_model: String,
    pub expect_fail: bool,
    pub status: ConvoStatus,
    /// The judge's sentence, or the error that made the trial inconclusive.
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scores: Option<ConvoScores>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub per_turn: Vec<String>,
    pub turns: Vec<ConvoTurn>,
    pub duration_ms: u64,
    pub cost_usd: f64,
}

/// A whole `smooth-bench convo` run.
#[derive(Debug, Clone, Serialize)]
pub struct ConvoRun {
    pub url: String,
    pub model: String,
    pub driver_model: String,
    pub judge_model: String,
    pub trials: usize,
    pub ran_at: chrono::DateTime<chrono::Utc>,
    pub results: Vec<ConvoOutcome>,
}

impl ConvoRun {
    /// One JSON object per line — the transcript archive.
    ///
    /// # Errors
    /// Errors only if an outcome fails to serialise.
    pub fn to_jsonl(&self) -> Result<String> {
        use std::fmt::Write;
        let mut out = String::new();
        for r in &self.results {
            let _ = writeln!(out, "{}", serde_json::to_string(r)?);
        }
        Ok(out)
    }

    /// Whether every trial's status lets the suite exit zero.
    #[must_use]
    pub fn suite_ok(&self) -> bool {
        self.flaky_expected_gaps().1
    }

    /// Scenario-level verdict for `expect_fail` gaps, plus whether the
    /// suite is green.
    ///
    /// `expect_fail` is a claim about a scenario, not about one trial, so
    /// it has to be judged across the trials of that scenario. Measured
    /// at `--trials 3`, the documented interrupt gap came back
    /// `XFAIL, XFAIL, XPASS` — it is not broken, it is **flaky**. Judging
    /// per-trial, that single XPASS failed the whole run (exit 1), so a
    /// ~1-in-3 flake would turn CI red at random. A benchmark that cries
    /// wolf gets ignored, which costs more than the gap it was reporting.
    ///
    /// So: an `expect_fail` scenario only counts as XPASS — "the gap is
    /// closed, drop the flag" — when it passed on EVERY conclusive trial.
    /// A partial pass is a flaky gap: the suite stays green and the
    /// flakiness is reported, because "sometimes works" is real
    /// information about the gap and not a reason to fail a build.
    ///
    /// Returns `(flaky gap descriptions, suite_ok)`.
    #[must_use]
    pub fn flaky_expected_gaps(&self) -> (Vec<String>, bool) {
        use std::collections::BTreeMap;

        let mut by_scenario: BTreeMap<&str, Vec<&ConvoOutcome>> = BTreeMap::new();
        for r in &self.results {
            by_scenario.entry(r.id.as_str()).or_default().push(r);
        }

        let mut flaky = Vec::new();
        let mut ok = true;
        for (id, trials) in by_scenario {
            if trials.first().is_some_and(|t| t.expect_fail) {
                let passed = trials.iter().filter(|t| t.status == ConvoStatus::XPass).count();
                let conclusive = trials.iter().filter(|t| t.status != ConvoStatus::Inconclusive).count();
                if conclusive > 0 && passed == conclusive {
                    // Consistently passing: the flag is now a lie.
                    ok = false;
                } else if passed > 0 {
                    flaky.push(format!("{id}: expected-fail gap passed {passed}/{conclusive} trials — flaky, not closed"));
                }
                // Any inconclusive trial still fails the suite: missing
                // data must never be read as a satisfied expectation.
                if trials.iter().any(|t| t.status == ConvoStatus::Inconclusive) {
                    ok = false;
                }
            } else if !trials.iter().all(|t| t.status.suite_ok()) {
                ok = false;
            }
        }
        (flaky, ok)
    }

    /// Human summary table.
    #[must_use]
    pub fn render_table(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "\nconvo — target {} · agent {} · driver {} · judge {}",
            self.url, self.model, self.driver_model, self.judge_model
        );
        let _ = writeln!(out, "{:<22} {:<6} {:<12} {:<5} {:>7} REASON", "SCENARIO", "TRIAL", "STATUS", "TURNS", "SCORES");
        for r in &self.results {
            let scores = r.scores.map_or_else(
                || "  -/-  ".to_string(),
                |s| format!("{}{}{}{}", s.helpfulness, s.correctness, s.tool_use, s.consistency),
            );
            let _ = writeln!(
                out,
                "{:<22} {:<6} {:<12} {:<5} {:>7} {}",
                r.id,
                r.trial_index + 1,
                r.status.as_str(),
                r.turns.len(),
                scores,
                truncate(&r.reason, 90).replace('\n', " ")
            );
        }
        let _ = writeln!(out, "\nSCORES = helpfulness/correctness/tool_use/consistency, each 1-5.");
        let ok = self.results.iter().filter(|r| r.status.suite_ok()).count();
        let _ = writeln!(out, "{ok}/{} trial(s) OK.", self.results.len());
        out
    }
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Knobs for one convo run.
#[derive(Debug, Clone)]
pub struct ConvoOpts {
    /// Base HTTP URL of the running Big Smooth.
    pub url: String,
    /// Auth token (the rust daemon runs strict-auth).
    pub token: Option<String>,
    /// The model the agent under test runs (recorded, not set — the
    /// daemon owns its own routing).
    pub model: String,
    pub driver_model: String,
    pub judge_model: String,
    pub gateway_url: String,
    pub gateway_key: Option<String>,
    pub trials: usize,
    /// Per-turn deadline.
    pub turn_deadline: Duration,
}

/// Run every scenario `opts.trials` times against the daemon at
/// `opts.url`.
///
/// Never returns `Err` for a scenario-level failure — a dead turn is a
/// verdict, because a suite that aborts halfway tells you nothing.
pub async fn run_convo(scenarios: &[ConvoScenario], opts: &ConvoOpts) -> ConvoRun {
    let trials = opts.trials.max(1);
    let mut results = Vec::with_capacity(scenarios.len() * trials);
    for s in scenarios {
        for trial in 0..trials {
            eprintln!("convo: {} (trial {}/{trials}) …", s.id, trial + 1);
            results.push(run_one(s, trial, opts).await);
        }
    }
    ConvoRun {
        url: opts.url.clone(),
        model: opts.model.clone(),
        driver_model: opts.driver_model.clone(),
        judge_model: opts.judge_model.clone(),
        trials,
        ran_at: chrono::Utc::now(),
        results,
    }
}

async fn run_one(s: &ConvoScenario, trial_index: usize, opts: &ConvoOpts) -> ConvoOutcome {
    let t0 = Instant::now();
    let mut outcome = ConvoOutcome {
        id: s.id.clone(),
        trial_index,
        model: opts.model.clone(),
        driver_model: opts.driver_model.clone(),
        judge_model: opts.judge_model.clone(),
        expect_fail: s.expect_fail,
        status: ConvoStatus::Inconclusive,
        reason: String::new(),
        scores: None,
        per_turn: Vec::new(),
        turns: Vec::new(),
        duration_ms: 0,
        cost_usd: 0.0,
    };

    let mut session = match CanonicalSession::connect(&opts.url, opts.token.as_deref()).await {
        Ok(sess) => sess,
        Err(e) => {
            outcome.reason = format!("could not open a session: {e:#}");
            outcome.duration_ms = elapsed_ms(t0);
            return outcome;
        }
    };

    outcome.turns = hold_conversation(&mut session, s, opts).await;
    session.close().await;
    outcome.cost_usd = outcome.turns.iter().map(|t| t.cost_usd).sum();
    outcome.duration_ms = elapsed_ms(t0);

    // An empty reply is only "nothing to grade" when the agent ALSO did
    // nothing. A build-shaped turn (scaffold a project, install deps,
    // write files) can run for minutes and produce no prose until the
    // end — discarding it threw away a complete, correct `create-next-app`
    // scaffold as INCONCLUSIVE and the result had to be read off the
    // filesystem by hand (th-b59d2b). Tool calls ARE evidence, and the
    // judge already receives the tool transcript.
    let said_nothing = outcome.turns.iter().all(|t| t.assistant.trim().is_empty());
    let did_nothing = outcome.turns.iter().all(|t| t.tools.is_empty());
    if said_nothing && did_nothing {
        outcome.reason = "the assistant never said anything and called no tools — nothing to grade".to_string();
        return outcome;
    }
    if said_nothing {
        // Gradeable, but the silence is itself a finding: an agent that
        // works for minutes and never reports back is a bad turn, not an
        // unmeasurable one. Say so to the judge rather than letting it
        // infer from an empty transcript.
        eprintln!(
            "convo: {} produced no text but made {} tool call(s) — grading on the tool transcript",
            s.id,
            outcome.turns.iter().map(|t| t.tools.len()).sum::<usize>()
        );
    }

    match judge_conversation(&opts.gateway_url, opts.gateway_key.as_deref(), &opts.judge_model, s, &outcome.turns).await {
        Ok(g) => {
            outcome.status = ConvoStatus::from_verdict(g.verdict.passed, s.expect_fail);
            outcome.reason = g.verdict.reason;
            outcome.scores = Some(g.scores);
            outcome.per_turn = g.per_turn;
        }
        // Never a silent PASS: a broken judge is missing data.
        Err(e) => outcome.reason = format!("judge inconclusive: {e:#}"),
    }
    outcome
}

/// Drive the conversation: opening (plus any barge-in), then LLM-driven
/// user turns up to the scenario's cap.
async fn hold_conversation(session: &mut CanonicalSession, s: &ConvoScenario, opts: &ConvoOpts) -> Vec<ConvoTurn> {
    let mut turns = Vec::new();

    // Opening. With a barge-in we fire the correction WITHOUT waiting for
    // the first turn — that concurrency is the whole point — then collect
    // both replies in order.
    let t0 = Instant::now();
    if let Err(e) = session.send(&s.opening).await {
        turns.push(dead_turn(0, &s.opening, &format!("send failed: {e:#}"), elapsed_ms(t0)));
        return turns;
    }
    let mut pending = vec![s.opening.clone()];
    if let Some(b) = &s.barge_in {
        tokio::time::sleep(Duration::from_millis(b.after_ms)).await;
        match session.send(&b.message).await {
            Ok(()) => pending.push(b.message.clone()),
            Err(e) => eprintln!("convo: barge-in send failed: {e:#}"),
        }
    }
    for (i, user) in pending.into_iter().enumerate() {
        turns.push(collect_turn(session, i, &user, opts).await);
    }

    // Then hand the wheel to the driver.
    while turns.len() < s.max_turns {
        // A dead turn means the daemon is gone; more driving is noise.
        if !turns.last().is_some_and(|t: &ConvoTurn| t.completed) {
            break;
        }
        let next = match driver_next_message(&opts.gateway_url, opts.gateway_key.as_deref(), &opts.driver_model, s, &turns).await {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => {
                eprintln!("convo: driver failed, ending conversation early: {e:#}");
                break;
            }
        };
        let idx = turns.len();
        let t = Instant::now();
        if let Err(e) = session.send(&next).await {
            turns.push(dead_turn(idx, &next, &format!("send failed: {e:#}"), elapsed_ms(t)));
            break;
        }
        turns.push(collect_turn(session, idx, &next, opts).await);
    }
    turns
}

/// Collect one reply, folding every failure mode into a turn record.
async fn collect_turn(session: &mut CanonicalSession, index: usize, user: &str, opts: &ConvoOpts) -> ConvoTurn {
    let t0 = Instant::now();
    match session.collect_turn(opts.turn_deadline).await {
        Ok(out) => ConvoTurn {
            index,
            user: user.to_string(),
            assistant: out.text,
            tools: out
                .tool_calls
                .iter()
                .map(|t| format!("{} -> {}", t.name, if t.success { "ok" } else { "error" }))
                .collect(),
            completed: out.completed,
            error: (!out.completed).then(|| "the turn never completed".to_string()),
            duration_ms: elapsed_ms(t0),
            cost_usd: out.cost,
        },
        Err(e) => dead_turn(index, user, &format!("{e:#}"), elapsed_ms(t0)),
    }
}

fn dead_turn(index: usize, user: &str, error: &str, duration_ms: u64) -> ConvoTurn {
    ConvoTurn {
        index,
        user: user.to_string(),
        completed: false,
        error: Some(error.to_string()),
        duration_ms,
        ..ConvoTurn::default()
    }
}

fn elapsed_ms(t0: Instant) -> u64 {
    t0.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn turn(index: usize, user: &str, assistant: &str) -> ConvoTurn {
        ConvoTurn {
            index,
            user: user.into(),
            assistant: assistant.into(),
            completed: true,
            ..ConvoTurn::default()
        }
    }

    #[test]
    fn embedded_scenarios_parse_and_cover_the_required_cases() {
        let s = default_convo_scenarios().unwrap();
        let ids: Vec<&str> = s.iter().map(|x| x.id.as_str()).collect();
        for required in ["calendar-list", "rapid-correction"] {
            assert!(ids.contains(&required), "missing scenario {required}: {ids:?}");
        }
        // The interrupt scenario must barge in AND be marked expected-fail,
        // or it stops being the th-3a912a regression test.
        let rc = s.iter().find(|x| x.id == "rapid-correction").unwrap();
        assert!(rc.expect_fail, "rapid-correction must be expect_fail while th-3a912a is open");
        assert!(rc.barge_in.is_some(), "rapid-correction must send a correction mid-turn");
    }

    #[test]
    fn scenario_validation_rejects_broken_documents() {
        for (bad, why) in [
            ("", "no scenarios"),
            ("[[scenario]]\nid=''\nopening='hi'\npersona='p'\nrubric='r'", "empty id"),
            ("[[scenario]]\nid='a'\nopening='hi'\npersona='p'\nrubric=''", "empty rubric"),
            ("[[scenario]]\nid='a'\nopening=''\npersona='p'\nrubric='r'", "empty opening"),
            ("[[scenario]]\nid='a'\nopening='hi'\npersona='p'\nrubric='r'\nmax_turns=0", "zero turns"),
            (
                "[[scenario]]\nid='a'\nopening='hi'\npersona='p'\nrubric='r'\n[[scenario]]\nid='a'\nopening='hi'\npersona='p'\nrubric='r'",
                "duplicate id",
            ),
        ] {
            assert!(parse_convo_scenarios(bad).is_err(), "should have been rejected ({why})");
        }
    }

    #[test]
    fn parses_the_full_grade_contract() {
        let g = parse_grade(
            "HELPFULNESS: 4\nCORRECTNESS: 2\nTOOL_USE: 5\nCONSISTENCY: 1\nTURN 1: 4 fine\nTURN 2: 1 contradicted turn 1\nVERDICT: FAIL\nREASON: it contradicted itself",
        )
        .unwrap();
        assert_eq!(
            g.scores,
            ConvoScores {
                helpfulness: 4,
                correctness: 2,
                tool_use: 5,
                consistency: 1
            }
        );
        assert!(!g.verdict.passed);
        assert_eq!(g.verdict.reason, "it contradicted itself");
        assert_eq!(g.per_turn.len(), 2);
        assert!(g.per_turn[1].contains("contradicted"), "{:?}", g.per_turn);
    }

    #[test]
    fn grade_tolerates_markdown_decoration() {
        let g = parse_grade("**HELPFULNESS:** 5\n- correctness: 5\n**TOOL USE:** 4\nCONSISTENCY: 5\nVERDICT: PASS\nREASON: solid").unwrap();
        assert_eq!(g.scores.tool_use, 4);
        assert!(g.verdict.passed);
    }

    /// Every one of these must be INCONCLUSIVE at the call site. A grade
    /// missing an axis or a verdict must never become a pass.
    #[test]
    fn partial_grades_are_none_never_a_silent_pass() {
        for bad in [
            "",
            "VERDICT: PASS\nREASON: no scores at all",
            "HELPFULNESS: 5\nCORRECTNESS: 5\nTOOL_USE: 5\nVERDICT: PASS",  // missing consistency
            "HELPFULNESS: 5\nCORRECTNESS: 5\nTOOL_USE: 5\nCONSISTENCY: 5", // missing verdict
            "HELPFULNESS: 9\nCORRECTNESS: 5\nTOOL_USE: 5\nCONSISTENCY: 5\nVERDICT: PASS", // out of range
            "HELPFULNESS: great\nCORRECTNESS: 5\nTOOL_USE: 5\nCONSISTENCY: 5\nVERDICT: PASS", // non-numeric
        ] {
            assert!(parse_grade(bad).is_none(), "must be INCONCLUSIVE: {bad:?}");
        }
    }

    #[test]
    fn score_parser_bounds() {
        assert_eq!(parse_score("5"), Some(5));
        assert_eq!(parse_score(" 3 — solid"), Some(3));
        assert_eq!(parse_score("0"), None);
        assert_eq!(parse_score("6"), None);
        assert_eq!(parse_score("five"), None);
        assert_eq!(parse_score(""), None);
    }

    #[test]
    fn status_folds_expectation_into_the_verdict() {
        assert_eq!(ConvoStatus::from_verdict(true, false), ConvoStatus::Pass);
        assert_eq!(ConvoStatus::from_verdict(false, false), ConvoStatus::Fail);
        assert_eq!(ConvoStatus::from_verdict(false, true), ConvoStatus::XFail);
        assert_eq!(ConvoStatus::from_verdict(true, true), ConvoStatus::XPass);
        // A known gap that closed must NOT stay silently green.
        assert!(ConvoStatus::XFail.suite_ok());
        assert!(!ConvoStatus::XPass.suite_ok());
        assert!(!ConvoStatus::Inconclusive.suite_ok());
        assert!(!ConvoStatus::Fail.suite_ok());
        assert!(ConvoStatus::Pass.suite_ok());
    }

    #[test]
    fn transcript_renders_turns_tools_and_gaps() {
        let mut t1 = turn(0, "list my calendar", "you have 3 meetings");
        t1.tools = vec!["read_file -> ok".into()];
        let mut t2 = turn(1, "are you sure?", "");
        t2.completed = false;
        t2.error = Some("timed out".into());
        let r = render_transcript(&[t1, t2]);
        assert!(r.contains("### TURN 1 — USER"), "{r}");
        assert!(r.contains("[tool calls: read_file -> ok]"), "{r}");
        assert!(r.contains("### TURN 2 — ASSISTANT"), "{r}");
        assert!(r.contains("(no reply — timed out)"), "{r}");
        assert!(r.contains("this turn never completed"), "{r}");
        // A turn with no tools is marked, not omitted — "it answered
        // without looking anything up" is exactly what the judge grades.
        assert!(r.contains("[tool calls: none]"), "{r}");
    }

    #[test]
    fn empty_transcript_is_marked_not_blank() {
        assert!(render_transcript(&[]).contains("not started"));
    }

    #[test]
    fn run_summary_counts_xfail_as_ok_and_xpass_as_not() {
        let mk = |id: &str, status| ConvoOutcome {
            id: id.into(),
            trial_index: 0,
            model: "m".into(),
            driver_model: "d".into(),
            judge_model: "j".into(),
            expect_fail: true,
            status,
            reason: "because".into(),
            scores: Some(ConvoScores {
                helpfulness: 3,
                correctness: 3,
                tool_use: 3,
                consistency: 1,
            }),
            per_turn: vec![],
            turns: vec![turn(0, "hi", "hello")],
            duration_ms: 10,
            cost_usd: 0.0,
        };
        let mut run = ConvoRun {
            url: "http://127.0.0.1:8791".into(),
            model: "m".into(),
            driver_model: "d".into(),
            judge_model: "j".into(),
            trials: 1,
            ran_at: chrono::Utc::now(),
            results: vec![mk("a", ConvoStatus::Pass), mk("b", ConvoStatus::XFail)],
        };
        assert!(run.suite_ok());
        let table = run.render_table();
        assert!(table.contains("XFAIL"), "{table}");
        assert!(table.contains("2/2 trial(s) OK"), "{table}");

        run.results.push(mk("c", ConvoStatus::XPass));
        assert!(!run.suite_ok(), "an unexpected pass must fail the suite so the flag gets dropped");

        // Every record round-trips as one JSONL line, transcript included.
        let jsonl = run.to_jsonl().unwrap();
        assert_eq!(jsonl.lines().count(), 3);
        let first: Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        assert_eq!(first["status"], "PASS");
        assert_eq!(first["turns"][0]["user"], "hi");
    }
    fn turn_with_tools(assistant: &str, tools: &[&str]) -> ConvoTurn {
        ConvoTurn {
            tools: tools.iter().map(|t| format!("{t} -> ok")).collect(),
            ..turn(0, "do the thing", assistant)
        }
    }

    /// th-b59d2b: a build-shaped turn scaffolds a project over several
    /// minutes and produces no prose until the end. Treating "no text"
    /// as "nothing to grade" discarded a complete, correct
    /// `create-next-app` scaffold — the result had to be read off the
    /// filesystem by hand. Tool calls are evidence.
    #[test]
    fn a_silent_turn_that_did_work_is_still_gradeable() {
        let worked = vec![turn_with_tools("", &["bash", "write_file", "write_file"])];
        let said_nothing = worked.iter().all(|t| t.assistant.trim().is_empty());
        let did_nothing = worked.iter().all(|t| t.tools.is_empty());
        assert!(said_nothing, "premise: no prose");
        assert!(!did_nothing, "but it called tools — that is evidence");

        // And the judge must be able to SEE that evidence.
        let rendered = render_transcript(&worked);
        assert!(rendered.contains("write_file"), "tool calls must reach the judge: {rendered}");
        assert!(rendered.contains("said nothing"), "the silence must be visible too: {rendered}");
    }

    #[test]
    fn a_turn_with_neither_text_nor_tools_is_genuinely_ungradeable() {
        let nothing = vec![turn_with_tools("  ", &[])];
        assert!(nothing.iter().all(|t| t.assistant.trim().is_empty()));
        assert!(nothing.iter().all(|t| t.tools.is_empty()));
        let rendered = render_transcript(&nothing);
        assert!(rendered.contains("tool calls: none"));
    }
    fn outcome(id: &str, status: ConvoStatus, expect_fail: bool) -> ConvoOutcome {
        ConvoOutcome {
            id: id.to_string(),
            trial_index: 0,
            model: "m".into(),
            driver_model: "d".into(),
            judge_model: "j".into(),
            expect_fail,
            status,
            reason: String::new(),
            scores: None,
            per_turn: Vec::new(),
            turns: Vec::new(),
            duration_ms: 0,
            cost_usd: 0.0,
        }
    }

    fn run_of(results: Vec<ConvoOutcome>) -> ConvoRun {
        ConvoRun {
            url: "u".into(),
            model: "m".into(),
            driver_model: "d".into(),
            judge_model: "j".into(),
            trials: 3,
            ran_at: chrono::Utc::now(),
            results,
        }
    }

    /// Measured at --trials 3: the documented interrupt gap came back
    /// XFAIL, XFAIL, XPASS. Judged per-trial that single XPASS failed the
    /// whole run, so a ~1-in-3 flake would turn CI red at random.
    #[test]
    fn a_flaky_expected_gap_does_not_fail_the_suite() {
        let run = run_of(vec![
            outcome("rapid-correction", ConvoStatus::XFail, true),
            outcome("rapid-correction", ConvoStatus::XFail, true),
            outcome("rapid-correction", ConvoStatus::XPass, true),
        ]);
        let (flaky, ok) = run.flaky_expected_gaps();
        assert!(ok, "a partially-passing known gap must not fail the build");
        assert_eq!(flaky.len(), 1, "but it must be reported: {flaky:?}");
        assert!(flaky[0].contains("1/3"), "report the rate: {}", flaky[0]);
    }

    /// The flag is only a lie when the gap is CONSISTENTLY closed.
    #[test]
    fn a_consistently_passing_expected_gap_fails_the_suite() {
        let run = run_of(vec![
            outcome("rapid-correction", ConvoStatus::XPass, true),
            outcome("rapid-correction", ConvoStatus::XPass, true),
            outcome("rapid-correction", ConvoStatus::XPass, true),
        ]);
        let (flaky, ok) = run.flaky_expected_gaps();
        assert!(!ok, "a gap that always passes means expect_fail is stale — say so loudly");
        assert!(flaky.is_empty(), "that is not flakiness, it is a closed gap");
    }

    #[test]
    fn a_never_passing_expected_gap_is_green_and_quiet() {
        let run = run_of(vec![
            outcome("rapid-correction", ConvoStatus::XFail, true),
            outcome("rapid-correction", ConvoStatus::XFail, true),
        ]);
        let (flaky, ok) = run.flaky_expected_gaps();
        assert!(ok);
        assert!(flaky.is_empty());
    }

    /// Missing data must never read as a satisfied expectation.
    #[test]
    fn an_inconclusive_trial_still_fails_even_on_an_expected_gap() {
        let run = run_of(vec![
            outcome("rapid-correction", ConvoStatus::XFail, true),
            outcome("rapid-correction", ConvoStatus::Inconclusive, true),
        ]);
        assert!(!run.flaky_expected_gaps().1, "an inconclusive trial is missing data, not a pass");
    }

    #[test]
    fn ordinary_scenarios_are_unaffected() {
        assert!(run_of(vec![outcome("a", ConvoStatus::Pass, false), outcome("a", ConvoStatus::Pass, false)]).suite_ok());
        assert!(!run_of(vec![outcome("a", ConvoStatus::Pass, false), outcome("a", ConvoStatus::Fail, false)]).suite_ok());
        assert!(!run_of(vec![outcome("a", ConvoStatus::Inconclusive, false)]).suite_ok());
    }
}

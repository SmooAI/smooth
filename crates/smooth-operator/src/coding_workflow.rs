//! Multi-phase coding workflow.
//!
//! A structured alternative to `Agent::run_with_channel`'s single
//! unstructured loop. Each phase runs an `Agent` dispatched through
//! a different `Activity` slot so the routing can pick the right
//! model for the shape of the subtask:
//!
//! | Phase    | Activity slot   | What it does                                        |
//! |----------|-----------------|-----------------------------------------------------|
//! | ASSESS   | Thinking        | Read tests + stub + INSTRUCTIONS; infer spec        |
//! | PLAN     | Planning        | Decompose into an implementation plan               |
//! | EXECUTE  | Coding          | Write code via edit_file/write_file tools           |
//! | VERIFY   | Coding          | Invoke bash tool to run tests, report pass/fail     |
//! | REVIEW   | Reviewing       | Adversarial critique of diff + test failures        |
//! | FINALIZE | Thinking        | Holistic last-look before emitting Completed        |
//!
//! The loop routes REVIEW → EXECUTE when VERIFY reports failures,
//! and EXECUTE → FINALIZE when tests pass or the iteration cap is
//! hit. Each phase emits an `AgentEvent::PhaseStart` on entry so
//! TUIs can update their status bar with the phase, routing alias,
//! and upstream model.
//!
//! This module does NOT own the sandbox, the security hooks, or
//! the tool registry — the caller assembles those and hands them in.
//! All workflow does is sequence Agent invocations with the right
//! prompts, slots, and context.

use std::sync::Arc;

use anyhow::{anyhow, Context};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{Agent, AgentConfig, AgentEvent};
use crate::conversation::Message;
use crate::cost::CostBudget;
use crate::llm::LlmClient;
use crate::providers::{Activity, ProviderRegistry};
use crate::tool::ToolRegistry;

/// Fixed phase order for the coding workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingPhase {
    Assess,
    Plan,
    Execute,
    Verify,
    Review,
    Finalize,
}

impl CodingPhase {
    /// Canonical uppercase display label emitted in `PhaseStart`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Assess => "ASSESS",
            Self::Plan => "PLAN",
            Self::Execute => "EXECUTE",
            Self::Verify => "VERIFY",
            Self::Review => "REVIEW",
            Self::Finalize => "FINALIZE",
        }
    }

    /// Which routing slot this phase dispatches through.
    ///
    /// `VERIFY` and `EXECUTE` share the Coding slot — verify is just
    /// the agent running bash tests, same surface as writing code.
    /// `FINALIZE` uses Thinking so the last-look review has room to
    /// reason about the whole diff holistically.
    pub fn activity(self) -> Activity {
        match self {
            Self::Assess | Self::Finalize => Activity::Thinking,
            Self::Plan => Activity::Planning,
            Self::Execute | Self::Verify => Activity::Coding,
            Self::Review => Activity::Reviewing,
        }
    }

    /// Max iterations for the Agent instance inside this phase.
    /// Non-iterative phases (assess/plan/review/finalize) cap short;
    /// execute + verify get the full budget.
    fn max_iterations(self) -> u32 {
        match self {
            Self::Assess | Self::Plan | Self::Review | Self::Finalize => 8,
            Self::Execute => 40,
            Self::Verify => 6,
        }
    }
}

/// Input to `run_coding_workflow`.
pub struct CodingWorkflowConfig {
    /// Stable id for the operator running this workflow — echoed
    /// into every AgentEvent.
    pub operator_id: String,
    /// The task prompt the user gave (same shape as the existing
    /// single-agent path).
    pub task_prompt: String,
    /// Provider registry for resolving Activity slots to concrete
    /// LlmConfigs.
    pub registry: Arc<ProviderRegistry>,
    /// Tool registry the phases will share.
    pub tools: ToolRegistry,
    /// Optional global budget cap — prorated loosely across phases
    /// (each phase gets the full remaining amount when it runs).
    pub budget_usd: Option<f64>,
    /// Max outer-loop iterations (EXECUTE → VERIFY → REVIEW cycle
    /// count). 3 is enough for most tasks; bumps to 5 for harder
    /// benchmarks.
    pub max_outer_iterations: u32,
    /// Event sink — every AgentEvent from every phase flows here,
    /// plus `PhaseStart` markers between phases.
    pub tx: UnboundedSender<AgentEvent>,
}

/// Summary of what each phase produced. Carried across phases so
/// the next one can see the prior outputs as user-message context
/// instead of restarting cold.
///
/// The important field here is `goal_summary` — a short
/// crystallization of what the task is actually trying to achieve,
/// produced by the ASSESS phase and threaded through every later
/// phase. Keeps the agent anchored to the objective across many
/// iterations instead of drifting into whatever subproblem it's
/// currently solving.
#[derive(Debug, Default, Clone)]
struct WorkflowState {
    /// 2–4 sentence goal crystallization from ASSESS. Every later
    /// phase sees this at the top of its user prompt so the model
    /// doesn't lose sight of the objective after N review loops.
    goal_summary: Option<String>,
    /// Full assessment from ASSESS (goal + enumeration of test
    /// cases + constraints). Longer than `goal_summary`; fed to
    /// PLAN and REVIEW but not to EXECUTE/VERIFY (they only need
    /// the goal).
    assessment: Option<String>,
    plan: Option<String>,
    last_exec_summary: Option<String>,
    last_verify_output: Option<String>,
    verify_passed: bool,
    last_review: Option<String>,
    outer_iteration: u32,
    total_cost_usd: f64,
}

/// Run the full workflow end-to-end.
///
/// Returns the final cost on success. Errors bubble up when
/// provider resolution fails or the channel closes early; per-phase
/// LLM failures are handled internally by the inner `Agent` loop
/// and get logged as `AgentEvent::Error`, not propagated (same
/// contract as `Agent::run_with_channel`).
pub async fn run_coding_workflow(cfg: CodingWorkflowConfig) -> anyhow::Result<f64> {
    let mut state = WorkflowState::default();

    // ASSESS ------------------------------------------------------------
    run_phase(&cfg, CodingPhase::Assess, &mut state, assess_prompt).await?;

    // PLAN --------------------------------------------------------------
    run_phase(&cfg, CodingPhase::Plan, &mut state, plan_prompt).await?;

    // EXECUTE → VERIFY → (REVIEW → EXECUTE) loop ------------------------
    for _ in 0..cfg.max_outer_iterations {
        state.outer_iteration += 1;

        run_phase(&cfg, CodingPhase::Execute, &mut state, execute_prompt).await?;
        run_phase(&cfg, CodingPhase::Verify, &mut state, verify_prompt).await?;

        if state.verify_passed {
            break;
        }

        run_phase(&cfg, CodingPhase::Review, &mut state, review_prompt).await?;
    }

    // FINALIZE ----------------------------------------------------------
    run_phase(&cfg, CodingPhase::Finalize, &mut state, finalize_prompt).await?;

    // Single authoritative Completed event — mirrors what
    // Agent::run_with_channel emits at the end of its loop so bench
    // harness / TUI stream consumers don't need a new code path to
    // detect "workflow is done."
    let _ = cfg.tx.send(AgentEvent::Completed {
        agent_id: cfg.operator_id.clone(),
        iterations: state.outer_iteration,
        cost_usd: state.total_cost_usd,
    });

    Ok(state.total_cost_usd)
}

type PromptBuilder = fn(&WorkflowState, &str) -> (String, String);

async fn run_phase(cfg: &CodingWorkflowConfig, phase: CodingPhase, state: &mut WorkflowState, prompt_builder: PromptBuilder) -> anyhow::Result<()> {
    let slot = cfg.registry.routing.slot_for(phase.activity()).clone();
    let alias = slot.model.clone();
    let llm_config = cfg
        .registry
        .llm_config_for(phase.activity())
        .with_context(|| format!("resolving {} slot → LLM config", phase.activity_name()))?;

    let _ = cfg.tx.send(AgentEvent::PhaseStart {
        phase: phase.label().to_string(),
        alias: alias.clone(),
        upstream: None,
        iteration: state.outer_iteration.max(1),
    });

    let (system_prompt, user_prompt) = prompt_builder(state, &cfg.task_prompt);

    let mut agent_config = AgentConfig::new(format!("{}/{}", cfg.operator_id, phase.label().to_lowercase()), system_prompt, llm_config)
        .with_max_iterations(phase.max_iterations());
    if let Some(cap) = cfg.budget_usd {
        let remaining = (cap - state.total_cost_usd).max(0.0);
        agent_config = agent_config.with_budget(CostBudget {
            max_cost_usd: Some(remaining),
            max_tokens: None,
        });
    }

    // Each phase gets its own Agent with a FRESH conversation —
    // prior phase output is carried explicitly via `user_prompt` so
    // the model sees only what it needs, not a growing history.
    // Share the same ToolRegistry handle (Arc-backed internally).
    let agent = Agent::new(agent_config, cfg.tools.clone());
    let conversation = agent.run_with_channel(user_prompt, cfg.tx.clone()).await?;

    let transcript = summarize_conversation(&conversation);

    let phase_cost = {
        let tracker = agent.cost_tracker.lock().expect("cost_tracker lock");
        tracker.total_cost_usd
    };
    state.total_cost_usd += phase_cost;

    match phase {
        CodingPhase::Assess => {
            // Pull out the `## Goal Summary` section so later phases
            // can quote it verbatim without lugging the whole ~500
            // word assessment along.
            state.goal_summary = extract_section(&transcript, "Goal Summary");
            state.assessment = Some(transcript);
        }
        CodingPhase::Plan => state.plan = Some(transcript),
        CodingPhase::Execute => state.last_exec_summary = Some(transcript),
        CodingPhase::Verify => {
            state.verify_passed = detect_verify_pass(&transcript);
            state.last_verify_output = Some(transcript);
        }
        CodingPhase::Review => {
            // REVIEW may update the goal summary if it noticed
            // we've been solving the wrong problem. The review
            // prompt asks for a `## Updated Goal Summary` block
            // only when the original needs refinement; when absent
            // we keep ASSESS's version.
            if let Some(refined) = extract_section(&transcript, "Updated Goal Summary") {
                state.goal_summary = Some(refined);
            }
            state.last_review = Some(transcript);
        }
        CodingPhase::Finalize => {}
    }

    Ok(())
}

/// Pull out the contents of a `## <heading>` section from a markdown
/// blob produced by one of the workflow phases. Returns `None` when
/// the heading isn't present, so callers can distinguish "didn't
/// emit" from "emitted empty".
///
/// Matches headings case-insensitively so minor capitalization drift
/// in the LLM output doesn't drop the section on the floor.
fn extract_section(markdown: &str, heading: &str) -> Option<String> {
    let needle = format!("## {heading}").to_lowercase();
    let lower = markdown.to_lowercase();
    let start_line = lower.find(&needle)?;
    // Advance to the line after the heading.
    let after_heading = markdown[start_line..].find('\n').map_or(markdown.len(), |i| start_line + i + 1);
    // Find the next `## ` heading (any level-2), which terminates
    // the section. If there isn't one, take everything to EOF.
    let rest = &markdown[after_heading..];
    let end_relative = rest.find("\n## ").or_else(|| rest.find("\n##")).unwrap_or(rest.len());
    let body = rest[..end_relative].trim();
    if body.is_empty() {
        None
    } else {
        Some(body.to_string())
    }
}

/// Heuristic: does the verifier's last message claim tests passed?
/// The verify phase's system prompt asks the agent to say either
/// `ALL TESTS PASS` or `TESTS FAILED:` with the output — so we look
/// for the pass marker. Falls back to looking for standard runner
/// summaries if the agent ignored the format request.
fn detect_verify_pass(transcript: &str) -> bool {
    let upper = transcript.to_uppercase();
    if upper.contains("ALL TESTS PASS") {
        return true;
    }
    if upper.contains("TESTS FAILED") || upper.contains("TESTS FAIL") {
        return false;
    }
    // Fallback: common runner summary shapes that indicate success.
    upper.contains("TEST RESULT: OK") || upper.contains("OK (") || upper.contains("PASSING") && !upper.contains("FAILING")
}

fn summarize_conversation(conv: &crate::conversation::Conversation) -> String {
    // The assistant's LAST message is what this phase produced. For
    // execute/verify that message summarises what was done; for
    // assess/plan/review it IS the output (reasoning text).
    conv.messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, crate::conversation::Role::Assistant))
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// System + user prompts per phase. Kept as free functions so tests
// can snapshot them; no LLM calls happen here.
// ---------------------------------------------------------------------------

fn assess_prompt(_state: &WorkflowState, task: &str) -> (String, String) {
    let sys = "\
You are in the ASSESS phase of a structured coding workflow.

You are the voice of the user's intent across every later phase. \
After you, the code gets written, tests get run, bugs get \
critiqued. Every one of those phases will see your output at the \
top of their prompt. Get it right; everything downstream depends \
on it.

Read the task, the test file, the stub code, and any \
INSTRUCTIONS.md (use read_file liberally — do not guess).

Then produce your response in EXACTLY this format:

    ## Goal Summary

    (2–4 short sentences. What is the user trying to achieve. What \
    does success look like. The fewest words that still preserve \
    the real objective. This is what every later phase will see.)

    ## Context

    (Anything relevant about the problem shape that the Goal \
    Summary omits — test framework, language, conventions the \
    surrounding code uses.)

    ## Test Cases

    (Enumerate the concrete cases the test file exercises, one per \
    line. If there are ignored/skipped markers, note they've been \
    stripped already.)

    ## Constraints & Gotchas

    (Hidden behaviors the tests reveal — specific error types \
    expected, ordering, edge-case values. One bullet each.)

    ## Open Judgement Calls

    (Anything truly ambiguous where you're making a choice. Skip \
    if nothing is ambiguous.)

Keep the whole thing under ~500 words. Do NOT start writing code. \
Do NOT edit any files. That's EXECUTE's job.";

    let user = format!(
        "Task:\n\n{task}\n\nProduce the assessment in the required format. Every later phase will quote your Goal Summary verbatim, so make it precise."
    );
    (sys.to_string(), user)
}

fn plan_prompt(state: &WorkflowState, task: &str) -> (String, String) {
    let sys = "\
You are in the PLAN phase of a structured coding workflow.

ASSESS produced a Goal Summary and a detailed assessment (both \
included below). Your job: turn them into a concrete \
implementation plan.

Output:
1. An ordered list of steps the EXECUTE phase should take.
2. The key data structures / types / functions that need to exist.
3. Any edge cases that will need special handling.
4. A short strategy for verifying incrementally (which cases to \
   get green first).

Keep it tight (~300 words). No code — just the plan. Do not edit \
files.";

    let goal = state.goal_summary.as_deref().unwrap_or("(no goal summary)");
    let assessment = state.assessment.as_deref().unwrap_or("(no assessment available)");
    let user = format!("Task:\n\n{task}\n\n## Goal Summary\n\n{goal}\n\n## Full Assessment\n\n{assessment}\n\nProduce the implementation plan.");
    (sys.to_string(), user)
}

fn execute_prompt(state: &WorkflowState, task: &str) -> (String, String) {
    let sys = "\
You are in the EXECUTE phase of a structured coding workflow.

Your job: implement the solution. You have read_file, write_file, \
edit_file, and bash tools available.

Rules:
- Follow the implementation plan from the PLAN phase (included below).
- If a prior REVIEW phase left critique (included below), address \
  EVERY point it raised before declaring done.
- Do NOT modify test files. The tests are the spec.
- You do NOT need to run the tests here — the VERIFY phase will. \
  But you may run a quick syntax check (e.g. `cargo check`, \
  `python -m py_compile`) if it helps you catch errors early.
- When your implementation is in place, stop and respond with a \
  one-paragraph summary of what you changed.";

    let goal = state.goal_summary.as_deref().unwrap_or("(no goal summary)");
    let plan = state.plan.as_deref().unwrap_or("(no plan available)");
    let review = state.last_review.as_deref().unwrap_or("(first iteration — no prior review)");
    let user = format!(
        "Task:\n\n{task}\n\n## Goal Summary (keep this in mind while coding)\n\n{goal}\n\n## Implementation plan\n\n{plan}\n\n## Review findings to address\n\n{review}\n\nImplement the solution. When done, summarize what you changed in one paragraph."
    );
    (sys.to_string(), user)
}

fn verify_prompt(state: &WorkflowState, _task: &str) -> (String, String) {
    let sys = "\
You are in the VERIFY phase of a structured coding workflow.

Your ONLY job: run the test command in the working directory and \
report the results.

1. Use the bash tool to run the task's test command.
2. Observe the output.
3. Respond with EXACTLY ONE of these two prefixes as your final message:
   - `ALL TESTS PASS` followed by a one-line summary (N passed).
   - `TESTS FAILED:` followed by the test runner's failure output \
     (trim to the most actionable ~30 lines — the REVIEW phase will \
     use this to figure out what to fix).

Do NOT edit files. Do NOT try to fix anything in this phase. Just \
run tests and report.";

    let goal = state.goal_summary.as_deref().unwrap_or("(no goal summary)");
    let exec = state.last_exec_summary.as_deref().unwrap_or("(no execute summary)");
    let user = format!(
        "## Goal (what the tests are supposed to confirm)\n\n{goal}\n\n## EXECUTE just made these changes\n\n{exec}\n\nRun the tests now. Use the language's standard test runner (pytest, cargo test, go test, jest, etc.) — find the right command by inspecting the task's configuration (package.json scripts, Cargo.toml, etc.). Then respond with the prefix-formatted result."
    );
    (sys.to_string(), user)
}

fn review_prompt(state: &WorkflowState, task: &str) -> (String, String) {
    let sys = "\
You are in the REVIEW phase of a structured coding workflow.

Tests just failed. Your job is adversarial critique: figure out \
WHY they failed and tell the EXECUTE phase exactly what to change \
on its next turn.

Format your output:
1. Root-cause analysis: 2–4 bullet points identifying what's wrong.
2. Specific fixes: numbered list of concrete changes to make (file \
   names + what to change).
3. Edge cases the current code is missing (if the failures suggest \
   one).

If — and ONLY if — the failures suggest we've misunderstood the \
task (not a bug but a wrong model of the problem), add a section:

    ## Updated Goal Summary

    (2–4 sentences restating the goal with the corrected \
    understanding. This REPLACES the ASSESS version for subsequent \
    phases. Omit this section entirely when the original goal is \
    still right and we just need to fix code bugs.)

Keep it direct and specific. Don't re-state what the task is — \
EXECUTE already knows. Don't write the code yourself; just tell \
EXECUTE what to do.

You have read-only file tools if you need to inspect the current \
code or the tests.";

    let goal = state.goal_summary.as_deref().unwrap_or("(no goal summary)");
    let verify = state.last_verify_output.as_deref().unwrap_or("(no verify output)");
    let user = format!("Task:\n\n{task}\n\n## Current Goal Summary\n\n{goal}\n\n## Test failure output from VERIFY\n\n{verify}\n\nProduce the critique.");
    (sys.to_string(), user)
}

fn finalize_prompt(state: &WorkflowState, task: &str) -> (String, String) {
    let sys = "\
You are in the FINALIZE phase of a structured coding workflow.

The tests have passed (or iterations are exhausted). Go back to \
first principles: does the final state of the code actually \
achieve the Goal Summary from ASSESS? Check against the goal, not \
just the tests — tests can pass on code that misses the user's \
real intent.

Produce a short holistic review:

1. **Goal Check** — one sentence: did we achieve the Goal Summary?
2. **Verdict** — SOLVED / PARTIAL / FAILED + why.
3. **Gaps** — edge cases worth adding tests for (even though \
   current tests pass), or aspects of the goal the tests don't \
   actually verify.
4. **Handoff notes** — anything the next developer picking this up \
   should know.

Keep it under 200 words. No tool calls needed — this is a pure \
summary.";

    let goal = state.goal_summary.as_deref().unwrap_or("(no goal summary)");
    let exec = state.last_exec_summary.as_deref().unwrap_or("");
    let verify = state.last_verify_output.as_deref().unwrap_or("");
    let user = format!(
        "Task:\n\n{task}\n\n## Goal Summary (the anchor)\n\n{goal}\n\n## Final execute summary\n\n{exec}\n\n## Final verify output\n\n{verify}\n\nProduce the finalization note."
    );
    (sys.to_string(), user)
}

// Ergonomic `activity_name` shim for error messages — the enum's
// Debug impl uses struct-variant formatting which reads ugly in
// `with_context`.
impl CodingPhase {
    fn activity_name(self) -> &'static str {
        match self.activity() {
            Activity::Thinking => "smooth-thinking",
            Activity::Planning => "smooth-planning",
            Activity::Coding => "smooth-coding",
            Activity::Reviewing => "smooth-reviewing",
            Activity::Judge => "smooth-judge",
            Activity::Summarize => "smooth-summarize",
            Activity::Fast => "smooth-fast",
        }
    }
}

// Kept here so the caller doesn't need to reach into `crate::llm`.
#[allow(dead_code)]
fn llm_client_for(registry: &ProviderRegistry, activity: Activity) -> anyhow::Result<LlmClient> {
    let config = registry.llm_config_for(activity).map_err(|e| anyhow!("resolve {activity:?} slot: {e}"))?;
    Ok(LlmClient::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_phase_has_a_distinct_activity_mapping() {
        use std::collections::HashMap;
        let mut by_activity: HashMap<Activity, Vec<CodingPhase>> = HashMap::new();
        for p in [
            CodingPhase::Assess,
            CodingPhase::Plan,
            CodingPhase::Execute,
            CodingPhase::Verify,
            CodingPhase::Review,
            CodingPhase::Finalize,
        ] {
            by_activity.entry(p.activity()).or_default().push(p);
        }

        // Thinking handles Assess + Finalize (deep reading + last-look).
        assert_eq!(by_activity.get(&Activity::Thinking).map(Vec::len), Some(2));
        // Coding handles Execute + Verify (both edit/run tool users).
        assert_eq!(by_activity.get(&Activity::Coding).map(Vec::len), Some(2));
        // Planning & Reviewing each own a single phase.
        assert_eq!(by_activity.get(&Activity::Planning).map(Vec::len), Some(1));
        assert_eq!(by_activity.get(&Activity::Reviewing).map(Vec::len), Some(1));
        // Judge / Summarize / Fast are NOT used by the workflow —
        // they belong to the bench harness, compaction path, and
        // session-naming callers respectively.
        assert!(!by_activity.contains_key(&Activity::Judge));
        assert!(!by_activity.contains_key(&Activity::Summarize));
        assert!(!by_activity.contains_key(&Activity::Fast));
    }

    #[test]
    fn phase_labels_are_stable_uppercase() {
        assert_eq!(CodingPhase::Assess.label(), "ASSESS");
        assert_eq!(CodingPhase::Plan.label(), "PLAN");
        assert_eq!(CodingPhase::Execute.label(), "EXECUTE");
        assert_eq!(CodingPhase::Verify.label(), "VERIFY");
        assert_eq!(CodingPhase::Review.label(), "REVIEW");
        assert_eq!(CodingPhase::Finalize.label(), "FINALIZE");
    }

    #[test]
    fn activity_name_is_the_smooth_alias() {
        assert_eq!(CodingPhase::Assess.activity_name(), "smooth-thinking");
        assert_eq!(CodingPhase::Plan.activity_name(), "smooth-planning");
        assert_eq!(CodingPhase::Execute.activity_name(), "smooth-coding");
        assert_eq!(CodingPhase::Verify.activity_name(), "smooth-coding");
        assert_eq!(CodingPhase::Review.activity_name(), "smooth-reviewing");
        assert_eq!(CodingPhase::Finalize.activity_name(), "smooth-thinking");
    }

    #[test]
    fn detect_verify_pass_matches_explicit_marker() {
        assert!(detect_verify_pass("ALL TESTS PASS — 31 of 31."));
        assert!(detect_verify_pass("all tests pass. great."));
        assert!(!detect_verify_pass("TESTS FAILED:\n  case one: expected 3 got 2"));
        assert!(!detect_verify_pass("tests fail in two cases"));
    }

    #[test]
    fn detect_verify_pass_falls_back_to_runner_summary() {
        assert!(detect_verify_pass("test result: ok. 31 passed; 0 failed;"));
        assert!(!detect_verify_pass("no signal at all"));
    }

    #[test]
    fn assess_prompt_mentions_all_key_sections() {
        let state = WorkflowState::default();
        let (sys, user) = assess_prompt(&state, "solve bowling");
        assert!(sys.contains("ASSESS"));
        // Structured output sections the prompt demands
        assert!(sys.contains("## Goal Summary"));
        assert!(sys.contains("## Context"));
        assert!(sys.contains("## Test Cases"));
        assert!(sys.contains("Do NOT start writing code"));
        assert!(user.contains("solve bowling"));
    }

    #[test]
    fn execute_prompt_includes_plan_and_review_findings() {
        let mut state = WorkflowState::default();
        state.plan = Some("step 1: write score()".into());
        state.last_review = Some("off-by-one in strike bonus".into());
        let (sys, user) = execute_prompt(&state, "solve bowling");
        assert!(sys.contains("EXECUTE"));
        assert!(sys.contains("edit_file"));
        assert!(user.contains("step 1: write score()"));
        assert!(user.contains("off-by-one in strike bonus"));
    }

    #[test]
    fn extract_section_pulls_out_goal_summary() {
        let doc = "## Goal Summary\n\nImplement bowling score with strikes/spares.\n\n## Context\n\nRust stub.\n";
        assert_eq!(
            extract_section(doc, "Goal Summary").as_deref(),
            Some("Implement bowling score with strikes/spares.")
        );
    }

    #[test]
    fn extract_section_is_case_insensitive() {
        let doc = "## GOAL SUMMARY\n\nBody text.\n";
        assert_eq!(extract_section(doc, "Goal Summary").as_deref(), Some("Body text."));
    }

    #[test]
    fn extract_section_returns_none_when_missing() {
        let doc = "## Context\n\nNo goal here.\n";
        assert_eq!(extract_section(doc, "Goal Summary"), None);
    }

    #[test]
    fn extract_section_terminates_at_next_heading() {
        let doc = "## Goal Summary\n\nGoal body.\n\n## Context\n\nContext body.\n\n## Test Cases\n\nOne per line.\n";
        assert_eq!(extract_section(doc, "Goal Summary").as_deref(), Some("Goal body."));
        assert_eq!(extract_section(doc, "Context").as_deref(), Some("Context body."));
    }

    #[test]
    fn extract_section_with_empty_body_returns_none() {
        let doc = "## Goal Summary\n\n## Next Section\n";
        assert_eq!(extract_section(doc, "Goal Summary"), None);
    }

    #[test]
    fn review_can_refine_goal_summary() {
        // If the review includes an "Updated Goal Summary" block,
        // state update should swap it in. Tested via the extractor
        // because the state mutation in run_phase is only reachable
        // with a live LLM.
        let review = "## Root cause\n\nOff by one.\n\n## Updated Goal Summary\n\nImplement bowling score; rolls can exceed 10 on bonus rolls.\n";
        let refined = extract_section(review, "Updated Goal Summary");
        assert_eq!(refined.as_deref(), Some("Implement bowling score; rolls can exceed 10 on bonus rolls."));
    }

    #[test]
    fn verify_prompt_demands_prefix_format() {
        let state = WorkflowState::default();
        let (sys, _user) = verify_prompt(&state, "solve bowling");
        assert!(sys.contains("ALL TESTS PASS"));
        assert!(sys.contains("TESTS FAILED"));
        assert!(sys.contains("EXACTLY ONE"));
    }
}

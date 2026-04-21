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
//! | TEST     | Reviewing       | Add real test coverage (MSW, Playwright, property…) |
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
    /// Adversarial test augmentation. Runs AFTER the provided tests
    /// pass. Classifies the code, picks the right testing stack for
    /// its shape (MSW, Playwright, testcontainers, property-based,
    /// …), installs missing deps, and writes boundary-pushing tests.
    /// Fails back into EXECUTE if the new tests expose real bugs.
    ///
    /// Skippable via `SMOOTH_WORKFLOW_SKIP_TEST=1` for benchmark
    /// runs where adding extra tests would change the score.
    Test,
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
            Self::Test => "TEST",
            Self::Finalize => "FINALIZE",
        }
    }

    /// Which routing slot this phase dispatches through.
    ///
    /// `VERIFY` and `EXECUTE` share the Coding slot — verify is just
    /// the agent running bash tests, same surface as writing code.
    /// `FINALIZE` uses Thinking so the last-look review has room to
    /// reason about the whole diff holistically. `TEST` uses
    /// Reviewing — adversarial test writing is closer to code
    /// review than to fresh implementation.
    pub fn activity(self) -> Activity {
        match self {
            Self::Assess | Self::Finalize => Activity::Thinking,
            Self::Plan => Activity::Planning,
            Self::Execute | Self::Verify => Activity::Coding,
            Self::Review | Self::Test => Activity::Reviewing,
        }
    }

    /// Max iterations for the Agent instance inside this phase.
    /// Non-iterative phases (assess/plan/review/finalize) cap short;
    /// execute + verify + test get the full budget because they all
    /// touch the filesystem and need room to iterate.
    fn max_iterations(self) -> u32 {
        match self {
            Self::Assess | Self::Plan | Self::Review | Self::Finalize => 8,
            Self::Execute => 40,
            Self::Test => 20,
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
    /// count). 10 is a ceiling — budget + plateau detection do the
    /// real governing.
    pub max_outer_iterations: u32,
    /// Skip the adversarial TEST phase (useful for benchmark runs
    /// where adding deps or extra tests would change the score).
    /// Defaults to false — real coding tasks want the TEST phase.
    pub skip_test_phase: bool,
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
    /// Rolling progress log — what's actually been accomplished so
    /// far. Updated after EXECUTE / VERIFY / REVIEW / TEST each
    /// emit a `## Progress Update` one-liner. Threaded into every
    /// subsequent phase's user prompt alongside `goal_summary` so
    /// the agent always knows both where it's going (goal) AND
    /// how far it's got (progress). FINALIZE uses it as the
    /// authoritative record for its verdict.
    progress_summary: String,
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
    /// Adversarial-test findings from the TEST phase — new tests
    /// written + whether they all passed. When red, these feed back
    /// into EXECUTE's review-findings slot on the next loop.
    last_test_report: Option<String>,
    /// TEST phase's verdict: did its added tests all pass?
    /// `true` lets the workflow move to FINALIZE; `false` loops
    /// back to EXECUTE with the new failures as review findings.
    test_phase_passed: bool,
    /// How many write-shape (`edit_file` / `write_file`) tool calls
    /// the most recent EXECUTE phase made. Zero means the agent
    /// spent the whole turn exploring — real failure mode in our
    /// Java bench run (29 `list_files`, 0 writes, stub code left
    /// intact → 0/31). We detect this between EXECUTE and VERIFY
    /// and force a retry with an override review-finding instead
    /// of wasting a VERIFY cycle on unchanged code.
    last_execute_write_count: usize,
    /// How many `bash` tool calls the most recent EXECUTE phase
    /// made. Zero is the "shipped without self-validation" failure
    /// mode — the agent edited a file but never ran the test suite
    /// or a compile check, and the code turns out to have an
    /// unclosed delimiter (Rust 0/1 bench run: 6 edit_file, 1 bash
    /// total including VERIFY's run = zero bash in EXECUTE). A
    /// single `cargo check` / `node --check` would have caught it.
    /// We treat zero-bash on a non-trivial EXECUTE the same way we
    /// treat zero-writes: force a retry with an override finding.
    last_execute_bash_count: usize,
    /// Override string prepended to the next EXECUTE's "Review
    /// findings" context when the previous turn was a no-op. Empty
    /// when the normal REVIEW phase output is authoritative. Lets
    /// us keep the REVIEW prompt untouched while still threading a
    /// blunt "you wrote no code last turn" warning when needed.
    execute_force_write_note: Option<String>,
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
    //
    // Stop conditions, in order of priority:
    //   1. verify_passed → done, break to FINALIZE
    //   2. budget would be exhausted before next cycle → break
    //   3. plateau: three consecutive identical verify signatures → break
    //   4. hard cap max_outer_iterations → break
    //
    // The cap is a safety net, not the primary governor. Budget +
    // plateau detection do most of the work. Plateau is the
    // trickiest knob: too eager and we give up on tasks where the
    // next REVIEW→EXECUTE would have closed the gap (bench runs
    // routinely stop at 28/31 when 31/31 was one more cycle away);
    // too lax and the loop burns budget on a stuck model. Three
    // identical signatures in a row is the sweet spot — one repeat
    // can be an unlucky re-fix that happened to yield the same
    // failure count with different specifics; three says the model
    // really is going in circles.
    let mut last_verify_signature: Option<String> = None;
    let mut plateau_strikes = 0u32;
    for _ in 0..cfg.max_outer_iterations {
        state.outer_iteration += 1;

        run_phase(&cfg, CodingPhase::Execute, &mut state, execute_prompt).await?;

        // EXECUTE degenerate-case recovery. Two distinct failure
        // modes we've observed and auto-fix before wasting a VERIFY
        // cycle on code that can't possibly be right:
        //
        //   A. Zero writes (Java bowling 0/31). Agent explored —
        //      29 list_files, 0 edit_file — and the stub sits
        //      intact. VERIFY would fail on unchanged code.
        //   B. Writes but zero bash (Rust bowling 0/1). Agent
        //      edited 6 times but never ran `cargo check`; shipped
        //      code with an unclosed delimiter. A single
        //      syntax-check would have caught it. Self-validation
        //      is the one thing the EXECUTE prompt calls non-
        //      optional, and skipping it is a 0/N bench result.
        //
        // Both cases fix the same way: inject an override finding
        // into the next EXECUTE's review-block and retry. Cap at
        // 2 retries so a genuinely stuck model can't burn the
        // whole budget here.
        let mut exec_retries = 0u32;
        loop {
            let no_writes = state.last_execute_write_count == 0;
            let no_bash = state.last_execute_bash_count == 0 && state.last_execute_write_count > 0;
            if !no_writes && !no_bash {
                break;
            }
            if exec_retries >= 2 {
                break;
            }
            exec_retries += 1;
            let (reason, note) = if no_writes {
                (
                    "no writes",
                    format!(
                        "YOUR PRIOR TURN WROTE NOTHING. You called zero edit_file / \
                         write_file tools in your last EXECUTE turn — the implementation \
                         file is still the starter stub. That is a broken outcome: the \
                         workflow cannot make progress without a write. This turn you \
                         MUST call edit_file or write_file at least once with real \
                         implementation code. Do not burn this turn on list_files / \
                         read_file exploration; you have already surveyed the repo. \
                         Open the implementation file and write code.\n\
                         (Retry {exec_retries} of 2 after a no-op EXECUTE.)"
                    ),
                )
            } else {
                (
                    "no bash",
                    format!(
                        "YOU EDITED FILES BUT NEVER RAN THE TESTS. Self-validation is \
                         the single non-optional rule of this phase and you skipped it. \
                         Your last turn made {writes} write call(s) but ZERO bash calls, \
                         which means you cannot know whether the code compiles — let \
                         alone whether it passes the suite. Most 0/N bench failures \
                         look exactly like this: the agent edits, declares victory, \
                         ships code with an unclosed delimiter. This turn you MUST run \
                         the test command via `bash` BEFORE your summary. Fix anything \
                         that fails. Then report the literal pass/fail count.\n\
                         (Retry {exec_retries} of 2 after an EXECUTE with no \
                         self-validation.)",
                        writes = state.last_execute_write_count
                    ),
                )
            };
            tracing::warn!(outer_iter = state.outer_iteration, exec_retries, reason, "EXECUTE degenerate-case retry");
            state.execute_force_write_note = Some(note);
            append_progress(
                &mut state.progress_summary,
                "EXECUTE",
                state.outer_iteration,
                &format!("{reason} — retry #{exec_retries}"),
            );
            run_phase(&cfg, CodingPhase::Execute, &mut state, execute_prompt).await?;
        }
        state.execute_force_write_note = None;

        run_phase(&cfg, CodingPhase::Verify, &mut state, verify_prompt).await?;

        if state.verify_passed {
            break;
        }

        let sig = verify_signature(state.last_verify_output.as_deref().unwrap_or(""));
        if Some(&sig) == last_verify_signature.as_ref() {
            plateau_strikes += 1;
            if plateau_strikes >= 2 {
                tracing::info!(
                    strikes = plateau_strikes,
                    signature = %sig,
                    "coding workflow: plateau detected (three identical verify signatures in a row), stopping early"
                );
                break;
            }
        } else {
            plateau_strikes = 0;
        }
        last_verify_signature = Some(sig);

        // Budget: break if the next iteration would likely blow the
        // cap. Rough heuristic — next iter costs roughly as much as
        // this iter did. Avoid the pathological case where the loop
        // drains the whole cap on retry spam.
        if let Some(cap) = cfg.budget_usd {
            if cap > 0.0 && state.outer_iteration > 0 {
                let per_iter = state.total_cost_usd / f64::from(state.outer_iteration);
                let projected = state.total_cost_usd + per_iter;
                if projected >= cap {
                    tracing::info!(
                        spent = state.total_cost_usd,
                        cap = cap,
                        projected = projected,
                        "coding workflow: budget would be exhausted next cycle, stopping early"
                    );
                    break;
                }
            }
        }

        // Compile-error short-circuit. When VERIFY's output is a
        // syntax / parse / compile error, REVIEW doesn't add much —
        // the fix is mechanical (close the delimiter, fix the
        // import, etc.) and a full REVIEW round spends an LLM call
        // on prose the model already knows how to produce.
        // Instead, inject a targeted review-finding directly and
        // loop straight back to EXECUTE. Saves one LLM call per
        // cycle and gives EXECUTE a blunt "fix the syntax first"
        // instruction it can act on.
        //
        // The JS bowling bench pattern we keep hitting: agent
        // appends a second class body after the class closure, jest
        // parse-errors, VERIFY reports "SyntaxError: Missing
        // semicolon", the loop was doing REVIEW → EXECUTE but
        // EXECUTE wasn't pinning on the syntax fix. A direct
        // override is blunter and cheaper.
        if let Some(compile_err) = detect_compile_error(state.last_verify_output.as_deref().unwrap_or("")) {
            tracing::info!(
                outer_iter = state.outer_iteration,
                "coding workflow: VERIFY reported a compile/parse error — skipping REVIEW, forcing EXECUTE to fix syntax first"
            );
            state.last_review = Some(format!(
                "## Root cause\n\n\
                 Your last EXECUTE turn shipped code that does not compile / parse. \
                 This is the single most common failure mode at this stage of the \
                 workflow: writing plausible-looking code and declaring done without \
                 running the test suite to confirm it parses. Fix the syntax before \
                 anything else — no logic work, no refactors, no new features.\n\n\
                 ## Compile error from VERIFY\n\n\
                 {compile_err}\n\n\
                 ## Specific fixes\n\n\
                 1. Open the file the error points at. Read it end-to-end.\n\
                 2. Find the exact line / column the error names. If it's a \"missing \
                    semicolon\", \"unclosed delimiter\", \"unexpected token\", or \
                    \"unexpected EOF\", you almost certainly have a class or function \
                    body that was duplicated, truncated, or had content pasted after \
                    its closing brace.\n\
                 3. Delete anything that appears after a module-level export / \
                    public-API closing brace (e.g. anything after `module.exports = \
                    Bowling;` in JS, anything after the final `}}` that closes an \
                    `impl` block in Rust).\n\
                 4. Run the test command via `bash` to confirm the syntax is valid \
                    before declaring done.\n\n\
                 Do NOT produce a new implementation this turn. Just repair the \
                 syntax of the file you already have."
            ));
            append_progress(
                &mut state.progress_summary,
                "REVIEW",
                state.outer_iteration,
                "compile error — auto-override to fix syntax",
            );
            continue;
        }

        run_phase(&cfg, CodingPhase::Review, &mut state, review_prompt).await?;
    }

    // TEST --------------------------------------------------------------
    // Only run when the core verify passed AND the caller didn't
    // opt out. If TEST adds tests that reveal real bugs, loop back
    // to EXECUTE with those as the next REVIEW findings — bounded
    // separately so a TEST-induced problem can't eat the whole
    // iteration cap on its own.
    if state.verify_passed && !cfg.skip_test_phase {
        let mut test_retry_strikes = 0u32;
        let test_retry_max = 3u32;
        loop {
            run_phase(&cfg, CodingPhase::Test, &mut state, test_prompt).await?;
            if state.test_phase_passed {
                break;
            }
            test_retry_strikes += 1;
            if test_retry_strikes >= test_retry_max {
                tracing::info!("coding workflow: TEST phase red after {test_retry_max} tries, finalizing with the gap noted");
                break;
            }
            // Budget check for the retry too.
            if let Some(cap) = cfg.budget_usd {
                if cap > 0.0 && state.total_cost_usd >= cap {
                    break;
                }
            }
            // Feed the TEST report into review findings so EXECUTE
            // sees what broke, then run one more EXECUTE → VERIFY
            // cycle to try to fix it.
            state.last_review = state.last_test_report.clone();
            state.outer_iteration += 1;
            run_phase(&cfg, CodingPhase::Execute, &mut state, execute_prompt).await?;
            run_phase(&cfg, CodingPhase::Verify, &mut state, verify_prompt).await?;
            if !state.verify_passed {
                // EXECUTE broke the original tests — bail, don't
                // keep drilling. FINALIZE will report the regression.
                break;
            }
        }
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
            append_progress(&mut state.progress_summary, "ASSESS", 0, "Read tests + stub + instructions; crystallized goal.");
        }
        CodingPhase::Plan => {
            state.plan = Some(transcript);
            append_progress(&mut state.progress_summary, "PLAN", 0, "Produced an implementation plan.");
        }
        CodingPhase::Execute => {
            state.last_execute_write_count = count_write_tool_calls(&conversation);
            state.last_execute_bash_count = count_tool_calls_named(&conversation, "bash");
            let update = extract_section(&transcript, "Progress Update").unwrap_or_else(|| first_line(&transcript, 160));
            append_progress(&mut state.progress_summary, "EXECUTE", state.outer_iteration, &update);
            state.last_exec_summary = Some(transcript);
        }
        CodingPhase::Verify => {
            state.verify_passed = detect_verify_pass(&transcript);
            let counts = verify_signature(&transcript);
            let status = if state.verify_passed { "tests PASS" } else { "tests fail" };
            append_progress(&mut state.progress_summary, "VERIFY", state.outer_iteration, &format!("{status} ({counts})"));
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
                append_progress(&mut state.progress_summary, "REVIEW", state.outer_iteration, "Refined goal summary.");
            } else {
                append_progress(
                    &mut state.progress_summary,
                    "REVIEW",
                    state.outer_iteration,
                    "Critiqued failures; queued fixes.",
                );
            }
            state.last_review = Some(transcript);
        }
        CodingPhase::Test => {
            // The TEST-phase prompt asks for a final `## Verdict`
            // line: `PASS` when every new test the agent added is
            // green, `FAIL` otherwise. Fall back to looking for the
            // verify-style marker if the agent ignored the format.
            let upper = transcript.to_uppercase();
            state.test_phase_passed = upper.contains("## VERDICT")
                && upper
                    .split("## VERDICT")
                    .nth(1)
                    .is_some_and(|tail| tail.lines().take(3).any(|l| l.trim_start().starts_with("PASS")))
                || (detect_verify_pass(&transcript) && !upper.contains("FAIL"));
            let status = if state.test_phase_passed { "new tests PASS" } else { "new tests fail" };
            append_progress(&mut state.progress_summary, "TEST", state.outer_iteration, status);
            state.last_test_report = Some(transcript);
        }
        CodingPhase::Finalize => {
            append_progress(&mut state.progress_summary, "FINALIZE", state.outer_iteration, "Session complete.");
        }
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
/// Append one line to the rolling progress log with a
/// phase/iteration tag. Iteration 0 means "not inside the outer
/// execute-verify-review loop" (ASSESS / PLAN / FINALIZE).
fn append_progress(buf: &mut String, phase: &str, iteration: u32, update: &str) {
    let update = update.trim();
    if update.is_empty() {
        return;
    }
    if iteration > 0 {
        buf.push_str(&format!("- [{phase} #{iteration}] {update}\n"));
    } else {
        buf.push_str(&format!("- [{phase}] {update}\n"));
    }
}

/// Trim a phase transcript to the first useful line — used as a
/// fallback progress update when the phase didn't emit a
/// `## Progress Update` section.
/// Format the rolling progress log for inclusion in a prompt.
/// Returns `"(starting fresh — no prior phases)"` when the log is
/// empty so the section never shows up blank.
fn progress_block(buf: &str) -> String {
    if buf.trim().is_empty() {
        "(starting fresh — no prior phases)".to_string()
    } else {
        buf.trim_end().to_string()
    }
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > max {
        line.chars().take(max).collect::<String>() + "…"
    } else {
        line.to_string()
    }
}

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

/// Distill a verifier transcript down to "how many passed vs
/// failed" so we can detect a plateau: if the numbers are the
/// same two cycles in a row, REVIEW isn't giving the agent new
/// information and we're wasting budget. Case-insensitive.
///
/// Returns a compact string like `"27p/4f"` built from the first
/// `N passed … N failed` pattern it finds. Falls back to the whole
/// trimmed message when no counts are visible — better to be
/// conservative (treat as plateau after one stable pass) than risk
/// false-positive.
pub fn verify_signature(transcript: &str) -> String {
    let lower = transcript.to_lowercase();
    // Pull the first passed/failed pair we find.
    let passed = scan_count(&lower, "passed");
    let failed = scan_count(&lower, "failed");
    if passed.is_some() || failed.is_some() {
        return format!("{}p/{}f", passed.unwrap_or(0), failed.unwrap_or(0));
    }
    // Build error vs test failure: track the first line mentioning
    // "error" so two identical compile errors count as a plateau.
    for line in lower.lines() {
        if line.contains("error") {
            return line.trim().to_string();
        }
    }
    lower.trim().chars().take(80).collect()
}

fn scan_count(haystack: &str, needle: &str) -> Option<u32> {
    // Walk through the string looking for "<number> <needle>".
    let mut chars = haystack.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !c.is_ascii_digit() {
            continue;
        }
        let start = i;
        let mut end = i + c.len_utf8();
        while let Some(&(j, ch)) = chars.peek() {
            if ch.is_ascii_digit() {
                end = j + ch.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        let num = &haystack[start..end];
        let rest = &haystack[end..].trim_start();
        if rest.starts_with(needle) {
            return num.parse().ok();
        }
    }
    None
}

/// Heuristic: does the verifier's last message claim tests passed?
/// The verify phase's system prompt asks the agent to say either
/// `ALL TESTS PASS` or `TESTS FAILED:` with the output — so we look
/// for the pass marker. Falls back to looking for standard runner
/// summaries if the agent ignored the format request.
fn detect_verify_pass(transcript: &str) -> bool {
    let upper = transcript.to_uppercase();
    // Explicit prefix from the VERIFY phase prompt contract.
    if upper.contains("ALL TESTS PASS") {
        return true;
    }
    if upper.contains("TESTS FAILED") || upper.contains("TESTS FAIL") {
        return false;
    }
    // Narrow runner-summary fallbacks. Used only when the VERIFY
    // model forgot the explicit prefix. Each positive pattern must
    // uniquely identify a *successful* final summary from a real
    // test runner — fuzzy phrases like "OK (" would false-positive
    // on Rust `Ok(..)` values that appear verbatim in failure diffs
    // ("left: Ok(()), right: Err(NotEnoughPinsLeft)"), and lone
    // "PASSING" would false-positive on prose like "most suites
    // are passing". A false positive here silently short-circuits
    // the EXECUTE → VERIFY → REVIEW loop on the very first cycle,
    // which is exactly the bug we're avoiding.
    //
    // "FAILED" in the transcript does NOT automatically mean
    // failure — "0 failed" appears inside real green summaries like
    // "31 passed; 0 failed". We only short-circuit on failure when
    // we see a *positive* failure count or a cargo-style
    // "test result: FAILED" line.
    if nonzero_failure_count(&upper) || upper.contains("TEST RESULT: FAILED") {
        return false;
    }
    upper.contains("TEST RESULT: OK")                     // cargo test green
        || upper.contains(" PASSED, 0 FAILED")             // pytest / go / jest summary
        || upper.contains("0 FAILED, 0 ERRORS")            // go test verbose
        || (upper.contains("TESTS:") && upper.contains(" PASSED") && upper.contains("0 FAILED"))
    // jest / vitest
}

/// Extract a compile / parse / syntax error from a VERIFY
/// transcript, or `None` when the failure is a normal test
/// assertion. Looking for the patterns each language's toolchain
/// emits when the input doesn't even parse — at that point running
/// REVIEW → EXECUTE with freeform critique wastes an LLM call on
/// advice the model already has. A blunt "fix the syntax first"
/// override closes the loop faster.
///
/// Returns a short extracted snippet (the error line + 1–2 lines
/// of surrounding context) when a pattern matches, suitable for
/// dropping into the EXECUTE-override note verbatim.
fn detect_compile_error(transcript: &str) -> Option<String> {
    // Language-agnostic signatures. Each is unambiguous — a real
    // test assertion won't print these phrases. We match the raw
    // case to preserve line numbers / column numbers in the
    // snippet, but detect case-insensitively.
    let upper = transcript.to_uppercase();
    let patterns = [
        // JavaScript / TypeScript — jest / vitest / babel / swc / node
        "SYNTAXERROR",
        "UNEXPECTED TOKEN",
        "MISSING SEMICOLON",
        "UNCLOSED DELIMITER",
        "UNEXPECTED EOF",
        // Rust — cargo / rustc
        "COULD NOT COMPILE",
        "THIS FILE CONTAINS AN UNCLOSED DELIMITER",
        "EXPECTED ONE OF",
        // Go — gc / go build
        "SYNTAX ERROR:",
        "EXPECTED '{'",
        "EXPECTED ';'",
        // Python — py_compile / pytest collect-time failure
        "INDENTATIONERROR",
        "TABERROR",
        // Java — javac / gradle compile
        "REACHED END OF FILE",
        "';' EXPECTED",
        "CLASS, INTERFACE, OR ENUM EXPECTED",
        "ERROR: COMPILATION FAILED",
    ];
    let hit_idx = patterns.iter().find_map(|p| upper.find(p))?;
    // Grab the error line + a small surrounding window from the
    // ORIGINAL (case-preserving) transcript, not the uppercased copy.
    // Clamp so we don't drag a megabyte of trailing noise.
    let bytes_per_char = transcript.len().checked_div(upper.len()).unwrap_or(1).max(1);
    let start = hit_idx.saturating_mul(bytes_per_char).saturating_sub(120);
    let end = (hit_idx.saturating_mul(bytes_per_char).saturating_add(600)).min(transcript.len());
    let snippet = transcript.get(start..end).unwrap_or(transcript);
    Some(snippet.trim().to_string())
}

/// True when the transcript contains a summary line reporting a
/// POSITIVE number of failures (e.g. "3 failed", "Tests: 2 failed,
/// 28 passed"). Zero-failure counts ("0 failed") don't count — they
/// appear in green summaries. We only need to be approximately
/// right: false negatives here fall through to the positive-shape
/// check, and false positives on a green run are what we just fixed.
fn nonzero_failure_count(upper: &str) -> bool {
    // Walk every occurrence of "FAILED" / "FAILURE" / "FAILING" and
    // look backwards for the nearest digit-run. If that digit-run is
    // nonzero, we've got a real failure count. Cheap and reliable.
    let needles = ["FAILED", "FAILURE", "FAILING"];
    for needle in needles {
        let mut search = upper;
        while let Some(idx) = search.find(needle) {
            let before = &search[..idx];
            // Pull the last digit-run from `before`.
            let digits: String = before
                .chars()
                .rev()
                .skip_while(|c| c.is_whitespace() || matches!(*c, ',' | ';' | '(' | '—' | '-'))
                .take_while(|c| c.is_ascii_digit())
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if let Ok(n) = digits.parse::<u32>() {
                if n > 0 {
                    return true;
                }
            }
            search = &search[idx + needle.len()..];
        }
    }
    false
}

/// Count the number of `edit_file` / `write_file` tool calls across
/// a completed phase's conversation. Zero after an EXECUTE phase
/// means the agent spent the turn on `read_file` / `list_files` /
/// `bash` exploration and never actually touched the code — a
/// failure mode we've observed in the Java bench task (the agent
/// got lost in the Gradle `src/main/java/…` tree and ran the test
/// clock out without writing). Detecting this lets us force a
/// retry with an override finding instead of wasting the VERIFY
/// slot on unchanged code.
fn count_write_tool_calls(conv: &crate::conversation::Conversation) -> usize {
    conv.messages
        .iter()
        .flat_map(|m| m.tool_calls.iter())
        .filter(|tc| matches!(tc.name.as_str(), "edit_file" | "write_file"))
        .count()
}

/// Count tool calls by exact name. Used alongside
/// `count_write_tool_calls` for the "did EXECUTE bother to run
/// tests?" check — zero `bash` calls after a write means the
/// agent shipped without self-validation.
fn count_tool_calls_named(conv: &crate::conversation::Conversation, name: &str) -> usize {
    conv.messages.iter().flat_map(|m| m.tool_calls.iter()).filter(|tc| tc.name == name).count()
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
    let progress = progress_block(&state.progress_summary);
    let user = format!(
        "Task:\n\n{task}\n\n## Goal Summary\n\n{goal}\n\n## Work So Far\n\n{progress}\n\n## Full Assessment\n\n{assessment}\n\nProduce the implementation plan."
    );
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
- Do NOT modify the provided test files — they are the spec.

**Self-validation is NOT optional.** Before you respond with a \
summary, you MUST run the provided test suite via `bash` and see \
how many pass. This is the single most important rule in this \
phase. Do not hand off to VERIFY on \"it should work\" — run the \
tests yourself and iterate until you either pass them or \
genuinely cannot make further progress.

Process:

1. **Pick the test command the repo actually uses.** Inspect the \
   repo first:
    * `package.json` scripts — `pnpm test` / `npm test` / \
      `yarn test` (mirror the lockfile). Often wraps jest / \
      vitest / mocha.
    * `Cargo.toml` — `cargo test` (or `cargo nextest run` when \
      `.config/nextest.toml` is present).
    * `pyproject.toml` / `pytest.ini` / `tox.ini` — `pytest` \
      (with whatever args the config file specifies).
    * `go.mod` — `go test ./...` at the module root.
    * `Makefile` / `justfile` — if there's a `test` target, \
      USE it (`make test`, `just test`).
    * `.github/workflows/` — mirrors CI. If CI runs a specific \
      test command, use that exact one.
   The test files in the workspace are the spec — find what \
   runs them, don't invent a new harness.

2. **Write your implementation.** Follow the plan. Don't add \
   orphan tests that reference unimplemented methods. Extra \
   tests are fine only if the matching implementation ships in \
   the same change.

3. **Run the test command.** Read the output. Count \
   passed / failed.

4. **If any test fails, iterate.** The output tells you exactly \
   which assertion blew up — fix the code and re-run. Keep \
   iterating until either (a) the suite is fully green, or (b) \
   you've hit a failure you cannot diagnose and have exhausted \
   your attempts. Do not stop at 'most tests pass' — run them \
   again after every fix.

5. **When you're done iterating,** your final response MUST \
   include a `## Progress Update` section AND a `## Test \
   Results` line showing the exact pass/fail count you observed \
   on your last run, verbatim from the test runner \
   (\"31 passed, 0 failed\" or \"28 passed, 3 failed — \
   tenth-frame strike bonus index off by 9\"). This is non-\
   negotiable — lying about the count (saying \"all pass\" when \
   they don't) breaks the downstream phases.

**Fallback when no test command can be determined** (rare — the \
repo almost always has one): `cargo check` / `python -m \
py_compile` / `go vet` / `node --check <file>` / `tsc \
--noEmit`. This only validates that the code parses, NOT that \
it's correct. Treat this as a last resort and say so in your \
summary.

**Do NOT modify the provided test files** even if a test looks \
wrong. The tests are the spec. If one seems impossible, note \
it in Progress Update — REVIEW will decide whether the spec \
itself is off.";

    let goal = state.goal_summary.as_deref().unwrap_or("(no goal summary)");
    let plan = state.plan.as_deref().unwrap_or("(no plan available)");
    let review = state.last_review.as_deref().unwrap_or("(first iteration — no prior review)");
    let progress = progress_block(&state.progress_summary);
    // If the prior EXECUTE turn was a no-op (zero writes), prepend
    // a blunt force-write note so this turn starts with the
    // correction instead of more context the agent will paraphrase.
    let review_block = match state.execute_force_write_note.as_deref() {
        Some(note) => format!("{note}\n\n---\n\n{review}"),
        None => review.to_string(),
    };
    let user = format!(
        "Task:\n\n{task}\n\n## Goal Summary (keep this in mind while coding)\n\n{goal}\n\n## Work So Far\n\n{progress}\n\n## Implementation plan\n\n{plan}\n\n## Review findings to address\n\n{review_block}\n\nImplement the solution, RUN THE TEST SUITE, and iterate until green (or you've exhausted your attempts). When you stop, include: (1) a one-paragraph summary of what you changed, (2) a `## Progress Update` section with a single-line description of this iteration's contribution, and (3) a `## Test Results` line with the literal pass/fail count from your final test run (e.g., \"31 passed, 0 failed\" or \"28 passed, 3 failed — unresolved: tenth-frame strike bonus\")."
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
    let progress = progress_block(&state.progress_summary);
    let user = format!(
        "## Goal (what the tests are supposed to confirm)\n\n{goal}\n\n## Work So Far\n\n{progress}\n\n## EXECUTE just made these changes\n\n{exec}\n\nRun the tests now. Use the language's standard test runner (pytest, cargo test, go test, jest, etc.) — find the right command by inspecting the task's configuration (package.json scripts, Cargo.toml, etc.). Then respond with the prefix-formatted result."
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
    let progress = progress_block(&state.progress_summary);
    let user = format!(
        "Task:\n\n{task}\n\n## Current Goal Summary\n\n{goal}\n\n## Work So Far\n\n{progress}\n\n## Test failure output from VERIFY\n\n{verify}\n\nProduce the critique."
    );
    (sys.to_string(), user)
}

fn test_prompt(state: &WorkflowState, task: &str) -> (String, String) {
    let sys = "\
You are in the TEST phase of a structured coding workflow.

The provided tests are green. Your job now is the thing that sets \
this agent apart from every run-tests-until-green coder: establish \
REAL test coverage appropriate to the shape of the code.

Your process:

1. **Survey the repo first.** Before you pick any tooling, read \
   what's already there:
   - `package.json` — scripts, devDependencies (jest? vitest? \
     playwright? msw? @testing-library/*? already installed?). \
     Which package manager (pnpm-lock.yaml / yarn.lock / \
     package-lock.json)?
   - `Cargo.toml` — which test runner (cargo test vs nextest), \
     which property-test crate already used (proptest, \
     quickcheck)?
   - `pyproject.toml` / `requirements*.txt` — pytest vs unittest, \
     is hypothesis already a dep?
   - `go.mod` — stdlib testing vs testify vs ginkgo?
   - `Makefile` / `justfile` / `.github/workflows/` — what does CI \
     actually run? Mirror it.
   - Look at a sibling test file in the repo — what conventions \
     does it follow (file naming, imports, fixtures, setup)?

2. **Assess coverage of THIS SESSION'S work.** Scope matters — \
   you are responsible for coverage of what the agent added or \
   changed in this session, not for retroactively covering legacy \
   code the agent didn't touch. Identify the gaps that belong to \
   the current change:
   - What code did EXECUTE add or modify? (check the diff — the \
     workspace has the before-state in git when available, or \
     infer from the ASSESS summary + PLAN + EXECUTE summary.)
   - Of that code, which branches / error paths / edge cases are \
     NOT exercised by the tests that just passed?
   - Which inputs / states / external conditions could trigger \
     the new code but are absent from the existing suite (empty \
     collections, zero, negative, unicode, concurrent access, \
     malformed input, timeout, retry, network failure, clock \
     skew)?
   - Which external boundaries the new code touches are only \
     covered by happy-path tests (the server always returns 200; \
     the DB always has rows; the clock never moves)?
   - For each gap: note the specific behaviour a new test should \
     prove correct AND confirm the behaviour belongs to THIS \
     session's work (not pre-existing untested code).
   Don't add tests for code the agent didn't touch. Don't \
   duplicate coverage that already exists.

3. **Classify the code** in the ambient context of the repo:
   - React / Vue / Solid / Svelte component?
   - HTTP / RPC client?
   - Browser-based user flow?
   - WebSocket / streaming client?
   - Database-backed service?
   - CLI tool?
   - Pure library / algorithm?
   - Async code with timers / retries?

4. **Pick the test stack that the repo already endorses.** Only \
   introduce new tooling when the repo genuinely lacks something \
   and adding it is idiomatic. Starting points (pick WHAT IS OR \
   WOULD BE NATIVE to this repo):
   - React component → Testing Library + \
     `@testing-library/user-event`.
   - API client → **MSW** (`msw`) to intercept real \
     `fetch`/axios calls; exercise retry, error, timeout, \
     non-2xx, malformed-JSON paths.
   - Web user flow → **Playwright** (`@playwright/test`) with a \
     headless browser; script the real click path.
   - WebSocket → stand up a fake WS server in-process; test \
     connect, message parsing, reconnect after disconnect, \
     backpressure.
   - DB layer → `testcontainers` / `sqlite::memory:` / `pg-mem`; \
     round-trip + constraint-violation + transaction tests.
   - CLI → shell out to the binary with fixtures, snapshot stdout.
   - Pure library → property-based (`hypothesis` / `proptest` / \
     `fast-check`) when the domain has laws (idempotent, \
     commutative, inverse-of, ordered).
   - Async / timer-driven → use the test framework's fake clock; \
     race-condition stress where meaningful.
   If the repo is a Rust crate, don't suggest MSW. If the repo is \
   a backend Node service with no browser component, don't \
   install Playwright. Honor the context.

5. **Install tooling the repo-native way.** Use the package \
   manager the repo uses (`pnpm add -D`, `cargo add --dev`, `uv \
   add --dev`, `go get -t`). Follow the repo's existing devDep \
   conventions — if the repo pins versions with `~`, match; if \
   it groups dev deps under a workspace root, add there.

6. **Write the tests in the repo's convention**, one test per \
   gap from step 2. Match file naming (`*_test.go` vs \
   `*.test.ts` vs `tests/foo.rs`), fixture layout, assertion \
   style (existing sibling tests are the style guide). NOT \
   \"one more unit test\" — use the tooling you picked to \
   exercise real boundaries: intercept the network, boot the \
   browser, fake the clock, stand up the fake WS server. Each \
   test closes one specific gap from step 2.

7. **Run them.** Every new test must pass. If one fails, that's a \
   real bug — the workflow will cycle you back into EXECUTE with \
   the failure as the next review finding. Do not ship red tests.

8. **Respond.** Produce your final message in this shape:
   ```
   ## Code Classification
   (one sentence — \"this is a React component with server-driven data\")

   ## Coverage Gaps (in this session's work)
   (bullet list from step 2 — what's uncovered, and why it matters)

   ## Tooling Chosen
   (list the test deps you installed, or \"none needed — existing stack is right\")

   ## Tests Added
   (short bullets — one per gap you closed, naming the gap)

   ## Verdict
   PASS    (iff every new test is green)
   FAIL: <summary>    (otherwise — this loops back to EXECUTE)
   ```

Rules of the road:
- Do NOT modify the ORIGINAL provided tests. Only ADD new ones.
- Do NOT ship tests for unimplemented methods. If a test requires \
  a new surface, add the implementation in the same change.
- Do NOT add tests that pass trivially or duplicate the provided \
  suite — this phase is adversarial on purpose.
- Benchmark-style tasks often have the stub's shape locked; if the \
  only reasonable tests are the ones already given, say so \
  explicitly in Tests Added (\"none — suite already covers the \
  surface\") and emit `PASS` in Verdict.";

    let goal = state.goal_summary.as_deref().unwrap_or("(no goal summary)");
    let exec = state.last_exec_summary.as_deref().unwrap_or("");
    let verify = state.last_verify_output.as_deref().unwrap_or("");
    let progress = progress_block(&state.progress_summary);
    let user = format!(
        "Task:\n\n{task}\n\n## Goal Summary\n\n{goal}\n\n## Work So Far\n\n{progress}\n\n## Final implementation summary\n\n{exec}\n\n## Provided-test verify output\n\n{verify}\n\nThe provided tests are green. Now classify the code and raise the bar with real test coverage using the right stack."
    );
    (sys.to_string(), user)
}

fn finalize_prompt(state: &WorkflowState, task: &str) -> (String, String) {
    let sys = "\
You are in the FINALIZE phase of a structured coding workflow. \
This is the last message the END USER sees when the loop stops \
working. Write it FOR them — plain language, easy to scan, no \
internal phase jargon.

Go back to first principles: does the final state of the code \
actually achieve the Goal Summary from ASSESS? Check against \
the goal, not just the tests — tests can pass on code that \
misses the user's real intent.

Produce your final message in EXACTLY this shape:

    ## Summary

    (ONE paragraph, 3–6 sentences, that marries the goal and \
    the work into a single readable story for the user. Open \
    with what we were trying to do, weave in what actually got \
    done, and close with where we ended up. Plain English — no \
    phase labels, no markdown quoting, no enumerating every \
    iteration. Example tone: \"The task was to implement a \
    bowling-scorer module. We drafted a plan around frame \
    iteration, built the scoring function, caught and fixed an \
    off-by-one in the strike bonus, and finished with \
    property-based tests covering frame boundaries. All 31 \
    tests pass.\")

    ## Verdict

    **SOLVED** / **PARTIAL** / **FAILED** — one sentence why.

    ## What's left for you

    (bullet list — edge cases worth more tests, aspects of the \
    goal the tests don't verify, follow-ups. If nothing, say so \
    in one line: \"Nothing — ready to ship.\")

    <details>
    <summary>Detailed phase log</summary>

    (Copy the Work So Far log verbatim here — the bullet list — \
    for users who want the full trail.)
    </details>

Keep the whole thing tight. No tool calls — this is a pure \
summary for a human reader.";

    let goal = state.goal_summary.as_deref().unwrap_or("(no goal summary)");
    let exec = state.last_exec_summary.as_deref().unwrap_or("");
    let verify = state.last_verify_output.as_deref().unwrap_or("");
    let progress = progress_block(&state.progress_summary);
    let user = format!(
        "Task:\n\n{task}\n\n## Goal Summary (weave into the plain-English Summary paragraph)\n\n{goal}\n\n## Work So Far (weave into the Summary paragraph AND copy verbatim into the collapsed detail block)\n\n{progress}\n\n## Final execute summary\n\n{exec}\n\n## Final verify output\n\n{verify}\n\nWrite the finalization message for the end user in the required shape."
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
            CodingPhase::Test,
            CodingPhase::Finalize,
        ] {
            by_activity.entry(p.activity()).or_default().push(p);
        }

        // Thinking handles Assess + Finalize (deep reading + last-look).
        assert_eq!(by_activity.get(&Activity::Thinking).map(Vec::len), Some(2));
        // Coding handles Execute + Verify (both edit/run tool users).
        assert_eq!(by_activity.get(&Activity::Coding).map(Vec::len), Some(2));
        // Planning owns its own phase.
        assert_eq!(by_activity.get(&Activity::Planning).map(Vec::len), Some(1));
        // Reviewing handles Review + Test (both adversarial).
        assert_eq!(by_activity.get(&Activity::Reviewing).map(Vec::len), Some(2));
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
    fn detect_verify_pass_rejects_rust_ok_value_false_positive() {
        // Regression: old fallback matched `OK (` which appeared in
        // Rust failure diffs like `left: Ok(()), right: Err(...)`.
        // A false positive here silently short-circuited the whole
        // EXECUTE → VERIFY → REVIEW loop on the first cycle.
        let diff = "assertion left == right failed\n  left: Ok(())\n  right: Err(NotEnoughPinsLeft)";
        assert!(!detect_verify_pass(diff));
    }

    #[test]
    fn detect_verify_pass_rejects_prose_passing_without_failing_word() {
        // Another regression target: old fallback matched "PASSING"
        // with absent "FAILING". Phrases like "most suites are
        // passing" or "28 passing (out of 30)" would false-positive.
        let prose = "Summary: 28 passing out of 30. Needs more work.";
        assert!(!detect_verify_pass(prose));
    }

    #[test]
    fn detect_verify_pass_rejects_jest_partial_failure() {
        // Real jest output shape for a partial failure. None of the
        // narrow success markers match; any failure marker wins.
        let jest = "Tests:       2 failed, 28 passed, 30 total\nTest Suites: 1 failed, 1 total";
        assert!(!detect_verify_pass(jest));
    }

    #[test]
    fn detect_verify_pass_accepts_jest_full_green() {
        let jest = "Tests:       30 passed, 0 failed, 30 total\nTest Suites: 1 passed, 1 total";
        assert!(detect_verify_pass(jest));
    }

    #[test]
    fn count_write_tool_calls_counts_edit_and_write_only() {
        use crate::conversation::{Conversation, Message};
        use crate::tool::ToolCall;
        fn tc(name: &str) -> ToolCall {
            ToolCall {
                id: "id".into(),
                name: name.into(),
                arguments: serde_json::json!({}),
            }
        }
        let mut conv = Conversation::new(8192);
        let mut m1 = Message::assistant("");
        m1.tool_calls = vec![tc("read_file"), tc("list_files"), tc("edit_file")];
        conv.messages.push(m1);
        let mut m2 = Message::assistant("");
        m2.tool_calls = vec![tc("write_file"), tc("bash"), tc("edit_file")];
        conv.messages.push(m2);
        // 3 writes total: edit_file, write_file, edit_file.
        assert_eq!(count_write_tool_calls(&conv), 3);
    }

    #[test]
    fn count_tool_calls_named_counts_exact_name_only() {
        use crate::conversation::{Conversation, Message};
        use crate::tool::ToolCall;
        fn tc(name: &str) -> ToolCall {
            ToolCall {
                id: "id".into(),
                name: name.into(),
                arguments: serde_json::json!({}),
            }
        }
        let mut conv = Conversation::new(8192);
        let mut m = Message::assistant("");
        m.tool_calls = vec![tc("bash"), tc("bash"), tc("bash"), tc("edit_file"), tc("read_file")];
        conv.messages.push(m);
        assert_eq!(count_tool_calls_named(&conv, "bash"), 3);
        assert_eq!(count_tool_calls_named(&conv, "edit_file"), 1);
        assert_eq!(count_tool_calls_named(&conv, "nonexistent"), 0);
    }

    #[test]
    fn count_write_tool_calls_zero_when_only_exploration() {
        use crate::conversation::{Conversation, Message};
        use crate::tool::ToolCall;
        fn tc(name: &str) -> ToolCall {
            ToolCall {
                id: "id".into(),
                name: name.into(),
                arguments: serde_json::json!({}),
            }
        }
        // Java-bench-failure shape: lots of list_files, zero writes.
        let mut conv = Conversation::new(8192);
        let mut m = Message::assistant("");
        m.tool_calls = vec![tc("list_files"); 29];
        m.tool_calls.push(tc("read_file"));
        m.tool_calls.push(tc("bash"));
        conv.messages.push(m);
        assert_eq!(count_write_tool_calls(&conv), 0);
    }

    #[test]
    fn execute_prompt_prepends_force_write_note_when_present() {
        let state = WorkflowState {
            execute_force_write_note: Some("YOUR PRIOR TURN WROTE NOTHING.".into()),
            last_review: Some("off-by-one in strike bonus".into()),
            ..Default::default()
        };
        let (_sys, user) = execute_prompt(&state, "task");
        // The note lands in the Review-findings block so EXECUTE
        // sees the correction without losing prior critique.
        assert!(user.contains("YOUR PRIOR TURN WROTE NOTHING"));
        assert!(user.contains("off-by-one in strike bonus"));
        // And the note comes first — it's the blunt correction.
        let note_idx = user.find("YOUR PRIOR TURN").unwrap();
        let review_idx = user.find("off-by-one").unwrap();
        assert!(note_idx < review_idx, "force-write note must come before prior review");
    }

    #[test]
    fn execute_prompt_omits_force_write_note_when_absent() {
        let state = WorkflowState {
            last_review: Some("regular review critique".into()),
            ..Default::default()
        };
        let (_sys, user) = execute_prompt(&state, "task");
        assert!(!user.contains("YOUR PRIOR TURN WROTE NOTHING"));
        assert!(user.contains("regular review critique"));
    }

    #[test]
    fn detect_compile_error_catches_js_parse_error() {
        // The exact shape we've been hitting on JS bowling — agent
        // appended content after the class closure, jest babel parse
        // errored out on the unexpected constructor.
        let jest_out = "TESTS FAILED:\n\n\
            FAIL ./bowling.spec.js\n  \
            Test suite failed to run\n\n    \
            SyntaxError: /workspace/bowling.js: Missing semicolon. (151:15)\n\n      \
            149 | module.exports = Bowling;\n      \
            150 |\n    > \
            151 |   constructor() {\n          \
                |               ^\n      \
            152 |     this.rolls = [];";
        let err = detect_compile_error(jest_out).expect("should detect parse error");
        assert!(err.to_lowercase().contains("syntaxerror") || err.contains("Missing semicolon"));
    }

    #[test]
    fn detect_compile_error_catches_rust_unclosed_delimiter() {
        let cargo_out = "TESTS FAILED:\n\n\
            error: this file contains an unclosed delimiter\n   \
            --> src/lib.rs:193:3\n";
        assert!(detect_compile_error(cargo_out).is_some());
    }

    #[test]
    fn detect_compile_error_catches_go_syntax_error() {
        let go_out = "TESTS FAILED:\n./bowling.go:45:2: syntax error: unexpected }";
        assert!(detect_compile_error(go_out).is_some());
    }

    #[test]
    fn detect_compile_error_ignores_real_assertion_failure() {
        // This is a LOGIC failure, not a compile failure — the code
        // parses fine, it just produced the wrong number. REVIEW
        // should handle it normally, not the compile-error override.
        let rust_out = "TESTS FAILED:\n\
            test all_strikes_is_a_perfect_score_of_300 ... FAILED\n  \
            left: None\n  right: Some(300)\n\
            assertion `left == right` failed";
        assert!(detect_compile_error(rust_out).is_none());
    }

    #[test]
    fn detect_compile_error_ignores_jest_assertion_diff() {
        let jest_out = "TESTS FAILED:\n\
            Bowling › consecutive strikes each get the two roll bonus\n\
            expect(received).toEqual(expected)\n\
            Expected: 81\nReceived: 66";
        assert!(detect_compile_error(jest_out).is_none());
    }

    #[test]
    fn detect_compile_error_catches_python_indentation() {
        let py_out = "TESTS FAILED:\n\
            IndentationError: expected an indented block";
        assert!(detect_compile_error(py_out).is_some());
    }

    #[test]
    fn detect_compile_error_catches_java_compilation_failed() {
        let java_out = "FAILURE: Build failed with an exception.\n\n\
            * What went wrong:\n\
            Execution failed for task ':compileJava'.\n> Error: Compilation failed; see the compiler error output for details.";
        assert!(detect_compile_error(java_out).is_some());
    }

    #[test]
    fn detect_verify_pass_accepts_go_full_green() {
        let go = "ok  bowling 0.123s — 22 passed, 0 failed, 0 errors";
        assert!(detect_verify_pass(go));
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
    fn test_phase_routes_through_reviewing_slot() {
        assert_eq!(CodingPhase::Test.activity(), Activity::Reviewing);
        assert_eq!(CodingPhase::Test.label(), "TEST");
    }

    #[test]
    fn test_phase_prompt_guides_classification_and_tooling() {
        let state = WorkflowState::default();
        let (sys, _user) = test_prompt(&state, "build an api client");
        // Classification guidance
        assert!(sys.contains("Classify the code"));
        // Right-tool-for-the-job callouts — the specific stacks the
        // user flagged: MSW + Playwright + property-based.
        assert!(sys.contains("MSW"));
        assert!(sys.contains("Playwright"));
        assert!(sys.to_lowercase().contains("property-based"));
        assert!(sys.contains("testcontainers"));
        // Orphan-test rule
        assert!(sys.contains("unimplemented methods") || sys.contains("unimplemented"));
        // Verdict format for machine parsing
        assert!(sys.contains("## Verdict"));
        assert!(sys.contains("PASS"));
    }

    #[test]
    fn test_phase_prompt_is_repo_aware() {
        let (sys, _) = test_prompt(&WorkflowState::default(), "task");
        // Survey-first discipline
        assert!(sys.contains("Survey the repo first"));
        // Repo signals the prompt explicitly checks
        assert!(sys.contains("package.json"));
        assert!(sys.contains("Cargo.toml"));
        assert!(sys.contains("go.mod"));
        assert!(sys.contains("pyproject.toml"));
        // Don't force mismatched tooling
        assert!(sys.contains("Rust crate, don't suggest MSW"));
    }

    #[test]
    fn test_phase_prompt_refuses_to_modify_provided_tests() {
        let (sys, _) = test_prompt(&WorkflowState::default(), "task");
        assert!(sys.to_lowercase().contains("do not modify"));
        assert!(sys.contains("ORIGINAL"));
    }

    #[test]
    fn execute_prompt_demands_running_the_test_suite() {
        let state = WorkflowState::default();
        let (sys, user) = execute_prompt(&state, "task");
        // Self-validation must run REAL tests, not just compile-check.
        assert!(
            sys.contains("Self-validation is NOT optional"),
            "execute must force the model to actually run the tests"
        );
        // Repo-first discipline — find the test command the repo
        // actually uses before inventing one.
        assert!(sys.contains("package.json"));
        assert!(sys.contains("Cargo.toml"));
        assert!(sys.contains("pyproject.toml"));
        assert!(sys.contains("go.mod"));
        // Canonical test invocations — the model should know these
        // by name, not reach for compile-only checks.
        assert!(sys.contains("cargo test"));
        assert!(sys.contains("pnpm test") || sys.contains("npm test"));
        assert!(sys.contains("pytest"));
        assert!(sys.contains("go test"));
        // Compile-only checks are the LAST RESORT fallback.
        assert!(sys.contains("last resort") || sys.contains("Fallback when no test command"));
        // Pass/fail count must appear in the output so REVIEW can
        // see truth vs. the model's summary.
        assert!(sys.contains("## Test Results"), "execute must demand a Test Results section in the response");
        assert!(
            user.contains("## Test Results"),
            "execute user prompt must reiterate the Test Results requirement"
        );
    }

    #[test]
    fn execute_prompt_includes_plan_and_review_findings() {
        let state = WorkflowState {
            plan: Some("step 1: write score()".into()),
            last_review: Some("off-by-one in strike bonus".into()),
            ..Default::default()
        };
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
    fn verify_signature_captures_passed_failed_pair() {
        let s = "Test runner output:\n15 passed, 4 failed in 0.03s\n";
        assert_eq!(verify_signature(s), "15p/4f");
    }

    #[test]
    fn verify_signature_case_insensitive() {
        let s = "Tests: 27 PASSED, 4 FAILED, 31 total";
        assert_eq!(verify_signature(s), "27p/4f");
    }

    #[test]
    fn verify_signature_uses_first_error_when_no_counts() {
        let s = "compile error: unexpected token at line 42\n... other lines ...";
        let sig = verify_signature(s);
        assert!(sig.contains("compile error"));
    }

    #[test]
    fn verify_signature_is_stable_across_identical_failures() {
        let a = "ran 31 tests\n3 failed, 28 passed\nassertionerror: expected 81 got 48";
        let b = "ran 31 tests\n3 failed, 28 passed\nassertionerror: expected 81 got 48";
        assert_eq!(verify_signature(a), verify_signature(b));
    }

    #[test]
    fn verify_signature_detects_progress() {
        let old = "15 passed, 16 failed";
        let new_ = "27 passed, 4 failed";
        assert_ne!(verify_signature(old), verify_signature(new_));
    }

    #[test]
    fn verify_prompt_demands_prefix_format() {
        let state = WorkflowState::default();
        let (sys, _user) = verify_prompt(&state, "solve bowling");
        assert!(sys.contains("ALL TESTS PASS"));
        assert!(sys.contains("TESTS FAILED"));
        assert!(sys.contains("EXACTLY ONE"));
    }

    #[test]
    fn append_progress_writes_iteration_tagged_bullet() {
        let mut buf = String::new();
        append_progress(&mut buf, "EXECUTE", 2, "wired the scorer");
        assert_eq!(buf, "- [EXECUTE #2] wired the scorer\n");
    }

    #[test]
    fn append_progress_skips_empty_updates() {
        let mut buf = String::new();
        append_progress(&mut buf, "EXECUTE", 1, "   ");
        assert!(buf.is_empty());
    }

    #[test]
    fn append_progress_omits_iteration_when_zero() {
        let mut buf = String::new();
        append_progress(&mut buf, "ASSESS", 0, "crystallized the goal");
        assert_eq!(buf, "- [ASSESS] crystallized the goal\n");
    }

    #[test]
    fn progress_block_placeholder_for_empty_log() {
        assert_eq!(progress_block(""), "(starting fresh — no prior phases)");
        assert_eq!(progress_block("   \n  "), "(starting fresh — no prior phases)");
    }

    #[test]
    fn progress_block_trims_trailing_blank_lines() {
        let log = "- [ASSESS] crystallized goal\n- [PLAN] drafted plan\n\n";
        assert_eq!(progress_block(log), "- [ASSESS] crystallized goal\n- [PLAN] drafted plan");
    }

    #[test]
    fn plan_prompt_threads_work_so_far() {
        let mut state = WorkflowState::default();
        state.progress_summary.push_str("- [ASSESS] crystallized goal\n");
        let (_sys, user) = plan_prompt(&state, "task");
        assert!(user.contains("## Work So Far"), "plan user prompt must carry Work So Far");
        assert!(user.contains("crystallized goal"));
    }

    #[test]
    fn execute_prompt_threads_work_so_far_and_demands_progress_update() {
        let mut state = WorkflowState::default();
        state.progress_summary.push_str("- [PLAN] drafted plan\n");
        let (_sys, user) = execute_prompt(&state, "task");
        assert!(user.contains("## Work So Far"), "execute user prompt must carry Work So Far");
        assert!(user.contains("drafted plan"));
        assert!(user.contains("## Progress Update"), "execute must demand a Progress Update section");
    }

    #[test]
    fn verify_prompt_threads_work_so_far() {
        let mut state = WorkflowState::default();
        state.progress_summary.push_str("- [EXECUTE #1] wired scorer\n");
        let (_sys, user) = verify_prompt(&state, "task");
        assert!(user.contains("## Work So Far"));
        assert!(user.contains("wired scorer"));
    }

    #[test]
    fn review_prompt_threads_work_so_far() {
        let mut state = WorkflowState::default();
        state.progress_summary.push_str("- [VERIFY #1] tests fail (15p/4f)\n");
        let (_sys, user) = review_prompt(&state, "task");
        assert!(user.contains("## Work So Far"), "review user prompt must carry Work So Far");
        assert!(user.contains("tests fail"));
    }

    #[test]
    fn test_prompt_threads_work_so_far() {
        let mut state = WorkflowState::default();
        state.progress_summary.push_str("- [VERIFY #1] tests PASS\n");
        let (_sys, user) = test_prompt(&state, "task");
        assert!(user.contains("## Work So Far"));
    }

    #[test]
    fn test_prompt_scopes_coverage_to_this_session() {
        let (sys, _user) = test_prompt(&WorkflowState::default(), "task");
        // The TEST phase must focus on gaps in the agent's session
        // work, not retroactive coverage of legacy code.
        assert!(
            sys.to_lowercase().contains("this session"),
            "test phase must scope coverage to this session's work"
        );
        assert!(sys.contains("EXECUTE add or modify") || sys.contains("agent added or changed"));
    }

    #[test]
    fn finalize_prompt_is_shaped_for_the_end_user() {
        let state = WorkflowState {
            goal_summary: Some("Implement bowling scorer".into()),
            progress_summary: "- [EXECUTE #1] wired scorer\n- [VERIFY #1] tests PASS\n".into(),
            ..Default::default()
        };
        let (sys, user) = finalize_prompt(&state, "solve bowling");
        // User-facing framing — not another intra-workflow handoff.
        assert!(sys.to_uppercase().contains("END USER"));
        // Required output sections: combined summary, verdict, what's left, detail log.
        assert!(sys.contains("## Summary"));
        assert!(sys.contains("## Verdict"));
        assert!(sys.contains("## What's left for you"));
        assert!(sys.contains("<details>") && sys.contains("Detailed phase log"));
        // User prompt carries both anchors.
        assert!(user.contains("Implement bowling scorer"));
        assert!(user.contains("## Work So Far"));
        assert!(user.contains("wired scorer"));
    }
}

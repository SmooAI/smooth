//! Coding workflow — single-agent outer loop.
//!
//! The agent handles its own iteration (LLM → tool → LLM → …)
//! via `Agent::run_with_channel`. We sit around that and do three
//! things:
//!
//!   1. Snapshot the workspace when the failing-test count drops
//!      — so a later turn can't regress past the best-seen state.
//!   2. On not-green, feed the test output back into the next
//!      turn's prompt so the agent has surgical failure context.
//!   3. Stop when we're green, within a few failures of green
//!      (more iteration is more likely to regress than improve),
//!      over budget, or past the outer-iteration cap.
//!
//! We used to decompose into 7 phases (ASSESS / PLAN / EXECUTE /
//! VERIFY / REVIEW / TEST / FINALIZE). That added a lot of prompt
//! surface area and failure modes — the phase decomposition kept
//! silent-short-circuiting at one detector or another and eating
//! runs that should have kept going. A single-agent loop is
//! smaller, easier to reason about, and matches the shape of
//! tools like OpenCode that are maintained against coding
//! benchmarks. We kept the parts that demonstrably help — the
//! self-validation requirement in the system prompt, the
//! best-state snapshot, the compile-error short-circuit — and
//! dropped the per-phase dispatch.
//!
//! This module does NOT own the sandbox, the security hooks, or
//! the tool registry — the caller assembles those and hands them
//! in.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::mpsc::UnboundedSender;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::agent::{Agent, AgentConfig, AgentEvent, InjectedMessage};
use crate::cast::Cast;
use crate::cost::CostBudget;
use crate::providers::ProviderRegistry;
use crate::tool::ToolRegistry;

/// Input to `run_coding_workflow`.
pub struct CodingWorkflowConfig {
    /// Stable id for the operator running this workflow — echoed
    /// into every AgentEvent.
    pub operator_id: String,
    /// The task prompt the user gave.
    pub task_prompt: String,
    /// Provider registry — used to resolve the Coding slot.
    pub registry: Arc<ProviderRegistry>,
    /// Tool registry the agent will use.
    pub tools: ToolRegistry,
    /// Optional global budget cap across the whole workflow.
    pub budget_usd: Option<f64>,
    /// Max outer-loop iterations. Each iteration is one full
    /// `Agent::run_with_channel` call; the agent itself iterates
    /// internally via tool calls. 5 is usually plenty — if the
    /// agent can't converge in 5 full turns with failure context,
    /// another turn is unlikely to help.
    pub max_outer_iterations: u32,
    /// Skip any post-implementation test-augmentation phase.
    /// Kept in the config for API stability, currently ignored —
    /// the single-agent loop doesn't have a separate TEST phase.
    pub skip_test_phase: bool,
    /// Event sink — every AgentEvent from the agent flows here.
    pub tx: UnboundedSender<AgentEvent>,
    /// Workspace root (bind-mounted at /workspace inside the
    /// sandbox). Used to snapshot the best-seen state and restore
    /// it on regression. `None` skips snapshotting.
    pub workspace_root: Option<PathBuf>,
    /// Optional injection channel for mailbox messages — passed to every
    /// inner Agent so steering/chat/answers from the lead reach a running
    /// teammate without needing to restart the workflow. `None` keeps
    /// the agent isolated (current behaviour for non-pearl-attached runs).
    pub chat_rx: Option<Arc<tokio::sync::Mutex<UnboundedReceiver<InjectedMessage>>>>,
}

/// Run the workflow end-to-end. Returns the accumulated cost.
///
/// Default path is single-agent (fixer only). Set
/// `SMOOTH_WORKFLOW_MULTIROLE=1` to invoke the multi-role chain:
/// mapper (Planning slot) once, then oracle (Thinking slot) +
/// fixer (Coding slot) per iteration. The mix lets a fast/cheap
/// coder model handle code edits while a thinking model handles
/// strategy and a planner handles up-front decomposition.
pub async fn run_coding_workflow(cfg: CodingWorkflowConfig) -> anyhow::Result<f64> {
    let multirole = std::env::var("SMOOTH_WORKFLOW_MULTIROLE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if multirole {
        return run_coding_workflow_multirole(cfg).await;
    }

    // Pull the fixer role definition from the cast so the prompt
    // lives in one place (`cast/prompts/fixer.txt`) and the slot
    // comes from the role's `slot` field instead of being hard-coded
    // here. The `fixer` role is always present in `Cast::builtin()`
    // — if it ever isn't, something is badly wrong and we want a
    // loud failure, not a silent fallback.
    let cast = Cast::builtin();
    let fixer_role = cast.get("fixer").context("missing 'fixer' role in cast — did Cast::builtin change?")?;
    let code_prompt = fixer_role.prompt.clone();
    let code_slot = fixer_role.slot;

    let llm_config = cfg.registry.llm_config_for(code_slot).context("resolving coding slot → LLM config")?;
    let coding_slot = cfg.registry.routing.slot_for(code_slot);
    let alias = coding_slot.model.clone();

    let mut total_cost_usd = 0.0_f64;
    let mut last_verify_output: Option<String> = None;
    let mut best_failed_count: Option<u32> = None;
    let mut snapshot_taken = false;
    let mut compile_retry_count: u32 = 0;

    let iter_cap = cfg.max_outer_iterations.max(1);
    let mut iteration = 0u32;
    let mut succeeded = false;

    for _ in 0..iter_cap {
        iteration += 1;

        let _ = cfg.tx.send(AgentEvent::PhaseStart {
            phase: "CODING".into(),
            alias: alias.clone(),
            upstream: None,
            iteration,
        });

        let user_prompt = build_user_prompt(&cfg.task_prompt, iteration, last_verify_output.as_deref());

        // Inner iteration cap. Agent can take a lot of tool-call turns
        // internally; default is 80 but `SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS`
        // lets benchmark/diagnostic runs shorten the feedback loop.
        let agent_max_iter: u32 = std::env::var("SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(80);
        let mut agent_config =
            AgentConfig::new(format!("{}/coding-{}", cfg.operator_id, iteration), code_prompt.clone(), llm_config.clone()).with_max_iterations(agent_max_iter);
        if let Some(rx) = cfg.chat_rx.clone() {
            agent_config = agent_config.with_chat_rx(rx);
        }
        if let Some(cap) = cfg.budget_usd {
            let remaining = (cap - total_cost_usd).max(0.0);
            agent_config = agent_config.with_budget(CostBudget {
                max_cost_usd: Some(remaining),
                max_tokens: None,
            });
        }

        let agent = Agent::new(agent_config, cfg.tools.clone());
        let conversation = agent.run_with_channel(user_prompt, cfg.tx.clone()).await?;

        let turn_cost = {
            let tracker = agent.cost_tracker.lock().expect("cost_tracker lock");
            tracker.total_cost_usd
        };
        total_cost_usd += turn_cost;

        // Pull the agent's final assistant message AND any recent
        // tool results that captured the test runner output. The
        // last-assistant-only view loses the verbatim compile error
        // when the LLM summarizes "test failed because X" instead of
        // pasting the rustc/pytest/jest block back into its message.
        // Including the most recent tool results lets detect_verify_pass
        // see green even when the agent forgot the `## Test Results`
        // line, and lets detect_compile_error pull a real diagnostic.
        let transcript = last_diagnostic_text(&conversation);
        last_verify_output = Some(transcript.clone());

        // Green? We're done.
        if detect_verify_pass(&transcript) {
            succeeded = true;
            tracing::info!(iteration, "coding workflow: agent reports green, stopping");
            break;
        }

        // Compile-error retry: if the test harness never even ran
        // because the code wouldn't compile/parse, force another
        // fixer iteration regardless of close-to-green / regression
        // heuristics. The next prompt's syntax-mode branch will feed
        // the verbatim diagnostic back to the agent. Capped at
        // MAX_COMPILE_RETRIES so a stubbornly-broken model doesn't
        // burn the entire budget on the same dereferencing mistake.
        let compile_err_present = detect_compile_error(&transcript).is_some();
        if compile_err_present {
            compile_retry_count += 1;
        } else {
            compile_retry_count = 0;
        }

        // Snapshot the workspace when this turn was the best so
        // far. If the agent never reports a count, we still snap
        // the first turn so a later regression has something to
        // restore to.
        let current_failed = extract_failed_count(&transcript);
        let improved = match (current_failed, best_failed_count) {
            (Some(now), Some(best)) => now < best,
            (Some(_), None) => true,
            (None, _) if !snapshot_taken => true, // first turn, unknown count
            _ => false,
        };
        if improved {
            if let Some(ref ws) = cfg.workspace_root {
                match snapshot_workspace(ws, &best_snapshot_dir(ws)) {
                    Ok(()) => {
                        snapshot_taken = true;
                        if let Some(now) = current_failed {
                            best_failed_count = Some(now);
                        }
                        tracing::info!(iteration, failed = current_failed, "coding workflow: snapshotted best-seen workspace");
                    }
                    Err(e) => tracing::warn!(error = %e, "coding workflow: snapshot failed"),
                }
            }
        }

        // Close-to-green stop. When we've seen a turn at ≤3 failures
        // and this turn didn't improve on it, another cycle is more
        // likely to regress than close the gap. EXCEPTION: if this
        // turn shipped code that doesn't compile at all, we have NOT
        // converged — close-to-green is meaningless because no test
        // even ran. Force another iteration so the syntax-mode prompt
        // can feed the diagnostic back, capped at MAX_COMPILE_RETRIES.
        if let Some(best) = best_failed_count {
            if best <= CLOSE_TO_GREEN_THRESHOLD && !improved && !(compile_err_present && compile_retry_count <= MAX_COMPILE_RETRIES) {
                tracing::info!(iteration, best_failed = best, "coding workflow: close to green, stopping before regression");
                break;
            }
        }

        if compile_err_present && compile_retry_count > MAX_COMPILE_RETRIES {
            tracing::info!(iteration, compile_retry_count, "coding workflow: max compile-error retries hit, stopping");
            break;
        }

        // Budget check: next turn would blow the cap.
        if let Some(cap) = cfg.budget_usd {
            if cap > 0.0 && total_cost_usd > 0.0 {
                let per_iter = total_cost_usd / f64::from(iteration);
                if total_cost_usd + per_iter >= cap {
                    tracing::info!(spent = total_cost_usd, cap, "coding workflow: budget exhausted");
                    break;
                }
            }
        }
    }

    // Restore the best-seen workspace if a later turn regressed.
    if !succeeded {
        if let (Some(ref ws), Some(best), true) = (&cfg.workspace_root, best_failed_count, snapshot_taken) {
            let final_failed = extract_failed_count(last_verify_output.as_deref().unwrap_or(""));
            let regressed = final_failed.is_some_and(|n| n > best);
            let snap = best_snapshot_dir(ws);
            if regressed && snap.is_dir() {
                match restore_workspace(&snap, ws) {
                    Ok(()) => tracing::info!(best_failed = best, "coding workflow: restored workspace to best-seen state"),
                    Err(e) => tracing::warn!(error = %e, "coding workflow: restore failed"),
                }
            }
        }
    }

    // Remove the snapshot so it doesn't leak into the scorer's
    // view of the workspace or a follow-up run on the same dir.
    if let Some(ref ws) = cfg.workspace_root {
        let snap = best_snapshot_dir(ws);
        if snap.is_dir() {
            let _ = std::fs::remove_dir_all(&snap);
        }
    }

    let _ = cfg.tx.send(AgentEvent::Completed {
        agent_id: cfg.operator_id.clone(),
        iterations: iteration,
        cost_usd: total_cost_usd,
    });

    Ok(total_cost_usd)
}

/// Multi-role workflow: mapper (Planning) once, then oracle
/// (Thinking) + fixer (Coding) per iteration.
///
/// Mapper produces a concrete plan from the task description. Each
/// iteration the oracle reviews the latest test output (or the plan
/// on iteration 1), reasons about strategy, and produces a directed
/// nudge. Then the fixer (the only role with mutating tools) takes
/// the plan + oracle's advice + last test output and writes code.
///
/// Each role uses its own LLM slot — so smooth-coding can be a fast
/// cheap coder while smooth-reasoning is a slow smart thinker.
async fn run_coding_workflow_multirole(cfg: CodingWorkflowConfig) -> anyhow::Result<f64> {
    use crate::cast::OperatorRole;

    let cast = Cast::builtin();
    let mapper_role = cast.get("mapper").context("missing 'mapper' role in cast")?.clone();
    let oracle_role = cast.get("oracle").context("missing 'oracle' role in cast")?.clone();
    let fixer_role = cast.get("fixer").context("missing 'fixer' role in cast")?.clone();

    let mut total_cost_usd = 0.0_f64;

    // Phase 0 (once): mapper decomposes the task into a plan.
    let mapper_input = format!(
        "Decompose this benchmark task into a concrete implementation plan. Read the existing files to understand the starting state, the test suite, and any constraints. Produce a numbered list of steps.\n\n## Task\n\n{}",
        cfg.task_prompt
    );
    let plan_outcome = run_role_phase(&cfg, &mapper_role, &mapper_input, "MAPPING", 1).await?;
    total_cost_usd += plan_outcome.cost;
    let plan_text = plan_outcome.transcript;

    let mut last_verify_output: Option<String> = None;
    let mut best_failed_count: Option<u32> = None;
    let mut snapshot_taken = false;
    let mut compile_retry_count: u32 = 0;
    let iter_cap = cfg.max_outer_iterations.max(1);
    let mut iteration = 0u32;
    let mut succeeded = false;

    for _ in 0..iter_cap {
        iteration += 1;

        // Phase 1: oracle reviews state and recommends next move.
        let oracle_input = build_oracle_prompt(&cfg.task_prompt, &plan_text, last_verify_output.as_deref(), iteration);
        let advice_outcome = run_role_phase(&cfg, &oracle_role, &oracle_input, "ORACLE", iteration).await?;
        total_cost_usd += advice_outcome.cost;

        // Phase 2: fixer implements.
        let fixer_input = build_fixer_prompt(
            &cfg.task_prompt,
            &plan_text,
            &advice_outcome.transcript,
            last_verify_output.as_deref(),
            iteration,
        );
        let fix_outcome = run_role_phase(&cfg, &fixer_role, &fixer_input, "FIXING", iteration).await?;
        total_cost_usd += fix_outcome.cost;

        let transcript = fix_outcome.transcript;
        last_verify_output = Some(transcript.clone());

        if detect_verify_pass(&transcript) {
            succeeded = true;
            tracing::info!(iteration, "multirole workflow: green, stopping");
            break;
        }

        // Compile-error retry: see single-agent loop for rationale.
        // A compile failure means tests never ran — close-to-green
        // is meaningless and the next iteration MUST get the verbatim
        // diagnostic. Capped at MAX_COMPILE_RETRIES.
        let compile_err_present = detect_compile_error(&transcript).is_some();
        if compile_err_present {
            compile_retry_count += 1;
        } else {
            compile_retry_count = 0;
        }

        let current_failed = extract_failed_count(&transcript);
        let improved = match (current_failed, best_failed_count) {
            (Some(now), Some(best)) => now < best,
            (Some(_), None) => true,
            (None, _) if !snapshot_taken => true,
            _ => false,
        };
        if improved {
            if let Some(ref ws) = cfg.workspace_root {
                if snapshot_workspace(ws, &best_snapshot_dir(ws)).is_ok() {
                    snapshot_taken = true;
                    if let Some(now) = current_failed {
                        best_failed_count = Some(now);
                    }
                }
            }
        }

        if let (Some(best), Some(now)) = (best_failed_count, current_failed) {
            if best <= CLOSE_TO_GREEN_THRESHOLD && now >= best && !(compile_err_present && compile_retry_count <= MAX_COMPILE_RETRIES) {
                tracing::info!(best, now, "multirole workflow: close-to-green, stopping");
                break;
            }
        }

        if compile_err_present && compile_retry_count > MAX_COMPILE_RETRIES {
            tracing::info!(iteration, compile_retry_count, "multirole workflow: max compile-error retries hit, stopping");
            break;
        }

        if let Some(cap) = cfg.budget_usd {
            if cap > 0.0 && total_cost_usd > 0.0 {
                let per_iter = total_cost_usd / f64::from(iteration);
                if total_cost_usd + per_iter >= cap {
                    tracing::info!(spent = total_cost_usd, cap, "multirole workflow: budget exhausted");
                    break;
                }
            }
        }
    }

    if !succeeded {
        if let (Some(ref ws), Some(best), true) = (&cfg.workspace_root, best_failed_count, snapshot_taken) {
            let final_failed = extract_failed_count(last_verify_output.as_deref().unwrap_or(""));
            let regressed = final_failed.is_some_and(|n| n > best);
            let snap = best_snapshot_dir(ws);
            if regressed && snap.is_dir() {
                let _ = restore_workspace(&snap, ws);
            }
        }
    }
    if let Some(ref ws) = cfg.workspace_root {
        let snap = best_snapshot_dir(ws);
        if snap.is_dir() {
            let _ = std::fs::remove_dir_all(&snap);
        }
    }

    let _ = cfg.tx.send(AgentEvent::Completed {
        agent_id: cfg.operator_id.clone(),
        iterations: iteration,
        cost_usd: total_cost_usd,
    });

    Ok(total_cost_usd)
}

struct PhaseOutcome {
    transcript: String,
    cost: f64,
}

/// Run a single role's Agent loop with the role's slot + clearance.
/// Tools are filtered down to what the role's `Clearance.allows()`
/// accepts — so mapper/oracle can't accidentally write code even if
/// the underlying tool registry has edit_file/etc.
async fn run_role_phase(
    cfg: &CodingWorkflowConfig,
    role: &crate::cast::OperatorRole,
    user_prompt: &str,
    phase: &str,
    iteration: u32,
) -> anyhow::Result<PhaseOutcome> {
    let mut llm_config = cfg
        .registry
        .llm_config_for(role.slot)
        .with_context(|| format!("resolving {:?} slot for role '{}'", role.slot, role.name))?;
    let mut alias = cfg.registry.routing.slot_for(role.slot).model.clone();

    // Per-role model override. Env var name is
    // `SMOOTH_<ROLE>_MODEL_OVERRIDE` (uppercase). When set, swaps the
    // model on the resolved config — keeps the gateway URL + key from
    // the slot routing, just changes which upstream model the runner
    // asks for. Lets the bench A/B different fixers/mappers/oracles
    // without redeploying LiteLLM.
    let env_var = format!("SMOOTH_{}_MODEL_OVERRIDE", role.name.to_ascii_uppercase());
    if let Ok(override_model) = std::env::var(&env_var) {
        if !override_model.trim().is_empty() {
            tracing::info!(
                role = %role.name,
                slot = ?role.slot,
                from = %llm_config.model,
                to = %override_model,
                "multirole: applying per-role model override"
            );
            llm_config.model = override_model.clone();
            alias = override_model;
        }
    }

    let _ = cfg.tx.send(AgentEvent::PhaseStart {
        phase: phase.into(),
        alias: alias.clone(),
        upstream: None,
        iteration,
    });

    // Tool registry filtered by the role's clearance. Each role gets
    // a fresh clone — mapper/oracle won't see edit_file/write_file,
    // fixer sees the full set.
    let mut tools = cfg.tools.clone();
    let role_clearance = role.permissions.clone();
    tools.retain(|name| role_clearance.allows(name));

    let agent_max_iter: u32 = std::env::var("SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    let mut agent_config = crate::agent::AgentConfig::new(format!("{}/{}-{}", cfg.operator_id, role.name, iteration), role.prompt.clone(), llm_config)
        .with_max_iterations(agent_max_iter);
    if let Some(rx) = cfg.chat_rx.clone() {
        agent_config = agent_config.with_chat_rx(rx);
    }

    let agent = Agent::new(agent_config, tools);
    let conversation = agent.run_with_channel(user_prompt, cfg.tx.clone()).await?;
    let cost = {
        let tracker = agent.cost_tracker.lock().expect("cost_tracker lock");
        tracker.total_cost_usd
    };
    // Use the rich diagnostic view (assistant message + most recent
    // tool results) so a fixer that ran the test runner but didn't
    // paste the rustc/pytest output verbatim into its summary still
    // surfaces compile errors and pass/fail counts to the workflow.
    let transcript = last_diagnostic_text(&conversation);

    Ok(PhaseOutcome { transcript, cost })
}

/// Compose the oracle's user-message: tell it about the plan, the
/// task, and the latest failure context. Output: directed advice
/// for the fixer's next move.
fn build_oracle_prompt(task: &str, plan: &str, prior_output: Option<&str>, iteration: u32) -> String {
    if iteration == 1 {
        return format!(
            "A planner has decomposed this task. Review the plan and the existing code. Identify any sharp edges in the planned approach (off-by-one risks, missing edge cases, things the plan glossed over). Produce 2-4 short pointed notes the implementer should keep in mind, and call out which step to tackle FIRST.\n\n## Plan\n{plan}\n\n## Task (reminder)\n{task}"
        );
    }
    let prior = prior_output.unwrap_or("(no prior output)");
    format!(
        "Previous fixer attempt didn't reach green. Read the failing test output below; identify the SPECIFIC bug (algorithm error, off-by-one, missed edge case, wrong formula). Produce 2-3 short pointed notes for the fixer's next attempt. Quote the failing test by name when it helps.\n\n## Plan\n{plan}\n\n## Last test output (truncated)\n{}\n\n## Task (reminder)\n{task}",
        prior.chars().take(3000).collect::<String>()
    )
}

/// Compose the fixer's user-message: plan + oracle advice + failure
/// context + task. Single LLM-with-tools call follows.
fn build_fixer_prompt(task: &str, plan: &str, advice: &str, prior_output: Option<&str>, iteration: u32) -> String {
    if iteration == 1 {
        return format!(
            "Implement the task. The planner produced a decomposition; the oracle has flagged sharp edges to watch for. Make the change, run the test suite, and iterate until green. Finish your final assistant turn with a `## Test Results` line.\n\n## Plan\n{plan}\n\n## Oracle's notes\n{advice}\n\n## Task\n{task}"
        );
    }
    let prior = prior_output.unwrap_or("");
    // Mirror the single-agent path's syntax-mode branch: when the
    // prior turn shipped code that doesn't compile/parse, the rustc
    // (or pytest, jest, javac, …) output usually contains the literal
    // fix as a "help:" / suggestion line. Pass that block through
    // verbatim so the fixer doesn't have to re-derive it.
    let compile_err = detect_compile_error(prior);
    let prior_section = if let Some(err) = compile_err.as_deref() {
        format!(
            "\n\n## Compile error from prior attempt\nThe code did not compile / parse — no tests ran. The compiler / parser emitted the diagnostic below; pay close attention to any `help:` / suggestion lines, they often spell out the fix.\n\n```\n{err}\n```"
        )
    } else if !prior.is_empty() {
        format!("\n\n## Last test output\n{}", prior.chars().take(3000).collect::<String>())
    } else {
        String::new()
    };
    let lead = if compile_err.is_some() {
        format!("Iteration {iteration}: prior attempt shipped code that does not compile / parse. Tests never ran. Read the compiler diagnostic carefully, apply the suggested fix (or its equivalent), and re-run the tests. The error block below contains a literal `help:` line in many cases — that is usually the answer.")
    } else {
        format!("Iteration {iteration}: prior attempt didn't reach green. The oracle has reviewed the failure and flagged the specific bug. Apply a targeted patch and re-run. Don't rewrite working code — only fix what the oracle named.")
    };
    format!("{lead} Finish with `## Test Results`.\n\n## Plan\n{plan}\n\n## Oracle's diagnosis\n{advice}{prior_section}\n\n## Task (reminder)\n{task}")
}

/// Stop escalating when we're this close to green — more
/// iteration is more likely to regress than close the gap.
const CLOSE_TO_GREEN_THRESHOLD: u32 = 3;

/// Maximum consecutive compile-error retries before we stop forcing
/// another iteration. If the model can't deref a `&char` after this
/// many tries with the compiler literally telling it the answer,
/// further iteration won't help and we'd be burning budget.
const MAX_COMPILE_RETRIES: u32 = 3;

// The coding system prompt lives in `crates/smooth-operator/src/cast/prompts/fixer.txt`
// and is loaded by `Cast::builtin()` via `include_str!`. The
// workflow resolves it at the top of `run_coding_workflow` so adding a
// new prompt-aware role there gives all call sites the same text.

/// Build the user-message prompt for a given outer iteration.
/// The first turn just shows the task. Subsequent turns include
/// the prior turn's test output so the agent has concrete failure
/// context to act on — this is what turns the outer loop from
/// "try the same thing again" into "try again with specific
/// failure feedback."
fn build_user_prompt(task: &str, iteration: u32, prior_output: Option<&str>) -> String {
    if iteration == 1 {
        return format!("{task}\n\nImplement the solution, run the test suite, and iterate until green. Finish with a `## Test Results` line.");
    }
    let prior = prior_output.unwrap_or("(no prior output)");
    let compile_err = detect_compile_error(prior);
    let preamble = if let Some(err) = compile_err {
        format!(
            "Your previous attempt shipped code that does not compile / parse. Before doing anything else, fix the syntax. The usual cause is a duplicated class body or extra content appended after the module's export. \n\n## Compile error\n\n{err}\n\n"
        )
    } else {
        format!(
            "Your previous attempt left some tests failing. The output from your last test run is below. Keep every test that's currently passing passing — most test regressions come from rewriting code that was working. Make a targeted patch that closes the specific failures.\n\n## Previous test output (truncated)\n\n{}\n\n",
            prior.chars().take(3000).collect::<String>()
        )
    };
    format!("{preamble}## Task (reminder)\n\n{task}\n\nFix the remaining failures and re-run the tests. Finish with a `## Test Results` line.")
}

// ---------------------------------------------------------------------------
// Helpers: test-result parsing, compile-error detection, snapshots.
// These are the same helpers the old multi-phase workflow used;
// they carry their own unit tests below and don't care whether
// the surrounding loop is one phase or seven.
// ---------------------------------------------------------------------------

#[allow(dead_code)] // kept for any external callers; workflow itself uses last_diagnostic_text.
fn summarize_conversation(conv: &crate::conversation::Conversation) -> String {
    conv.messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, crate::conversation::Role::Assistant))
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

/// Build a compact diagnostic view of the conversation: the agent's
/// last assistant message plus the most recent tool-result messages.
///
/// Why both: when a fixer agent runs `cargo test` / `pytest` via the
/// bash tool, the verbatim runner output (compile errors, "help:"
/// hints, "N passed / M failed" summaries) lands in the *tool result*
/// message. The assistant's follow-up message often paraphrases —
/// "the test failed because of a type mismatch" — which strips the
/// suggestion line the next fixer iteration desperately needs.
/// Including the recent tool results restores that signal for both
/// `detect_verify_pass` (so a green run is detected even when the
/// agent forgets `## Test Results`) and `detect_compile_error` (so
/// the rustc/javac/SyntaxError block survives).
///
/// Each tool result is head/tail-trimmed to keep a verbose pip log
/// or 100MB docker pull from drowning the actual error.
fn last_diagnostic_text(conv: &crate::conversation::Conversation) -> String {
    use crate::conversation::Role;
    const PER_TOOL_RESULT_LIMIT: usize = 2000;
    const MAX_TOOL_RESULTS: usize = 2;

    let mut tool_blocks: Vec<String> = Vec::new();
    let mut assistant_block: Option<String> = None;
    for msg in conv.messages.iter().rev() {
        match msg.role {
            Role::Assistant if assistant_block.is_none() && !msg.content.trim().is_empty() => {
                assistant_block = Some(msg.content.clone());
            }
            Role::Tool if tool_blocks.len() < MAX_TOOL_RESULTS => {
                let trimmed = head_tail(&msg.content, PER_TOOL_RESULT_LIMIT);
                tool_blocks.push(trimmed);
            }
            _ => {}
        }
        if assistant_block.is_some() && tool_blocks.len() >= MAX_TOOL_RESULTS {
            break;
        }
    }

    let mut out = String::new();
    if let Some(asst) = assistant_block {
        out.push_str(&asst);
        out.push('\n');
    }
    // Tool results in chronological order (we collected them in
    // reverse).
    for block in tool_blocks.iter().rev() {
        out.push_str("\n## Tool result\n");
        out.push_str(block);
        out.push('\n');
    }
    out
}

/// Keep the head and tail of a string, dropping the middle when
/// total byte length exceeds `limit`. Useful for tool-result blocks
/// where the early summary line + final error block carry the
/// signal but hundreds of intermediate lines (compile units, pip
/// install logs) don't.
///
/// Char-aware: never panics on a multi-byte boundary. If the input
/// has fewer chars than `2 * limit` but more bytes (worst case all
/// 4-byte UTF-8), the head+tail join would exceed the byte budget
/// — we accept that here, since the goal is to bound transcript
/// growth for typical (mostly-ASCII) compiler output, not to
/// guarantee a strict byte cap.
fn head_tail(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let half = limit / 2;
    let total_chars = s.chars().count();
    if total_chars <= half * 2 {
        return s.to_string();
    }
    let head: String = s.chars().take(half).collect();
    let tail: String = s.chars().skip(total_chars - half).collect();
    format!("{head}\n…[truncated middle]…\n{tail}")
}

/// True when the transcript reports the test suite is green.
/// Explicit prefix (`ALL TESTS PASS`) wins; runner-summary
/// fallbacks are narrow to avoid false positives on prose or
/// on Rust `Ok(..)` values that appear in failure diffs.
pub fn detect_verify_pass(transcript: &str) -> bool {
    let upper = transcript.to_uppercase();
    if upper.contains("ALL TESTS PASS") {
        return true;
    }
    if upper.contains("TESTS FAILED") || upper.contains("TESTS FAIL") {
        return false;
    }
    if nonzero_failure_count(&upper) || upper.contains("TEST RESULT: FAILED") {
        return false;
    }
    upper.contains("TEST RESULT: OK")                       // cargo test
        || upper.contains(" PASSED, 0 FAILED")              // pytest / go / jest
        || upper.contains("0 FAILED, 0 ERRORS")             // go test verbose
        || (upper.contains("TESTS:") && upper.contains(" PASSED") && upper.contains("0 FAILED"))
}

/// Extract the "N failed" count from a transcript. `None` when
/// we can't parse a shape — callers treat that as "unknown" and
/// fall through to iteration without progress tracking.
pub fn extract_failed_count(transcript: &str) -> Option<u32> {
    scan_count(&transcript.to_lowercase(), "failed")
}

fn scan_count(haystack: &str, needle: &str) -> Option<u32> {
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

/// True when the transcript contains a POSITIVE failure count.
/// Zero-failure counts ("0 failed") don't count — they appear
/// in green summaries. We only bail out on failure when a real
/// non-zero count shows up.
fn nonzero_failure_count(upper: &str) -> bool {
    let needles = ["FAILED", "FAILURE", "FAILING"];
    for needle in needles {
        let mut search = upper;
        while let Some(idx) = search.find(needle) {
            let before = &search[..idx];
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

/// Pull a compile / parse / syntax error snippet out of a
/// transcript when the failure isn't a normal test assertion.
/// Returns `None` when we should treat the failure as a regular
/// red-test run. Used by `build_user_prompt` and
/// `build_fixer_prompt` to switch retry tone from "fix the
/// failures" to "fix the syntax / type" — and by the workflow loop
/// to force at least one more fixer iteration when the test harness
/// never even ran.
///
/// Matching strategy: pick the EARLIEST occurrence of any pattern
/// in the transcript (not the first pattern in the list to match
/// somewhere). Otherwise rustc output like
///
///   error[E0308]: mismatched types
///   …help: consider dereferencing the borrow…
///   error: could not compile `alphametics` …
///
/// would have its snippet anchored at "could not compile" (because
/// that pattern happened to be earlier in the array), missing the
/// actual error block + help: line.
fn detect_compile_error(transcript: &str) -> Option<String> {
    let upper = transcript.to_uppercase();
    let patterns = [
        // Rust — anchor on "ERROR[E" first so the snippet starts at
        // the actual diagnostic block (which carries the `help:`
        // suggestion line) rather than the trailing
        // "could not compile" summary.
        "ERROR[E",
        "COULD NOT COMPILE",
        "THIS FILE CONTAINS AN UNCLOSED DELIMITER",
        "EXPECTED ONE OF",
        "MISMATCHED TYPES",
        "CANNOT FIND VALUE",
        "CANNOT FIND TYPE",
        // Python — syntax + type/name errors that fail before tests run.
        // pytest collection errors land here too.
        "SYNTAXERROR",
        "INDENTATIONERROR",
        "TABERROR",
        "TYPEERROR:",
        "NAMEERROR:",
        "ATTRIBUTEERROR:",
        "IMPORTERROR:",
        "MODULENOTFOUNDERROR:",
        "ERROR COLLECTING",
        // JS / TS
        "UNEXPECTED TOKEN",
        "MISSING SEMICOLON",
        "UNCLOSED DELIMITER",
        "UNEXPECTED EOF",
        "UNEXPECTED END OF INPUT",
        "REFERENCEERROR:",
        "ERROR TS", // tsc errors look like `error TS2304: ...`
        // Go
        "SYNTAX ERROR:",
        "EXPECTED '{'",
        "EXPECTED ';'",
        "UNDEFINED:",
        "CANNOT USE",
        "CANNOT CONVERT",
        // Java
        "REACHED END OF FILE",
        "';' EXPECTED",
        "CLASS, INTERFACE, OR ENUM EXPECTED",
        "ERROR: COMPILATION FAILED",
        "CANNOT FIND SYMBOL",
        "INCOMPATIBLE TYPES",
        // Generic
        "FATAL ERROR:",
    ];
    // Earliest hit across all patterns wins.
    let hit_idx = patterns.iter().filter_map(|p| upper.find(p)).min()?;
    // upper.to_uppercase() can lengthen the string for some unicode
    // characters; for ASCII it's a 1:1 byte mapping. Most compiler
    // output is ASCII, so byte index ~= char index here.
    let bytes_per_char = transcript.len().checked_div(upper.len().max(1)).unwrap_or(1).max(1);
    let start = hit_idx.saturating_mul(bytes_per_char).saturating_sub(120);
    let end = (hit_idx.saturating_mul(bytes_per_char).saturating_add(800)).min(transcript.len());
    // Snap to char boundaries so we never panic on a multi-byte cut.
    let start = next_char_boundary(transcript, start);
    let end = prev_char_boundary(transcript, end);
    let snippet = transcript.get(start..end).unwrap_or(transcript);
    Some(snippet.trim().to_string())
}

fn next_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn prev_char_boundary(s: &str, mut i: usize) -> usize {
    if i > s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// Best-state snapshot + restore. Lives under a hidden dir inside
// the workspace so `pytest` / `jest` / `cargo test` / gradle
// all skip it naturally.

fn best_snapshot_dir(workspace: &Path) -> PathBuf {
    workspace.join(".smooth-best-snapshot")
}

fn is_snapshot_excluded(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(".git")
            | Some(".smooth-best-snapshot")
            | Some("node_modules")
            | Some("target")
            | Some("build")
            | Some("dist")
            | Some("__pycache__")
            | Some(".pytest_cache")
            | Some(".venv")
            | Some("venv")
            | Some(".gradle")
            | Some(".cargo")
    )
}

/// Refuse to snapshot a workspace that's clearly NOT a project — most
/// commonly $HOME (or a parent of it) when the chat agent dispatched a
/// teammate without passing a working_dir, which makes the runner
/// inherit Big Smooth's cwd. Recursing through tens of GB of user data
/// hangs the workflow; better to skip the snapshot than freeze.
///
/// Heuristic:
///   * if the dir IS or is a parent of $HOME → unsafe
///   * if the dir contains classic $HOME children (`Library`, `Desktop`,
///     `Documents`) → unsafe
///   * if it has more than 200 top-level entries → unsafe
fn is_unsafe_to_snapshot(src: &Path) -> bool {
    if let Ok(home) = std::env::var("HOME") {
        let home_path = std::path::PathBuf::from(home);
        if let (Ok(c_src), Ok(c_home)) = (src.canonicalize(), home_path.canonicalize()) {
            if c_src == c_home || c_home.starts_with(&c_src) {
                return true;
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(src) {
        let mut count = 0usize;
        for entry in rd.flatten() {
            count += 1;
            if count > 200 {
                return true;
            }
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some("Library") | Some("Desktop") | Some("Documents") | Some("Movies") | Some("Pictures")
            ) {
                return true;
            }
        }
    }
    false
}

fn snapshot_workspace(src: &Path, dst: &Path) -> std::io::Result<()> {
    if is_unsafe_to_snapshot(src) {
        tracing::warn!(
            src = %src.display(),
            "coding workflow: refusing to snapshot — workspace looks like $HOME or a non-project dir"
        );
        return Ok(());
    }
    if dst.exists() {
        std::fs::remove_dir_all(dst)?;
    }
    std::fs::create_dir_all(dst)?;
    copy_recursive(src, dst)
}

fn restore_workspace(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dst)? {
        let entry = entry?;
        let name = entry.file_name();
        if is_snapshot_excluded(&name) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    copy_recursive(src, dst)
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if is_snapshot_excluded(&name) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_recursive(&from, &to)?;
        } else if file_type.is_symlink() {
            if let Ok(target) = std::fs::read_link(&from) {
                let _ = std::fs::remove_file(&to);
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &to)?;
                #[cfg(not(unix))]
                std::fs::copy(&from, &to)?;
            }
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_to_snapshot_flags_home_lookalikes() {
        let tmp = tempfile::tempdir().expect("tmp");
        // A project-like dir is fine.
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert!(!is_unsafe_to_snapshot(tmp.path()));

        // A dir with macOS HOME-style children is rejected.
        let homey = tempfile::tempdir().expect("home");
        for child in ["Library", "Desktop", "Documents"] {
            std::fs::create_dir_all(homey.path().join(child)).unwrap();
        }
        assert!(is_unsafe_to_snapshot(homey.path()));
    }

    #[test]
    fn detect_verify_pass_explicit_marker() {
        assert!(detect_verify_pass("ALL TESTS PASS — 31 of 31."));
        assert!(!detect_verify_pass("TESTS FAILED:\nsome failure"));
    }

    #[test]
    fn detect_verify_pass_runner_summaries() {
        assert!(detect_verify_pass("test result: ok. 31 passed; 0 failed;"));
        assert!(detect_verify_pass("Tests:       30 passed, 0 failed, 30 total"));
        assert!(!detect_verify_pass("Tests: 2 failed, 28 passed"));
    }

    #[test]
    fn detect_verify_pass_rejects_rust_ok_false_positive() {
        // Regression: old fallback matched `OK (` on Rust failure
        // diffs with `Ok(())` values. Must return false here.
        let diff = "assertion `left == right` failed\n  left: Ok(())\n  right: Err(NotEnoughPinsLeft)";
        assert!(!detect_verify_pass(diff));
    }

    #[test]
    fn detect_compile_error_catches_js_syntax() {
        let jest = "TESTS FAILED:\n\nSyntaxError: /workspace/bowling.js: Missing semicolon. (151:15)";
        assert!(detect_compile_error(jest).is_some());
    }

    #[test]
    fn detect_compile_error_catches_rust_unclosed() {
        let cargo = "TESTS FAILED:\nerror: this file contains an unclosed delimiter\n   --> src/lib.rs:193:3";
        assert!(detect_compile_error(cargo).is_some());
    }

    #[test]
    fn detect_compile_error_ignores_real_assertion() {
        let rust = "TESTS FAILED:\ntest all_strikes_is_300 ... FAILED\n  left: None\n  right: Some(300)";
        assert!(detect_compile_error(rust).is_none());
    }

    #[test]
    fn extract_failed_count_standard_shapes() {
        assert_eq!(extract_failed_count("3 failed, 28 passed"), Some(3));
        assert_eq!(extract_failed_count("Tests: 2 failed, 28 passed"), Some(2));
        assert_eq!(extract_failed_count("all tests pass"), None);
    }

    #[test]
    fn build_user_prompt_first_iter_is_plain_task() {
        let p = build_user_prompt("solve bowling", 1, None);
        assert!(p.starts_with("solve bowling"));
        assert!(p.contains("## Test Results"));
        assert!(!p.contains("previous attempt"), "iter 1 has no prior context");
    }

    #[test]
    fn build_user_prompt_subsequent_iter_includes_prior_output_and_preserve_passing_warning() {
        let prior = "2 failed, 28 passed\nconsecutive strikes got 66, expected 81";
        let p = build_user_prompt("solve bowling", 2, Some(prior));
        assert!(p.contains("previous attempt"));
        assert!(p.contains("28 passed") || p.contains("2 failed"));
        assert!(p.to_lowercase().contains("keep every test that's currently passing"));
    }

    #[test]
    fn build_user_prompt_switches_to_syntax_mode_on_compile_error() {
        let prior = "SyntaxError: Missing semicolon (151:15)";
        let p = build_user_prompt("task", 2, Some(prior));
        assert!(p.contains("does not compile"));
        assert!(p.contains("Missing semicolon"));
    }

    /// Real failure record from bench run b2ff102e: rustc emitted both
    /// the E0308 mismatched-types error AND a literal `help: consider
    /// dereferencing the borrow` line. The fixer dispatch never retried.
    /// `detect_compile_error` MUST flag this AND the snippet MUST
    /// include the help: line so the next fixer prompt can paste it
    /// verbatim into context.
    const B2FF102E_RUST_E0308_STDOUT: &str = r"   Compiling alphametics v1.3.0 (/Users/brentrager/.smooth/bench-runs/b2ff102e/work)
error[E0308]: mismatched types
  --> src/lib.rs:85:24
   |
85 |         mapping.insert(next_letter, digit);
   |                 ------ ^^^^^^^^^^^ expected `char`, found `&char`
   |                 |
   |                 arguments to this method are incorrect
   |
note: method defined here
  --> /rustc/01f6ddf7588f42ae2d7eb0a2f21d44e8e96674cf/library/std/src/collections/hash/map.rs:1207:12
help: consider dereferencing the borrow
   |
85 |         mapping.insert(*next_letter, digit);
   |                        +

For more information about this error, try `rustc --explain E0308`.
error: could not compile `alphametics` (lib) due to 1 previous error
warning: build failed, waiting for other jobs to finish...
error: could not compile `alphametics` (lib test) due to 1 previous error
";

    #[test]
    fn detect_compile_error_fixture_b2ff102e_e0308_includes_help_line() {
        let snippet = detect_compile_error(B2FF102E_RUST_E0308_STDOUT).expect("rustc E0308 must be detected as a compile error");
        // The whole point of forwarding this: the help: line literally
        // contains the fix. If our snippet doesn't carry it, the next
        // fixer iteration is no better off than the failed one.
        assert!(
            snippet.contains("help: consider dereferencing the borrow"),
            "snippet must include the rustc `help:` suggestion line; got:\n{snippet}"
        );
        assert!(
            snippet.contains("*next_letter") || snippet.contains("E0308"),
            "snippet must anchor near the actual error block (E0308 or its fix code), not at the trailing `could not compile` summary; got:\n{snippet}"
        );
    }

    #[test]
    fn detect_compile_error_python_type_error() {
        let pytest = "============================== ERRORS ===============================\n___ ERROR collecting test_x.py ___\nTypeError: unsupported operand type(s) for +: 'int' and 'str'\n";
        assert!(detect_compile_error(pytest).is_some());
    }

    #[test]
    fn detect_compile_error_java_cannot_find_symbol() {
        let javac = "src/Foo.java:42: error: cannot find symbol\n  symbol: variable bar\nlocation: class Foo\n";
        assert!(detect_compile_error(javac).is_some());
    }

    #[test]
    fn detect_compile_error_go_undefined() {
        let go = "./main.go:12:5: undefined: foo\n";
        assert!(detect_compile_error(go).is_some());
    }

    #[test]
    fn detect_compile_error_typescript() {
        let tsc = "src/foo.ts(12,3): error TS2304: Cannot find name 'bar'.\n";
        assert!(detect_compile_error(tsc).is_some());
    }

    #[test]
    fn detect_compile_error_picks_earliest_match_so_help_line_survives() {
        // Regression for the b2ff102e bug shape. A pattern list bug
        // (find_map first-match) anchored the snippet at "could not
        // compile" instead of "error[E0308]" — the help: line, which
        // appears between them, fell off the end of the 600-byte window.
        let snippet = detect_compile_error(B2FF102E_RUST_E0308_STDOUT).expect("must detect");
        let help_idx = snippet.find("help:").expect("help: must be in snippet");
        let cnc_idx = snippet.find("could not compile").unwrap_or(usize::MAX);
        assert!(
            help_idx < cnc_idx,
            "help: line must appear BEFORE the trailing summary in the snippet (help_idx={help_idx}, cnc_idx={cnc_idx})"
        );
    }

    #[test]
    fn build_fixer_prompt_uses_syntax_mode_on_compile_error() {
        let prior = B2FF102E_RUST_E0308_STDOUT;
        let p = build_fixer_prompt("solve alphametics", "1. parse\n2. solve\n", "watch leading zeros", Some(prior), 2);
        assert!(
            p.contains("does not compile") || p.contains("Compile error from prior attempt"),
            "fixer prompt must switch to syntax mode on compile error"
        );
        assert!(
            p.contains("help: consider dereferencing the borrow"),
            "verbatim rustc help: line must reach the fixer's next prompt"
        );
    }

    #[test]
    fn build_fixer_prompt_normal_failure_uses_test_output_section() {
        let prior = "test result: FAILED. 28 passed; 2 failed";
        let p = build_fixer_prompt("solve bowling", "plan", "advice", Some(prior), 2);
        assert!(p.contains("Last test output"));
        assert!(!p.contains("does not compile"));
    }

    #[test]
    fn last_diagnostic_text_combines_assistant_and_recent_tool_results() {
        use crate::conversation::{Conversation, Message};
        let mut conv = Conversation::new(8192);
        conv.messages.push(Message::user("do the thing"));
        conv.messages.push(Message::tool_result("call_1", "stale tool result"));
        conv.messages.push(Message::assistant("intermediate thought"));
        conv.messages.push(Message::tool_result(
            "call_2",
            "## CARGO TEST\nerror[E0308]: mismatched types\nhelp: consider dereferencing the borrow",
        ));
        conv.messages.push(Message::assistant("I attempted the fix; tests should pass now."));

        let view = last_diagnostic_text(&conv);
        // Last assistant message present.
        assert!(view.contains("I attempted the fix"), "must include last assistant message");
        // Recent tool result present so detect_compile_error can see it.
        assert!(view.contains("error[E0308]"), "must include recent tool-result content");
        assert!(view.contains("help: consider dereferencing the borrow"), "must include verbatim help line");
        // And detect_compile_error agrees.
        assert!(detect_compile_error(&view).is_some(), "compile error must be detected via combined view");
    }

    #[test]
    fn last_diagnostic_text_truncates_long_tool_result_via_head_tail() {
        use crate::conversation::{Conversation, Message};
        let mut conv = Conversation::new(8192);
        let middle_filler: String = "x".repeat(10_000);
        let huge = format!("error[E0308]: at start\n{middle_filler}\nhelp: at the end");
        conv.messages.push(Message::tool_result("call_1", huge));
        conv.messages.push(Message::assistant("done"));

        let view = last_diagnostic_text(&conv);
        // Both the head AND the tail of the tool result must survive
        // the truncation — that's the head/tail policy.
        assert!(view.contains("error[E0308]"), "head of tool result must survive");
        assert!(view.contains("help: at the end"), "tail of tool result must survive");
        assert!(view.contains("[truncated middle]"), "middle must be marked truncated");
    }

    #[test]
    fn head_tail_passes_short_strings_through_unchanged() {
        assert_eq!(head_tail("short", 100), "short");
    }

    #[test]
    fn head_tail_does_not_panic_on_multibyte_boundary() {
        // 200 grinning faces (4 bytes each); limit forces truncation
        // and the cut points must land on char boundaries — naive
        // byte-slicing here would panic.
        let s: String = "😀".repeat(200);
        let out = head_tail(&s, 100);
        // No panic is the real assertion. We don't assert "truncated"
        // here because at 200 chars / limit=100 bytes the function
        // may take the head+tail path or the early return depending
        // on `2 * (limit/2)` vs total char count. Either is correct.
        assert!(!out.is_empty());
    }

    #[test]
    fn snapshot_and_restore_roundtrip_preserves_non_excluded_entries() {
        let src = tempfile::tempdir().unwrap();
        let snap = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        std::fs::write(src.path().join("bowling.py"), b"BEST").unwrap();
        std::fs::create_dir_all(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub").join("nested.txt"), b"keep").unwrap();
        // Excluded: must NOT be copied.
        std::fs::create_dir_all(src.path().join("node_modules")).unwrap();
        std::fs::write(src.path().join("node_modules").join("pkg.json"), b"{}").unwrap();

        snapshot_workspace(src.path(), snap.path()).unwrap();
        assert!(snap.path().join("bowling.py").is_file());
        assert!(snap.path().join("sub").join("nested.txt").is_file());
        assert!(!snap.path().join("node_modules").exists());

        // Pollute dst with stale non-excluded content + excluded
        // content that must SURVIVE (node_modules caches).
        std::fs::write(dst.path().join("stale.py"), b"regressed").unwrap();
        std::fs::create_dir_all(dst.path().join("node_modules")).unwrap();
        std::fs::write(dst.path().join("node_modules").join("cache.json"), b"cached").unwrap();

        restore_workspace(snap.path(), dst.path()).unwrap();
        assert!(!dst.path().join("stale.py").exists());
        assert!(dst.path().join("bowling.py").is_file());
        assert_eq!(std::fs::read(dst.path().join("bowling.py")).unwrap(), b"BEST");
        assert!(
            dst.path().join("node_modules").join("cache.json").is_file(),
            "excluded cache must survive restore"
        );
    }

    #[test]
    fn best_snapshot_dir_uses_dotfile_name_so_test_runners_skip_it() {
        let snap = best_snapshot_dir(Path::new("/workspace"));
        assert_eq!(snap, Path::new("/workspace/.smooth-best-snapshot"));
        let name = snap.file_name().and_then(|s| s.to_str()).unwrap();
        assert!(name.starts_with('.'), "must be a dotfile for pytest/jest/cargo/gradle to skip");
    }
}

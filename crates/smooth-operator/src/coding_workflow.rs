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
pub async fn run_coding_workflow(cfg: CodingWorkflowConfig) -> anyhow::Result<f64> {
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
    // Pearl th-bench-loop iter 2: track NoEvidence retries. The
    // agent's first turn often skips the test run entirely (saw
    // 0 bash invocations in real bench runs). One retry with a
    // forcing prompt that demands an explicit test invocation
    // catches most of those before we give up.
    let mut no_evidence_retries: u32 = 0;
    const MAX_NO_EVIDENCE_RETRIES: u32 = 1;

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
        let mut conversation = agent.run_with_channel(user_prompt, cfg.tx.clone()).await?;

        let turn_cost = {
            let tracker = agent.cost_tracker.lock().expect("cost_tracker lock");
            tracker.total_cost_usd
        };
        total_cost_usd += turn_cost;

        // Pull the agent's final assistant message — used for
        // failure-context feedback into the next turn's prompt.
        let transcript = summarize_conversation(&conversation);
        last_verify_output = Some(transcript.clone());

        // Pearl th-7cf405 / th-ed7bfa: trust evidence, not claims.
        // The assistant's prose can fabricate "31 passed, 0 failed"
        // without ever running a test; only believe a tool-result
        // message produced by `bash` / `test_run`.
        let evidence = verify_with_evidence(&conversation);

        match evidence {
            VerifyEvidence::EvidencedPass => {
                succeeded = true;
                tracing::info!(iteration, "coding workflow: tool evidence shows green, stopping");
                break;
            }
            VerifyEvidence::EvidencedFail(_) => {
                // Stay in the loop and feed failure context forward.
            }
            VerifyEvidence::NoEvidence => {
                // No bash / test_run ever ran this turn. Two
                // possibilities: the task didn't require code (e.g.
                // a "what's this repo" routed here in error), OR
                // the agent silently skipped the test run despite
                // editing files. Pearl th-bench-loop iter 2 found
                // the latter is the dominant case on real benchmark
                // dispatches — the model returns plausible code +
                // narrative without ever running `cargo test`.
                //
                // First time: retry with a forcing prompt that
                // makes the test invocation non-negotiable. Only
                // give up if the agent ignores the forcing prompt
                // too. Capped at MAX_NO_EVIDENCE_RETRIES so a
                // genuinely non-test task can't trap the workflow.
                if no_evidence_retries < MAX_NO_EVIDENCE_RETRIES {
                    no_evidence_retries += 1;
                    tracing::info!(
                        iteration,
                        retry = no_evidence_retries,
                        "coding workflow: no test-run evidence — re-prompting with forcing directive"
                    );
                    last_verify_output = Some(
                        "Your previous turn edited the code but never ran the test suite. Before doing anything else this turn, run the project's test command via `bash` (cargo test / pytest / pnpm test / etc.) and report the actual output. The implementation is unverified until you do.".to_string(),
                    );
                    continue;
                }
                tracing::info!(iteration, "coding workflow: no test-run evidence after retry, exiting");
                if detect_verify_pass(&transcript) {
                    // Pearl iter-10/11: the assistant claimed pass
                    // without evidence. Three actions:
                    //
                    // 1. tracing::warn for log retention.
                    // 2. [cast-summary] stderr line — surfaced
                    //    by the runner stderr forward when
                    //    /verbose is on.
                    // 3. APPEND a TokenDelta to the live event
                    //    stream so the user sees the correction
                    //    INLINE in the streamed chat. The
                    //    streaming tokens already shipped — we
                    //    can't unsay them — but we can append a
                    //    correction the user sees alongside.
                    // 4. Mutate `conversation.messages` so saved
                    //    sessions don't preserve the lie either.
                    tracing::warn!(iteration, "coding workflow: assistant claimed pass with NO tool evidence — likely hallucinated");
                    eprintln!("[cast-summary] WARNING: assistant claimed test pass without evidence — no `bash` / `test_run` tool actually ran this turn.");
                    let correction = "\n\n---\n\n⚠️  **Correction:** the agent's `## Test Results` claim above is unverified — no `bash` / `test_run` tool actually ran this turn. The change above may be correct on its own merits but was not validated by the test suite. Run the tests yourself before trusting the result.\n";
                    let _ = cfg.tx.send(AgentEvent::TokenDelta { content: correction.into() });
                    redact_hallucinated_test_claims(&mut conversation);
                }
                break;
            }
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
        // likely to regress than close the gap.
        if let Some(best) = best_failed_count {
            if best <= CLOSE_TO_GREEN_THRESHOLD && !improved {
                tracing::info!(iteration, best_failed = best, "coding workflow: close to green, stopping before regression");
                break;
            }
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

/// Stop escalating when we're this close to green — more
/// iteration is more likely to regress than close the gap.
const CLOSE_TO_GREEN_THRESHOLD: u32 = 3;

// The coding system prompt lives in `crates/smooth-operator/src/cast/prompts/fixer.txt`
// and is loaded by `Cast::builtin()` via `include_str!`. The
// workflow resolves it at the top of `run_coding_workflow` so adding a
// new prompt-aware role there gives all call sites the same text.

/// Build the user-message prompt for a given outer iteration.
///
/// Pearl iter-7 finding: the iteration-1 prompt used to ALWAYS append
/// "Implement the solution, run the test suite, and iterate until
/// green." That framing actively pushed the model toward green-field
/// implementation regardless of what the user actually asked. "Make
/// App.tsx better" became "Make App.tsx better // Implement the
/// solution // iterate until green" → agent rewrote the whole file,
/// added main.tsx, overwrote tsconfig.json. Same shape on "delete the
/// src directory" → agent deleted, then re-implemented.
///
/// Now the iteration-1 prompt is the user's task verbatim. The fixer
/// system prompt already covers the "run the test suite before final
/// summary" discipline; we don't need to re-state it per turn at the
/// cost of confusing the model on non-test-driven tasks.
fn build_user_prompt(task: &str, iteration: u32, prior_output: Option<&str>) -> String {
    if iteration == 1 {
        return task.to_string();
    }
    let prior = prior_output.unwrap_or("(no prior output)");
    // Pearl th-bench-loop iter 2: the NoEvidence retry path
    // injects a synthetic "you didn't run tests" message into
    // prior_output. When we see that exact preamble, frame the
    // next turn as a verification-only nudge instead of the
    // standard fix-the-failures preamble — there were no
    // failures captured because no test ever ran.
    if prior.starts_with("Your previous turn edited the code but never ran the test suite.") {
        return format!("{prior}\n\n## Task (reminder)\n\n{task}");
    }
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

fn summarize_conversation(conv: &crate::conversation::Conversation) -> String {
    conv.messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, crate::conversation::Role::Assistant))
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

/// What the evidence in the conversation says about this turn —
/// not what the assistant *claims*. Pearl th-7cf405 / th-ed7bfa:
/// the workflow used to trust the assistant's `## Test Results: 31
/// passed, 0 failed` line verbatim, which made hallucinated
/// success indistinguishable from real success. We now require an
/// actual `bash` / `test_run` tool-result message in the
/// conversation whose output contains a recognizable test summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyEvidence {
    /// A tool actually ran and reported a green test suite.
    EvidencedPass,
    /// A tool actually ran and reported failures. `Some(n)` if a
    /// failure count was parseable; `None` if the output looked
    /// red but didn't include a count we could extract.
    EvidencedFail(Option<u32>),
    /// No bash / test_run tool call ever happened in this turn.
    /// The assistant either did nothing (silently passed text
    /// back) or hallucinated a result it never observed. Both are
    /// "no work was actually done" — caller decides whether to
    /// retry or exit gracefully.
    NoEvidence,
}

/// Strip fabricated "X passed, Y failed" / "ALL TESTS PASS"
/// claims from the last assistant message and replace with an
/// honest annotation. Pearl iter-10: emitting a stderr WARNING
/// alone wasn't enough — the lie still appeared verbatim in the
/// chat, so users could miss the warning and trust the false
/// claim. This rewrites the message itself.
///
/// Heuristic: look for the conventional `## Test Results` /
/// `Test Results` block at the end of the assistant prose and
/// replace its body. Also strip standalone count lines like
/// "31 passed, 0 failed" / "test result: ok. 5 passed; 0 failed".
pub fn redact_hallucinated_test_claims(conv: &mut crate::conversation::Conversation) {
    // Find the last assistant message — that's where the user-
    // visible final answer sits.
    let Some(msg) = conv.messages.iter_mut().rev().find(|m| matches!(m.role, crate::conversation::Role::Assistant)) else {
        return;
    };
    msg.content = redact_fabricated_test_results(&msg.content);
}

/// String-only version of the redactor — pulled out for tests.
/// Pure function so the unit suite can pin every shape we know
/// the model produces.
#[must_use]
pub fn redact_fabricated_test_results(content: &str) -> String {
    const NOTICE: &str = "⚠️  Test Results: NOT RUN — the agent did not actually execute the test suite this turn. The change above may be correct but is unverified. Run the tests yourself before trusting it.";

    // Strip "X passed, Y failed" / "X passed; Y failed" lines and
    // replace the "## Test Results" block at the tail. Patterns:
    //   - "## Test Results\n\n31 passed, 0 failed"
    //   - "## Test Results\n\nALL TESTS PASS"
    //   - "Test Results: 31 passed, 0 failed"
    //   - bare "31 passed, 0 failed" line at end of content
    let lines: Vec<&str> = content.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 2);
    let mut in_test_results_block = false;
    let mut redacted_block = false;
    for line in &lines {
        let trimmed = line.trim();
        // Heading variants.
        let is_heading =
            trimmed.eq_ignore_ascii_case("## test results") || trimmed.eq_ignore_ascii_case("test results") || trimmed.eq_ignore_ascii_case("# test results");
        if is_heading && !redacted_block {
            in_test_results_block = true;
            out.push(NOTICE.to_string());
            redacted_block = true;
            continue;
        }
        if in_test_results_block {
            // Continue swallowing lines until a new heading
            // (`## ...`) starts a different section.
            if trimmed.starts_with("## ") || trimmed.starts_with("# ") {
                in_test_results_block = false;
                out.push((*line).to_string());
                continue;
            }
            // Drop content inside the block.
            continue;
        }
        // Bare "X passed, Y failed" / "X passed; Y failed" / "ALL TESTS PASS" lines.
        let upper = trimmed.to_ascii_uppercase();
        let looks_like_count = (trimmed.contains("passed, ") || trimmed.contains("passed; ") || trimmed.contains("PASSED, ") || trimmed.contains("PASSED; "))
            && (trimmed.contains("failed") || trimmed.contains("FAILED"));
        let looks_like_marker = upper.contains("ALL TESTS PASS") || upper == "TEST RESULT: OK";
        if looks_like_count || looks_like_marker {
            // Replace with a one-line redaction marker. Append
            // the full notice once if we haven't already (e.g.
            // when there's no "## Test Results" heading).
            if !redacted_block {
                out.push(NOTICE.to_string());
                redacted_block = true;
            }
            continue;
        }
        out.push((*line).to_string());
    }
    let result = out.join("\n");
    // Edge case: content didn't have a heading or count line
    // pattern but still looked green to detect_verify_pass (rare;
    // happens when the model uses idiomatic phrasing like "all
    // tests pass" embedded in prose). In that case, append the
    // notice at the end so the reader at least sees the warning.
    if !redacted_block && detect_verify_pass(content) {
        return format!("{result}\n\n{NOTICE}");
    }
    result
}

/// Inspect the conversation for tool-result evidence of test
/// outcomes. Walks tool-role messages in order and returns the
/// LAST shaped result — later tool runs win, since the agent
/// often runs the suite multiple times in one turn.
pub fn verify_with_evidence(conv: &crate::conversation::Conversation) -> VerifyEvidence {
    let mut last_outcome = VerifyEvidence::NoEvidence;
    for msg in &conv.messages {
        if !matches!(msg.role, crate::conversation::Role::Tool) {
            continue;
        }
        // Only test-shaped tools produce evidence we believe.
        // `bash` is the catch-all (the agent runs `pnpm test` /
        // `cargo test` / `pytest` through it). `test_run` is the
        // workflow's structured test tool when present. Other
        // tool outputs (read_file, list_files, grep) don't count
        // even if the user happened to grep for "PASS" somewhere.
        let name = msg.tool_name.as_deref().unwrap_or("");
        if name != "bash" && name != "test_run" && name != "shell" {
            continue;
        }
        // Pearl th-bench-loop iter 13: "all tests skipped, 0 ran"
        // is NOT a pass. Exercism JS uses `xtest()` and Java uses
        // `@Disabled`; both default to skip and require the student
        // to flip annotations as they implement. The agent ships
        // implementations that look correct, the test runner returns
        // 0 ran/0 failed (exit code 0), and the workflow used to
        // call that a pass. It's not — nothing actually ran.
        //
        // Detect BEFORE detect_verify_pass since the skip-only
        // output may also coincidentally match "0 failed" patterns.
        if looks_all_skipped(&msg.content) {
            last_outcome = VerifyEvidence::EvidencedFail(None);
            continue;
        }
        if detect_verify_pass(&msg.content) {
            last_outcome = VerifyEvidence::EvidencedPass;
            continue;
        }
        // Look for explicit failure shapes. We reuse
        // `nonzero_failure_count` so all the same patterns the
        // pass-detection guards against count as fail signals.
        let upper = msg.content.to_uppercase();
        let looks_red =
            upper.contains("TEST RESULT: FAILED") || upper.contains("TESTS FAILED") || upper.contains("TESTS FAIL") || nonzero_failure_count(&upper);
        if looks_red {
            last_outcome = VerifyEvidence::EvidencedFail(extract_failed_count(&msg.content));
        }
        // Otherwise leave last_outcome as-is — this tool call
        // wasn't a test, or didn't produce a recognizable summary.
    }
    last_outcome
}

/// True when test output indicates EVERY test was skipped — common
/// when an exercism framework defaults to `@Disabled` / `xtest()` /
/// `test.skip` and the student hasn't flipped them yet. Treat as
/// failure-no-evidence (pearl th-bench-loop iter 13): 0 tests
/// actually ran, the implementation is unverified.
///
/// Heuristics (all case-insensitive on uppercase input):
///   - Jest: "Tests:       N skipped, 0 passed, N total"
///   - Gradle/JUnit: "BUILD SUCCESSFUL" + "N tests completed, N skipped"
///     OR all "SKIPPED" markers with no "PASSED" / "FAILED" lines
///   - pytest: "N skipped" alongside "0 passed"
///   - go test: "ok ... [no tests to run]" (Go has no skip annotation
///     by default, but the no-tests case is the same problem)
pub fn looks_all_skipped(transcript: &str) -> bool {
    let upper = transcript.to_uppercase();

    // Gradle/JUnit: count of SKIPPED markers as inline test
    // outcomes. Check FIRST because gradle lines don't have a
    // numeric prefix the pytest-shape path would expect.
    let skipped_lines = upper.lines().filter(|l| l.trim_end().ends_with("SKIPPED")).count();
    let pass_lines = upper.lines().filter(|l| l.trim_end().ends_with("PASSED")).count();
    let fail_lines = upper.lines().filter(|l| l.trim_end().ends_with("FAILED")).count();
    if skipped_lines >= 3 && pass_lines == 0 && fail_lines == 0 {
        return true;
    }

    // Jest / pytest shape: explicit "N skipped, 0 passed".
    if (upper.contains("0 PASSED") || upper.contains(" 0 PASSED,") || upper.contains(", 0 PASSED")) && upper.contains("SKIPPED") {
        return true;
    }

    // Go: "no tests to run" + ok status.
    if upper.contains("[NO TESTS TO RUN]") {
        return true;
    }

    // Pytest: "N skipped" with no "passed" count at all. Last
    // because the digit-prefix check is strict — wouldn't catch
    // gradle's per-line shape, only pytest's summary count.
    if upper.contains(" SKIPPED") && !upper.contains(" PASSED") && !upper.contains(" FAILED") {
        return has_count_before(&upper, "SKIPPED");
    }

    false
}

/// True when `needle` is preceded by a digit (possibly with
/// whitespace) somewhere in `haystack`. Used by `looks_all_skipped`
/// to distinguish a count line (`10 SKIPPED`) from a comment
/// ("# this section is skipped").
fn has_count_before(haystack: &str, needle: &str) -> bool {
    let mut search = haystack;
    while let Some(idx) = search.find(needle) {
        let before = &search[..idx];
        let digits: String = before
            .chars()
            .rev()
            .skip_while(|c| c.is_whitespace())
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>();
        if let Ok(n) = digits.chars().rev().collect::<String>().parse::<u32>() {
            if n > 0 {
                return true;
            }
        }
        search = &search[idx + needle.len()..];
    }
    false
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
/// red-test run. Used by `build_user_prompt` to switch retry
/// tone from "fix the failures" to "fix the syntax".
fn detect_compile_error(transcript: &str) -> Option<String> {
    let upper = transcript.to_uppercase();
    let patterns = [
        // JS / TS
        "SYNTAXERROR",
        "UNEXPECTED TOKEN",
        "MISSING SEMICOLON",
        "UNCLOSED DELIMITER",
        "UNEXPECTED EOF",
        // Rust
        "COULD NOT COMPILE",
        "THIS FILE CONTAINS AN UNCLOSED DELIMITER",
        "EXPECTED ONE OF",
        // Go
        "SYNTAX ERROR:",
        "EXPECTED '{'",
        "EXPECTED ';'",
        // Python
        "INDENTATIONERROR",
        "TABERROR",
        // Java
        "REACHED END OF FILE",
        "';' EXPECTED",
        "CLASS, INTERFACE, OR ENUM EXPECTED",
        "ERROR: COMPILATION FAILED",
    ];
    let hit_idx = patterns.iter().find_map(|p| upper.find(p))?;
    let bytes_per_char = transcript.len().checked_div(upper.len()).unwrap_or(1).max(1);
    let start = hit_idx.saturating_mul(bytes_per_char).saturating_sub(120);
    let end = (hit_idx.saturating_mul(bytes_per_char).saturating_add(600)).min(transcript.len());
    let snippet = transcript.get(start..end).unwrap_or(transcript);
    Some(snippet.trim().to_string())
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

    fn make_conv() -> crate::conversation::Conversation {
        crate::conversation::Conversation::new(8192).with_system_prompt("test")
    }

    #[test]
    fn redact_replaces_hash_test_results_block() {
        let input = "I made the change.\n\n## Test Results\n\n31 passed, 0 failed";
        let out = redact_fabricated_test_results(input);
        assert!(!out.contains("31 passed"), "fabricated count must be redacted: {out}");
        assert!(out.contains("NOT RUN"));
        assert!(out.contains("I made the change."));
    }

    #[test]
    fn redact_replaces_bare_count_line() {
        let input = "Fixed the bug.\n\n5 passed, 0 failed";
        let out = redact_fabricated_test_results(input);
        assert!(!out.contains("5 passed, 0 failed"));
        assert!(out.contains("NOT RUN"));
    }

    #[test]
    fn redact_preserves_following_section() {
        // A "## Notes" heading after Test Results must survive.
        let input = "Did the work.\n\n## Test Results\n\n31 passed, 0 failed\n\n## Notes\n\nbe careful with edge cases.";
        let out = redact_fabricated_test_results(input);
        assert!(out.contains("be careful with edge cases"));
        assert!(out.contains("## Notes"));
        assert!(out.contains("NOT RUN"));
    }

    #[test]
    fn redact_no_op_when_content_has_no_test_claims() {
        let input = "I read the file. It looks fine.";
        let out = redact_fabricated_test_results(input);
        assert_eq!(out, input);
    }

    #[test]
    fn redact_appends_notice_when_only_idiomatic_marker_present() {
        // No heading, no "X passed" line, but content reads as
        // green per detect_verify_pass.
        let input = "I've finished. ALL TESTS PASS now.";
        let out = redact_fabricated_test_results(input);
        // The marker was on a line containing other text — current
        // implementation matches whole-line variants only, so this
        // exercises the trailing-append fallback.
        assert!(out.contains("NOT RUN"));
    }

    #[test]
    fn verify_with_evidence_no_tool_calls_returns_no_evidence() {
        // Pearl th-7cf405: a turn with no bash / test_run tool
        // results, even if the assistant claims pass, must NOT
        // count as evidence.
        let mut conv = make_conv();
        conv.push(crate::conversation::Message::user("can we commit to main"));
        conv.push(crate::conversation::Message::assistant("## Test Results\n\n31 passed, 0 failed"));
        assert_eq!(verify_with_evidence(&conv), VerifyEvidence::NoEvidence);
    }

    #[test]
    fn verify_with_evidence_evidenced_pass_via_bash_tool() {
        let mut conv = make_conv();
        conv.push(crate::conversation::Message::user("fix the failing test"));
        conv.push(crate::conversation::Message::tool_result_named(
            "call-1",
            "bash",
            "test result: ok. 5 passed; 0 failed;",
        ));
        conv.push(crate::conversation::Message::assistant("Done."));
        assert_eq!(verify_with_evidence(&conv), VerifyEvidence::EvidencedPass);
    }

    #[test]
    fn verify_with_evidence_evidenced_fail_via_test_run() {
        let mut conv = make_conv();
        conv.push(crate::conversation::Message::user("fix the failing test"));
        conv.push(crate::conversation::Message::tool_result_named(
            "call-1",
            "test_run",
            "Tests: 2 failed, 5 passed",
        ));
        conv.push(crate::conversation::Message::assistant("Working on it."));
        assert_eq!(verify_with_evidence(&conv), VerifyEvidence::EvidencedFail(Some(2)));
    }

    #[test]
    fn verify_with_evidence_ignores_non_test_tool_outputs() {
        // read_file or list_files outputs that happen to contain
        // "passed" must not count as test evidence — agents read
        // README files etc all the time.
        let mut conv = make_conv();
        conv.push(crate::conversation::Message::user("what does this repo do"));
        conv.push(crate::conversation::Message::tool_result_named(
            "call-1",
            "read_file",
            "## Test Status\n\nAll 31 tests pass.",
        ));
        conv.push(crate::conversation::Message::assistant("It's a budgeting app."));
        assert_eq!(verify_with_evidence(&conv), VerifyEvidence::NoEvidence);
    }

    #[test]
    fn looks_all_skipped_jest_shape() {
        // Pearl th-bench-loop iter 13: jest output when all tests
        // are xtest. We saw this on iter 5 javascript/binary.
        let out = "Test Suites: 1 passed, 1 total\nTests: 9 skipped, 1 passed, 10 total\nSnapshots: 0 total";
        // Has 1 passed too so it's not ALL-skipped. Detector must be
        // careful about partial-skip cases.
        assert!(!looks_all_skipped(out), "must not trigger on 9 skipped + 1 passed");
    }

    #[test]
    fn looks_all_skipped_jest_pure_skip() {
        // Pure skip shape: 10 skipped, 0 passed.
        let out = "Tests:       10 skipped, 0 passed, 10 total";
        assert!(looks_all_skipped(out), "must trigger on all-skipped jest output");
    }

    #[test]
    fn looks_all_skipped_gradle_disabled() {
        // Iter 10 java/change shape. Gradle prints one line per test
        // with SKIPPED suffix when @Disabled.
        let out = r"ChangeCalculatorTest > testLilliputianCurrency() SKIPPED
ChangeCalculatorTest > testLargeAmountOfChange() SKIPPED
ChangeCalculatorTest > testZeroChange() SKIPPED
ChangeCalculatorTest > testAGreedyApproachIsNotOptimal() SKIPPED";
        assert!(looks_all_skipped(out), "must trigger on multi-line gradle SKIPPED output");
    }

    #[test]
    fn looks_all_skipped_does_not_false_positive_on_mixed() {
        // Mixed pass+skip = NOT all-skipped.
        let out = r"FooTest > testOne PASSED
FooTest > testTwo SKIPPED
FooTest > testThree PASSED";
        assert!(!looks_all_skipped(out), "must not trigger when some tests passed");
    }

    #[test]
    fn looks_all_skipped_no_tests_to_run() {
        let out = "ok      myproject  [no tests to run]";
        assert!(looks_all_skipped(out), "must trigger on go's 'no tests to run'");
    }

    #[test]
    fn looks_all_skipped_does_not_trigger_on_normal_pass() {
        let out = "test result: ok. 31 passed; 0 failed";
        assert!(!looks_all_skipped(out), "must not trigger on a real green run");
    }

    #[test]
    fn looks_all_skipped_does_not_trigger_on_skipped_comment() {
        // The word "skipped" appearing in prose without a count
        // should not trigger.
        let out = "Looking at the codebase, I notice this section is skipped.";
        assert!(!looks_all_skipped(out), "must not trigger on prose-only 'skipped'");
    }

    #[test]
    fn verify_with_evidence_returns_fail_on_all_skipped() {
        let mut conv = make_conv();
        conv.push(crate::conversation::Message::user("implement it"));
        conv.push(crate::conversation::Message::tool_result_named(
            "call-1",
            "bash",
            "Tests: 10 skipped, 0 passed, 10 total",
        ));
        conv.push(crate::conversation::Message::assistant("Done."));
        let evidence = verify_with_evidence(&conv);
        assert_eq!(evidence, VerifyEvidence::EvidencedFail(None), "all-skipped must register as fail, not pass");
    }

    #[test]
    fn verify_with_evidence_uses_last_test_run() {
        // Multiple test runs in one turn — the last one wins
        // (the agent often runs the suite, fixes, runs again).
        let mut conv = make_conv();
        conv.push(crate::conversation::Message::user("fix it"));
        conv.push(crate::conversation::Message::tool_result_named("call-1", "bash", "Tests: 3 failed, 28 passed"));
        conv.push(crate::conversation::Message::tool_result_named(
            "call-2",
            "bash",
            "test result: ok. 31 passed; 0 failed;",
        ));
        conv.push(crate::conversation::Message::assistant("Fixed."));
        assert_eq!(verify_with_evidence(&conv), VerifyEvidence::EvidencedPass);
    }

    #[test]
    fn build_user_prompt_no_evidence_retry_frames_as_verification_only() {
        // Pearl th-bench-loop iter 2: when the NoEvidence retry path
        // injects the forcing preamble into prior_output, the next
        // turn's prompt must NOT prepend the standard
        // "Your previous attempt left tests failing" preamble (it's
        // not true — no tests ran). It should land as a clean
        // verification nudge with the task reminder attached.
        let prior = "Your previous turn edited the code but never ran the test suite. Before doing anything else this turn, run the project's test command via `bash` (cargo test / pytest / pnpm test / etc.) and report the actual output. The implementation is unverified until you do.";
        let out = build_user_prompt("Implement the leap function.", 2, Some(prior));
        assert!(out.contains("never ran the test suite"), "forcing preamble must be present");
        assert!(out.contains("## Task (reminder)"), "task reminder must be attached");
        assert!(out.contains("Implement the leap function."), "original task must be present");
        // Critical: the standard fix-failures preamble must NOT appear.
        assert!(!out.contains("Previous test output"), "must not include the fail-recovery preamble");
        assert!(!out.contains("currently passing"), "must not include preserve-passing preamble");
    }

    #[test]
    fn build_user_prompt_first_iter_is_task_verbatim() {
        // Pearl iter-7: the iteration-1 prompt must NOT prepend
        // "Implement the solution, run the test suite, iterate until
        // green" — that framing pushed the model toward green-field
        // implementation on non-test-driven asks ("make X better"
        // → rewrote the file). The user's task flows verbatim; the
        // fixer system prompt covers test-running discipline.
        let p = build_user_prompt("solve bowling", 1, None);
        assert_eq!(p, "solve bowling");
        assert!(!p.contains("Implement the solution"), "iter 1 must not push 'Implement' framing");
        assert!(!p.contains("iterate until green"), "iter 1 must not push 'iterate until green' framing");
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

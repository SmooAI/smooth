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

use crate::agent::{Agent, AgentConfig, AgentEvent};
use crate::agents::AgentRegistry;
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
}

/// Run the workflow end-to-end. Returns the accumulated cost.
pub async fn run_coding_workflow(cfg: CodingWorkflowConfig) -> anyhow::Result<f64> {
    // Pull the coding agent definition from the registry so the
    // prompt lives in one place (`agents/prompts/code.txt`) and the
    // slot comes from the agent's `slot` field instead of being
    // hard-coded here. The `code` agent is always present in
    // `AgentRegistry::builtin()` — if it ever isn't, something is
    // badly wrong and we want a loud failure, not a silent fallback.
    let agents = AgentRegistry::builtin();
    let code_agent = agents
        .get("code")
        .context("missing 'code' agent in registry — did AgentRegistry::builtin change?")?;
    let code_prompt = code_agent.prompt.clone();
    let code_slot = code_agent.slot;

    let llm_config = cfg.registry.llm_config_for(code_slot).context("resolving coding slot → LLM config")?;
    let coding_slot = cfg.registry.routing.slot_for(code_slot);
    let alias = coding_slot.model.clone();

    let mut total_cost_usd = 0.0_f64;
    let mut last_verify_output: Option<String> = None;
    let mut best_failed_count: Option<u32> = None;
    let mut snapshot_taken = false;

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

        let mut agent_config =
            AgentConfig::new(format!("{}/coding-{}", cfg.operator_id, iteration), code_prompt.clone(), llm_config.clone()).with_max_iterations(80); // agent can take a lot of tool-call turns internally
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

        // Pull the agent's final assistant message — it usually
        // contains a summary of what it did plus the last test
        // result. We parse THIS for pass/fail and failure detail.
        let transcript = summarize_conversation(&conversation);
        last_verify_output = Some(transcript.clone());

        // Green? We're done.
        if detect_verify_pass(&transcript) {
            succeeded = true;
            tracing::info!(iteration, "coding workflow: agent reports green, stopping");
            break;
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

// The coding system prompt lives in `crates/smooth-operator/src/agents/prompts/code.txt`
// and is loaded by `AgentRegistry::builtin()` via `include_str!`. The
// workflow resolves it at the top of `run_coding_workflow` so adding a
// new prompt-aware agent there gives all call sites the same text.

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

fn summarize_conversation(conv: &crate::conversation::Conversation) -> String {
    conv.messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, crate::conversation::Role::Assistant))
        .map(|m| m.content.clone())
        .unwrap_or_default()
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

fn snapshot_workspace(src: &Path, dst: &Path) -> std::io::Result<()> {
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

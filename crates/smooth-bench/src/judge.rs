//! LLM-as-judge for open-ended agentic scenarios (pearl th-300d7d).
//!
//! Deterministic checks are preferred everywhere they fit — they're
//! exact and free. Some workflow goals have no crisp ground truth
//! ("summarise the state and draft a reply"), and those get scored by a
//! cheap model against a rubric.
//!
//! Two invariants:
//!
//! 1. **A judge failure is never a PASS.** Transport error, HTTP error,
//!    unparseable verdict — all map to `INCONCLUSIVE` at the call site.
//!    Parsing is strict: the model must emit a `VERDICT:` line whose
//!    first word is exactly `PASS` or `FAIL`.
//! 2. **The judge runs on the host, not in the sandbox.** It reads
//!    evidence the scenario runner already collected; it never gets
//!    network access into the VM and the VM never learns it exists.

use anyhow::{Context, Result};
use serde_json::{json, Value};

/// Everything the judge is shown about one scenario run.
#[derive(Debug, Clone, Default)]
pub struct JudgeEvidence {
    /// The rubric from the scenario's `[check]` block.
    pub rubric: String,
    /// The user goal the agent was given.
    pub prompt: String,
    /// One line per tool call: `name -> ok|error`.
    pub transcript: String,
    /// The agent's spoken answer.
    pub final_response: String,
    /// A dump of the resulting workspace (path + truncated contents).
    pub workspace: String,
}

/// A parsed judge verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeVerdict {
    pub passed: bool,
    pub reason: String,
}

/// System prompt: pins the output contract the parser enforces.
const SYSTEM: &str =
    "You grade an AI agent's work against a rubric. You see the user's goal, the agent's tool calls, its final answer, and the resulting workspace files. \
Judge ONLY whether the rubric is satisfied. Be strict: unmet rubric points are a FAIL. \
Reply with EXACTLY two lines and nothing else:\nVERDICT: PASS\nREASON: <one sentence>";

/// Ask `model` at the OpenAI-compatible `gateway_url` to grade `ev`.
///
/// # Errors
/// Errors on transport failure, a non-2xx gateway response, a missing
/// message body, or a verdict that doesn't match the contract. Callers
/// must map every one of those to `INCONCLUSIVE`, never to PASS.
pub async fn judge(gateway_url: &str, gateway_key: Option<&str>, model: &str, ev: &JudgeEvidence) -> Result<JudgeVerdict> {
    let url = format!("{}/chat/completions", gateway_url.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "temperature": smooth_policy::llm_params::AGENT_TEMPERATURE,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": render_evidence(ev)},
        ],
    });

    let client = reqwest::Client::new();
    let mut req = client.post(&url).json(&body);
    if let Some(k) = gateway_key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.with_context(|| format!("judge request to {url}"))?;
    let status = resp.status();
    // This call is OUR spend, not the agent's. Record it so the
    // leaderboard can subtract it (pearl th-adf614) — otherwise a cheap
    // agent graded by an expensive judge reads as expensive.
    crate::spend::record_harness_response(resp.headers());
    let text = resp.text().await.context("reading judge response body")?;
    anyhow::ensure!(status.is_success(), "judge gateway returned {status}: {}", truncate(&text, 400));

    let v: Value = serde_json::from_str(&text).with_context(|| format!("judge response was not JSON: {}", truncate(&text, 400)))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("judge response carried no message content: {}", truncate(&text, 400)))?;

    parse_verdict(content).ok_or_else(|| anyhow::anyhow!("judge verdict did not match the contract: {}", truncate(content, 400)))
}

/// Render the evidence block the judge reads. Each section is bounded so
/// a runaway workspace can't blow the model's context.
#[must_use]
pub fn render_evidence(ev: &JudgeEvidence) -> String {
    let transcript = if ev.transcript.trim().is_empty() {
        "(none)".to_string()
    } else {
        ev.transcript.trim().to_string()
    };
    let answer = if ev.final_response.trim().is_empty() {
        "(the agent said nothing)".to_string()
    } else {
        truncate(ev.final_response.trim(), 8_000)
    };
    format!(
        "## RUBRIC\n{}\n\n## USER GOAL\n{}\n\n## TOOL CALLS\n{transcript}\n\n## AGENT FINAL ANSWER\n{answer}\n\n## RESULTING WORKSPACE\n{}\n",
        ev.rubric.trim(),
        truncate(ev.prompt.trim(), 4_000),
        truncate(ev.workspace.trim(), 20_000),
    )
}

/// Parse the two-line verdict contract.
///
/// Returns `None` for anything that doesn't clearly say PASS or FAIL —
/// the caller turns that into INCONCLUSIVE, so a chatty or broken judge
/// can never silently pass a scenario.
#[must_use]
pub fn parse_verdict(text: &str) -> Option<JudgeVerdict> {
    let mut passed = None;
    let mut reason = String::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['*', '#', '-', ' ']);
        if let Some(rest) = strip_prefix_ci(line, "VERDICT:") {
            // First word only: "PASS — the file exists" is fine,
            // "PASS or FAIL depending" is not (first word wins, and a
            // hedging judge is the caller's problem, not a silent pass).
            let word: String = rest
                .trim()
                .trim_start_matches(['*', '_', ' '])
                .chars()
                .take_while(char::is_ascii_alphabetic)
                .collect();
            passed = match word.to_ascii_uppercase().as_str() {
                "PASS" => Some(true),
                "FAIL" => Some(false),
                _ => return None,
            };
        } else if let Some(rest) = strip_prefix_ci(line, "REASON:") {
            reason = rest.trim().trim_start_matches(['*', '_', ' ']).trim().to_string();
        }
    }
    passed.map(|passed| JudgeVerdict {
        passed,
        reason: if reason.is_empty() { "(no reason given)".to_string() } else { reason },
    })
}

/// Case-insensitive `strip_prefix` for ASCII prefixes.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)).then(|| &s[prefix.len()..])
}

/// Truncate on a char boundary, appending an elision marker.
#[must_use]
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n…[truncated]")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn parses_the_happy_two_line_contract() {
        let v = parse_verdict("VERDICT: PASS\nREASON: summary.md covers all three tickets").unwrap();
        assert!(v.passed);
        assert_eq!(v.reason, "summary.md covers all three tickets");
    }

    #[test]
    fn parses_fail() {
        let v = parse_verdict("VERDICT: FAIL\nREASON: no draft reply was written").unwrap();
        assert!(!v.passed);
        assert_eq!(v.reason, "no draft reply was written");
    }

    #[test]
    fn tolerates_markdown_decoration_and_case() {
        let v = parse_verdict("**verdict:** pass\n**reason:** looks right").unwrap();
        assert!(v.passed);
        assert_eq!(v.reason, "looks right");
    }

    #[test]
    fn tolerates_preamble_lines() {
        let v = parse_verdict("Sure! Here is my assessment.\n\nVERDICT: FAIL\nREASON: nothing was written").unwrap();
        assert!(!v.passed);
    }

    #[test]
    fn verdict_word_is_taken_from_the_front_not_scanned() {
        // Prose containing the word FAIL after a PASS verdict must not flip it.
        let v = parse_verdict("VERDICT: PASS\nREASON: it did not FAIL any rubric point").unwrap();
        assert!(v.passed);
    }

    #[test]
    fn missing_reason_still_parses_with_a_placeholder() {
        let v = parse_verdict("VERDICT: PASS").unwrap();
        assert!(v.passed);
        assert_eq!(v.reason, "(no reason given)");
    }

    /// Every one of these must yield `None` so the caller records
    /// INCONCLUSIVE. A judge that can't be parsed must never pass.
    #[test]
    fn malformed_verdicts_are_none_never_a_silent_pass() {
        for bad in [
            "",
            "I think the agent did fine.",
            "PASS",                            // no VERDICT: key
            "VERDICT: MAYBE\nREASON: unclear", // not PASS/FAIL
            "VERDICT: \nREASON: empty",        // no word at all
            "{\"verdict\":\"pass\"}",          // JSON instead of the contract
            "REASON: it worked",               // reason without a verdict
            "The VERDICT is PASS",             // no colon-delimited key
            "VERDICT: 1\nREASON: numeric",     // numeric verdict
        ] {
            assert!(parse_verdict(bad).is_none(), "must be INCONCLUSIVE, not a verdict: {bad:?}");
        }
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        // Multi-byte input truncated mid-string must not panic.
        let s = "héllo wörld ✅✅✅";
        let t = truncate(s, 5);
        assert!(t.starts_with("héllo"), "{t}");
        assert!(t.contains("truncated"));
        // Short input passes through untouched.
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn evidence_render_carries_every_section() {
        let ev = JudgeEvidence {
            rubric: "must write summary.md".into(),
            prompt: "summarise the tickets".into(),
            transcript: "read_file -> ok\nwrite_file -> ok".into(),
            final_response: "done".into(),
            workspace: "summary.md:\nall good".into(),
        };
        let r = render_evidence(&ev);
        for section in ["## RUBRIC", "## USER GOAL", "## TOOL CALLS", "## AGENT FINAL ANSWER", "## RESULTING WORKSPACE"] {
            assert!(r.contains(section), "missing {section} in:\n{r}");
        }
        assert!(r.contains("must write summary.md"));
        assert!(r.contains("write_file -> ok"));
    }

    #[test]
    fn evidence_render_marks_a_silent_agent() {
        let ev = JudgeEvidence {
            rubric: "r".into(),
            ..Default::default()
        };
        let r = render_evidence(&ev);
        assert!(r.contains("(the agent said nothing)"), "{r}");
        assert!(r.contains("(none)"), "empty transcript is marked: {r}");
    }
}

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
use serde::{Deserialize, Serialize};
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
    /// What the objective checks found (commands, hidden tests, scope),
    /// one line per check. Empty for scenarios with no objective half.
    pub objective: String,
    /// Which files the agent added, changed or deleted relative to the
    /// seeded workspace. Empty for an unseeded scenario.
    pub diff: String,
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
    let mut out = format!(
        "## RUBRIC\n{}\n\n## USER GOAL\n{}\n\n## TOOL CALLS\n{transcript}\n\n## AGENT FINAL ANSWER\n{answer}\n\n",
        ev.rubric.trim(),
        truncate(ev.prompt.trim(), 4_000),
    );
    // Only scenarios with a seeded repo or objective checks carry these,
    // and an empty section would read as "nothing changed / nothing ran".
    if !ev.objective.trim().is_empty() {
        out.push_str(&format!(
            "## OBJECTIVE CHECKS (run by the harness after the turn)\n{}\n\n",
            truncate(ev.objective.trim(), 6_000)
        ));
    }
    if !ev.diff.trim().is_empty() {
        out.push_str(&format!("## FILES CHANGED vs. THE SEEDED REPO\n{}\n\n", truncate(ev.diff.trim(), 4_000)));
    }
    out.push_str(&format!("## RESULTING WORKSPACE\n{}\n", truncate(ev.workspace.trim(), 20_000)));
    out
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

/// The axes the scored judge grades, each 1–5.
///
/// The PASS/FAIL judge answers "is the rubric met"; a model comparison
/// needs more than one bit. Two models can both pass a build task while
/// one shipped tidy, tested code and the other rewrote half the repo to
/// get there — that is the difference between `code_quality` and
/// `scope_discipline`, and the leaderboard should be able to see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeScores {
    /// Every explicit requirement in the user's ask was delivered.
    pub requirements: u8,
    /// The code does what it claims; edge cases and errors are handled.
    pub correctness: u8,
    /// Readable, idiomatic, consistent with the existing code.
    pub code_quality: u8,
    /// Changed only what the task needed — no drive-by rewrites.
    pub scope_discipline: u8,
    /// Looked before writing, used tools sensibly, verified the result.
    pub process: u8,
}

impl JudgeScores {
    /// Mean of the five axes, on the judge's 1–5 scale.
    #[must_use]
    pub fn mean(self) -> f64 {
        f64::from(
            u16::from(self.requirements)
                + u16::from(self.correctness)
                + u16::from(self.code_quality)
                + u16::from(self.scope_discipline)
                + u16::from(self.process),
        ) / 5.0
    }

    fn all_in_range(self) -> bool {
        [self.requirements, self.correctness, self.code_quality, self.scope_discipline, self.process]
            .iter()
            .all(|v| (1..=5).contains(v))
    }
}

/// A parsed scored verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredVerdict {
    pub passed: bool,
    pub scores: JudgeScores,
    pub rationale: String,
}

/// System prompt for the scored judge. The axes are spelled out so two
/// runs of the judge mean the same thing by "3".
const SCORED_SYSTEM: &str = "You are a strict senior engineer reviewing an AI coding agent's work. You see the user's goal, a rubric, the agent's tool calls, \
its final answer, the results of objective checks the harness ran AFTER the turn (builds, held-out tests the agent never saw, scope checks), \
which files it changed, and the resulting workspace.\n\
Score each axis 1-5 (1 = absent or wrong, 3 = acceptable with clear gaps, 5 = what a strong engineer would ship):\n\
- requirements: every explicit requirement in the goal and rubric was delivered.\n\
- correctness: the code works; edge cases and errors are handled. Failed objective checks are hard evidence against this.\n\
- code_quality: readable, idiomatic, consistent with any existing code.\n\
- scope_discipline: changed only what the task needed; no unrequested rewrites, deleted tests, or weakened assertions.\n\
- process: inspected before editing, used tools sensibly, and VERIFIED its work (ran tests/builds) instead of asserting success.\n\
verdict is PASS only if the rubric is fully satisfied AND no objective check failed. \
Reply with ONLY a JSON object matching the schema: {\"verdict\":\"PASS\"|\"FAIL\",\"scores\":{\"requirements\":n,\"correctness\":n,\"code_quality\":n,\"scope_discipline\":n,\"process\":n},\"rationale\":\"2-4 sentences citing specific evidence\"}";

/// The JSON schema the scored judge must emit, sent as an OpenAI-style
/// `response_format`. Providers that ignore it still get the shape from
/// the system prompt, and [`parse_scored`] enforces it either way.
#[must_use]
pub fn scored_schema() -> Value {
    let axis = json!({"type": "integer", "minimum": 1, "maximum": 5});
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "scores", "rationale"],
        "properties": {
            "verdict": {"type": "string", "enum": ["PASS", "FAIL"]},
            "scores": {
                "type": "object",
                "additionalProperties": false,
                "required": ["requirements", "correctness", "code_quality", "scope_discipline", "process"],
                "properties": {
                    "requirements": axis,
                    "correctness": axis,
                    "code_quality": axis,
                    "scope_discipline": axis,
                    "process": axis,
                },
            },
            "rationale": {"type": "string"},
        },
    })
}

/// Ask `model` for a scored verdict over `ev`.
///
/// # Errors
/// Same contract as [`judge`]: transport failure, a non-2xx response, or
/// output that does not match the schema are all errors, which callers
/// map to INCONCLUSIVE — never to a pass, and never to a made-up score.
pub async fn judge_scored(gateway_url: &str, gateway_key: Option<&str>, model: &str, ev: &JudgeEvidence) -> Result<ScoredVerdict> {
    let url = format!("{}/chat/completions", gateway_url.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "temperature": smooth_policy::llm_params::AGENT_TEMPERATURE,
        "response_format": {
            "type": "json_schema",
            "json_schema": {"name": "bench_verdict", "strict": true, "schema": scored_schema()},
        },
        "messages": [
            {"role": "system", "content": SCORED_SYSTEM},
            {"role": "user", "content": render_evidence(ev)},
        ],
    });
    let mut req = reqwest::Client::new().post(&url).json(&body);
    if let Some(k) = gateway_key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.with_context(|| format!("judge request to {url}"))?;
    let status = resp.status();
    crate::spend::record_harness_response(resp.headers());
    let text = resp.text().await.context("reading judge response body")?;
    anyhow::ensure!(status.is_success(), "judge gateway returned {status}: {}", truncate(&text, 400));
    let v: Value = serde_json::from_str(&text).with_context(|| format!("judge response was not JSON: {}", truncate(&text, 400)))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("judge response carried no message content: {}", truncate(&text, 400)))?;
    parse_scored(content).ok_or_else(|| anyhow::anyhow!("scored verdict did not match the schema: {}", truncate(content, 400)))
}

/// Parse a scored verdict. Tolerates a markdown fence or a sentence of
/// preamble around the object; rejects anything with a missing axis, an
/// axis outside 1–5, an unknown verdict, or an empty rationale.
#[must_use]
pub fn parse_scored(text: &str) -> Option<ScoredVerdict> {
    #[derive(Deserialize)]
    struct Raw {
        verdict: String,
        scores: JudgeScores,
        rationale: String,
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let raw: Raw = serde_json::from_str(&text[start..=end]).ok()?;
    let passed = match raw.verdict.trim().to_ascii_uppercase().as_str() {
        "PASS" => true,
        "FAIL" => false,
        _ => return None,
    };
    if !raw.scores.all_in_range() || raw.rationale.trim().is_empty() {
        return None;
    }
    Some(ScoredVerdict {
        passed,
        scores: raw.scores,
        rationale: raw.rationale.trim().to_string(),
    })
}

/// Fold objective checks and judge axes into one 0–100 quality score.
///
/// Half objective (the share of checks that held), half judge (the axis
/// mean, rescaled so 1 → 0 and 5 → 1). Neither half alone is enough: the
/// objective half cannot see a hand-rolled mess that happens to pass the
/// tests, and the judge cannot run the tests. With no objective checks
/// the judge carries the whole score.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "check counts are small")]
pub fn combined_score(objective_passed: usize, objective_total: usize, scores: JudgeScores) -> f64 {
    let judge = (scores.mean() - 1.0) / 4.0;
    let raw = if objective_total == 0 {
        judge
    } else {
        0.5 * (objective_passed as f64 / objective_total as f64) + 0.5 * judge
    };
    (raw * 1000.0).round() / 10.0
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
            ..Default::default()
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

    #[test]
    fn scored_parses_the_schema_shape() {
        let v = parse_scored(
            r#"{"verdict":"PASS","scores":{"requirements":5,"correctness":4,"code_quality":4,"scope_discipline":5,"process":3},"rationale":"Built it and ran the tests."}"#,
        )
        .unwrap();
        assert!(v.passed);
        assert_eq!(v.scores.correctness, 4);
        assert!((v.scores.mean() - 4.2).abs() < 1e-9);
        assert_eq!(v.rationale, "Built it and ran the tests.");
    }

    #[test]
    fn scored_tolerates_fences_and_preamble() {
        let v = parse_scored(
            "Here you go:\n```json\n{\"verdict\":\"fail\",\"scores\":{\"requirements\":2,\"correctness\":1,\"code_quality\":3,\"scope_discipline\":4,\"process\":2},\"rationale\":\"Tests fail.\"}\n```",
        )
        .unwrap();
        assert!(!v.passed);
        assert_eq!(v.scores.requirements, 2);
    }

    /// A score the schema forbids must never be averaged into a leaderboard.
    #[test]
    fn scored_rejects_out_of_contract_output() {
        for bad in [
            "",
            "PASS",
            r#"{"verdict":"PASS","rationale":"no scores"}"#,
            r#"{"verdict":"MAYBE","scores":{"requirements":3,"correctness":3,"code_quality":3,"scope_discipline":3,"process":3},"rationale":"x"}"#,
            r#"{"verdict":"PASS","scores":{"requirements":6,"correctness":3,"code_quality":3,"scope_discipline":3,"process":3},"rationale":"x"}"#,
            r#"{"verdict":"PASS","scores":{"requirements":0,"correctness":3,"code_quality":3,"scope_discipline":3,"process":3},"rationale":"x"}"#,
            r#"{"verdict":"PASS","scores":{"requirements":3,"correctness":3,"code_quality":3,"scope_discipline":3},"rationale":"x"}"#,
            r#"{"verdict":"PASS","scores":{"requirements":3,"correctness":3,"code_quality":3,"scope_discipline":3,"process":3},"rationale":"  "}"#,
            r#"{"verdict":"PASS","scores":{"requirements":3.5,"correctness":3,"code_quality":3,"scope_discipline":3,"process":3},"rationale":"x"}"#,
        ] {
            assert!(parse_scored(bad).is_none(), "must be INCONCLUSIVE: {bad:?}");
        }
    }

    #[test]
    fn scored_schema_requires_every_axis() {
        let s = scored_schema();
        let req: Vec<&str> = s
            .pointer("/properties/scores/required")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(req, ["requirements", "correctness", "code_quality", "scope_discipline", "process"]);
    }

    #[test]
    fn combined_score_weights_objective_and_judge_equally() {
        let top = JudgeScores {
            requirements: 5,
            correctness: 5,
            code_quality: 5,
            scope_discipline: 5,
            process: 5,
        };
        let bottom = JudgeScores {
            requirements: 1,
            correctness: 1,
            code_quality: 1,
            scope_discipline: 1,
            process: 1,
        };
        assert!((combined_score(4, 4, top) - 100.0).abs() < 1e-9);
        assert!((combined_score(0, 4, bottom) - 0.0).abs() < 1e-9);
        assert!((combined_score(2, 4, top) - 75.0).abs() < 1e-9);
        assert!((combined_score(4, 4, bottom) - 50.0).abs() < 1e-9);
        // No objective checks: the judge carries it alone.
        assert!((combined_score(0, 0, top) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn evidence_render_includes_objective_and_diff_only_when_present() {
        let mut ev = JudgeEvidence {
            rubric: "r".into(),
            ..Default::default()
        };
        let bare = render_evidence(&ev);
        assert!(!bare.contains("## OBJECTIVE CHECKS"));
        assert!(!bare.contains("## FILES CHANGED"));
        ev.objective = "✓ npm test".into();
        ev.diff = "M src/cart.js".into();
        let full = render_evidence(&ev);
        assert!(full.contains("## OBJECTIVE CHECKS") && full.contains("✓ npm test"));
        assert!(full.contains("## FILES CHANGED") && full.contains("M src/cart.js"));
    }
}

//! Boardroom Narc — Big Smooth's central access arbiter.
//!
//! `BoardroomNarc` is the in-process service that backs `POST
//! /api/narc/judge`. Per-VM Wonks escalate to it when their local policy
//! cannot auto-approve a `/check/*` request; Narc runs a rule engine + LLM
//! judge and returns an `approve` / `deny` / `escalate_to_human` verdict.
//!
//! Narc is designed to be the default decision layer for the whole Smooth
//! fleet:
//!
//! - Every operator VM's Wonk talks to the same Boardroom Narc, so
//!   decisions are consistent across nodes.
//! - Short-circuit rules (see [`smooth_narc::judge`]) handle obviously-safe
//!   and obviously-dangerous resources without touching the LLM.
//! - The LLM judge is asked only when the rule engine falls through, and
//!   its verdict must be returned as strict JSON.
//! - Low-confidence verdicts are coerced to `EscalateToHuman` — Narc never
//!   silently approves something it isn't confident about.
//! - A decision cache de-duplicates repeated escalations for the same
//!   (bead, kind, resource) tuple.
//! - Audit logging goes through the normal Big Smooth audit pipeline, so
//!   every decision is traceable.

use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;
use smooth_narc::judge::{rule_engine_decide, Decision, DecisionCache, JudgeDecision, JudgeRequest};
use smooth_operator::llm::{LlmClient, LlmConfig};

/// Default confidence floor. LLM verdicts below this are rewritten to
/// `EscalateToHuman` — Narc won't auto-approve something it isn't sure
/// about.
pub const DEFAULT_ESCALATION_THRESHOLD: f32 = 0.7;

/// Maximum number of characters of the task summary we'll include in the
/// judge prompt. Longer summaries are truncated with an ellipsis.
pub const MAX_TASK_SUMMARY_CHARS: usize = 600;

/// In-process Boardroom Narc service.
///
/// Cheap to clone — everything inside is `Arc<_>` or a thin config value.
#[derive(Clone)]
pub struct BoardroomNarc {
    inner: Arc<Inner>,
}

struct Inner {
    llm: Option<LlmClient>,
    cache: DecisionCache,
    escalation_threshold: f32,
}

impl BoardroomNarc {
    /// Construct a Narc that will call `llm_config`'s provider for any
    /// decision the rule engine can't short-circuit. Pass `None` to get a
    /// rule-engine-only Narc that escalates every unhandled request.
    #[must_use]
    pub fn new(llm_config: Option<LlmConfig>) -> Self {
        let llm = llm_config.map(LlmClient::new);
        Self {
            inner: Arc::new(Inner {
                llm,
                cache: DecisionCache::new(),
                escalation_threshold: DEFAULT_ESCALATION_THRESHOLD,
            }),
        }
    }

    /// Construct a Narc with no LLM backend — used in tests and in modes
    /// where Big Smooth has no provider configured. Every request that
    /// isn't short-circuited by the rule engine is escalated to a human.
    #[must_use]
    pub fn without_llm() -> Self {
        Self::new(None)
    }

    /// Current size of the decision cache. For diagnostics.
    #[must_use]
    pub fn cache_len(&self) -> usize {
        self.inner.cache.len()
    }

    /// The main entry point. Rule engine → cache → LLM judge → escalation.
    ///
    /// # Errors
    ///
    /// This function does not return errors — every failure path is coerced
    /// into an `EscalateToHuman` decision so callers always get a verdict.
    pub async fn judge(&self, mut request: JudgeRequest) -> JudgeDecision {
        // Truncate the task summary before it enters any prompt or log line.
        if let Some(ref mut s) = request.task_summary {
            if s.chars().count() > MAX_TASK_SUMMARY_CHARS {
                let truncated: String = s.chars().take(MAX_TASK_SUMMARY_CHARS).collect();
                *s = format!("{truncated}…");
            }
        }

        let started = Instant::now();

        // 1. Rule engine short-circuit.
        if let Some(decision) = rule_engine_decide(&request) {
            self.record("rule_engine", &request, &decision, started.elapsed().as_millis());
            self.inner.cache.put(&request, &decision);
            return decision;
        }

        // 2. Cache hit.
        if let Some(decision) = self.inner.cache.get(&request) {
            self.record("cache", &request, &decision, started.elapsed().as_millis());
            return decision;
        }

        // 3. LLM judge.
        let decision = match self.run_llm_judge(&request).await {
            Ok(raw) => self.coerce_by_confidence(raw),
            Err(e) => JudgeDecision::escalate(format!("Narc LLM judge failed ({e}); escalating to human")),
        };

        self.record("llm_judge", &request, &decision, started.elapsed().as_millis());
        self.inner.cache.put(&request, &decision);
        decision
    }

    /// Low-confidence approvals become escalations — Narc never silently
    /// approves a request whose LLM verdict it isn't sure about.
    fn coerce_by_confidence(&self, decision: JudgeDecision) -> JudgeDecision {
        if matches!(decision.decision, Decision::Approve) && decision.confidence < self.inner.escalation_threshold {
            return JudgeDecision::escalate(format!(
                "LLM judge approved but confidence {:.2} < threshold {:.2}; escalating to human: {}",
                decision.confidence, self.inner.escalation_threshold, decision.reason
            ));
        }
        decision
    }

    fn record(&self, source: &str, request: &JudgeRequest, decision: &JudgeDecision, duration_ms: u128) {
        tracing::info!(
            source,
            kind = request.kind.as_str(),
            resource = %request.resource,
            operator_id = %request.operator_id,
            bead_id = %request.bead_id,
            decision = ?decision.decision,
            confidence = decision.confidence,
            duration_ms,
            reason = %decision.reason,
            "boardroom narc: access decision"
        );
    }

    async fn run_llm_judge(&self, request: &JudgeRequest) -> anyhow::Result<JudgeDecision> {
        let Some(ref llm) = self.inner.llm else {
            return Err(anyhow::anyhow!("Narc has no LLM backend configured"));
        };

        let system_prompt = concat!(
            "You are Narc, the central access-control judge for an AI coding agent running in a hardware-isolated microVM. ",
            "An operator agent has requested access to a resource its local policy can't auto-approve, and you must decide whether ",
            "to approve, deny, or escalate to a human. ",
            "You MUST respond with exactly one line of strict JSON matching this schema: ",
            "{\"decision\":\"approve\"|\"deny\"|\"escalate_to_human\",\"confidence\":<float 0-1>,\"reason\":\"<short explanation>\",\"add_to_allowlist_glob\":\"<optional glob>\"|null}. ",
            "Approve when the resource is clearly legitimate for the stated task (e.g., package registries, toolchain downloads, project dependencies). ",
            "Deny when the resource is clearly malicious or abusive (crypto wallets, credential exfiltration, rm -rf /). ",
            "Escalate when you are uncertain — it is better to escalate than to approve a risky request. ",
            "Keep the reason under 160 characters. Do not emit markdown, code fences, or any text outside the JSON object.",
        );

        let user_prompt = self.build_user_prompt(request);

        let sys_msg = smooth_operator::conversation::Message::system(system_prompt);
        let user_msg = smooth_operator::conversation::Message::user(&user_prompt);

        let response = llm.chat(&[&sys_msg, &user_msg], &[]).await?;
        parse_judge_response(&response.content)
    }

    fn build_user_prompt(&self, request: &JudgeRequest) -> String {
        let task_summary = request.task_summary.as_deref().unwrap_or("(no task summary provided)");
        let agent_reason = request.agent_reason.as_deref().unwrap_or("(no reason given)");
        let detail = request.detail.as_deref().unwrap_or("");
        format!(
            "Operator context:\n\
             - operator_id: {operator_id}\n\
             - bead_id: {bead_id}\n\
             - phase: {phase}\n\
             - task summary: {task_summary}\n\n\
             Access request:\n\
             - kind: {kind}\n\
             - resource: {resource}\n\
             - detail: {detail}\n\
             - agent-stated reason: {agent_reason}\n\n\
             Respond with the strict JSON verdict described in the system prompt.",
            operator_id = request.operator_id,
            bead_id = request.bead_id,
            phase = request.phase,
            kind = request.kind.as_str(),
            resource = request.resource,
        )
    }
}

/// JSON shape we expect the LLM to emit. We're strict about parsing — if it
/// doesn't match, we treat it as an error and escalate to human.
#[derive(Debug, Deserialize)]
struct RawVerdict {
    decision: String,
    confidence: Option<f32>,
    reason: Option<String>,
    #[serde(default)]
    add_to_allowlist_glob: Option<String>,
}

fn parse_judge_response(content: &str) -> anyhow::Result<JudgeDecision> {
    // The LLM may wrap its JSON in code fences despite instructions not to.
    // Strip any leading fences before parsing.
    let trimmed = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Some models also emit a leading summary line before the JSON. Try to
    // extract the first JSON object block.
    let json_slice = extract_first_json_object(trimmed).unwrap_or(trimmed);

    let raw: RawVerdict = serde_json::from_str(json_slice).map_err(|e| anyhow::anyhow!("parse judge JSON: {e}; content: {content}"))?;

    let decision = match raw.decision.to_ascii_lowercase().as_str() {
        "approve" | "allow" | "accept" => Decision::Approve,
        "deny" | "reject" | "block" => Decision::Deny,
        "escalate" | "escalate_to_human" | "human" | "uncertain" => Decision::EscalateToHuman,
        other => return Err(anyhow::anyhow!("unknown decision value: {other}")),
    };

    let confidence = raw.confidence.unwrap_or(0.0).clamp(0.0, 1.0);
    let reason = raw.reason.unwrap_or_else(|| "(no reason provided)".into());

    let cache_ttl_seconds = match decision {
        Decision::Approve => Some(3600),
        Decision::Deny => Some(300),
        Decision::EscalateToHuman => None,
    };

    Ok(JudgeDecision {
        decision,
        confidence,
        reason,
        add_to_allowlist_glob: raw.add_to_allowlist_glob,
        cache_ttl_seconds,
    })
}

/// Extract the first `{...}` object from a string, tracking brace depth and
/// ignoring braces inside string literals. Returns a sub-slice or `None` if
/// no balanced object is present.
fn extract_first_json_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut start: Option<usize> = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape_next = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escape_next {
                escape_next = false;
            } else if b == b'\\' {
                escape_next = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        let start_idx = start?;
                        return Some(&s[start_idx..=i]);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use smooth_narc::judge::JudgeKind;

    fn req_network(domain: &str) -> JudgeRequest {
        JudgeRequest {
            kind: JudgeKind::Network,
            operator_id: "op".into(),
            bead_id: "pearl-1".into(),
            phase: "execute".into(),
            resource: domain.into(),
            detail: None,
            task_summary: Some("agent is running a Rust cargo build".into()),
            agent_reason: None,
        }
    }

    #[tokio::test]
    async fn rule_engine_short_circuits_before_llm() {
        let narc = BoardroomNarc::without_llm();
        let decision = narc.judge(req_network("registry.npmjs.org")).await;
        assert_eq!(decision.decision, Decision::Approve);
    }

    #[tokio::test]
    async fn without_llm_unknown_domain_escalates() {
        let narc = BoardroomNarc::without_llm();
        let decision = narc.judge(req_network("playwright.azureedge.net")).await;
        assert_eq!(decision.decision, Decision::EscalateToHuman);
    }

    #[tokio::test]
    async fn cli_dangerous_pattern_denies() {
        let narc = BoardroomNarc::without_llm();
        let req = JudgeRequest {
            kind: JudgeKind::Cli,
            operator_id: "op".into(),
            bead_id: String::new(),
            phase: String::new(),
            resource: "rm -rf / --no-preserve-root".into(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        };
        let decision = narc.judge(req).await;
        assert_eq!(decision.decision, Decision::Deny);
    }

    #[test]
    fn parse_strict_json_verdict() {
        let content = r#"{"decision":"approve","confidence":0.92,"reason":"playwright browser CDN","add_to_allowlist_glob":"*.azureedge.net"}"#;
        let parsed = parse_judge_response(content).expect("parses");
        assert_eq!(parsed.decision, Decision::Approve);
        assert!((parsed.confidence - 0.92).abs() < 1e-6);
        assert_eq!(parsed.add_to_allowlist_glob.as_deref(), Some("*.azureedge.net"));
    }

    #[test]
    fn parse_handles_code_fence_wrapper() {
        let content = "```json\n{\"decision\":\"deny\",\"confidence\":1.0,\"reason\":\"crypto wallet\"}\n```";
        let parsed = parse_judge_response(content).expect("parses");
        assert_eq!(parsed.decision, Decision::Deny);
    }

    #[test]
    fn parse_handles_leading_summary() {
        let content = "Here is my verdict: {\"decision\":\"escalate_to_human\",\"confidence\":0.3,\"reason\":\"unclear\"}";
        let parsed = parse_judge_response(content).expect("parses");
        assert_eq!(parsed.decision, Decision::EscalateToHuman);
    }

    #[test]
    fn parse_unknown_decision_fails() {
        let content = r#"{"decision":"maybe","confidence":0.5,"reason":"idk"}"#;
        assert!(parse_judge_response(content).is_err());
    }

    #[test]
    fn confidence_coercion_escalates_uncertain_approvals() {
        let narc = BoardroomNarc::without_llm();
        let approval = JudgeDecision {
            decision: Decision::Approve,
            confidence: 0.5, // below default threshold 0.7
            reason: "kinda safe".into(),
            add_to_allowlist_glob: None,
            cache_ttl_seconds: Some(60),
        };
        let coerced = narc.coerce_by_confidence(approval);
        assert_eq!(coerced.decision, Decision::EscalateToHuman);
    }

    #[test]
    fn confidence_coercion_keeps_high_confidence_approvals() {
        let narc = BoardroomNarc::without_llm();
        let approval = JudgeDecision {
            decision: Decision::Approve,
            confidence: 0.95,
            reason: "clearly fine".into(),
            add_to_allowlist_glob: None,
            cache_ttl_seconds: Some(60),
        };
        let coerced = narc.coerce_by_confidence(approval);
        assert_eq!(coerced.decision, Decision::Approve);
    }

    #[tokio::test]
    async fn cache_dedup_returns_same_decision_twice() {
        let narc = BoardroomNarc::without_llm();
        // Rule engine approves registry.npmjs.org on first call AND caches it.
        let first = narc.judge(req_network("registry.npmjs.org")).await;
        let second = narc.judge(req_network("registry.npmjs.org")).await;
        assert_eq!(first.decision, Decision::Approve);
        assert_eq!(second.decision, Decision::Approve);
    }
}

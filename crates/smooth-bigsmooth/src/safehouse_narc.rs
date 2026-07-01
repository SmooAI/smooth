//! Safehouse Narc — Big Smooth's central access arbiter.
//!
//! `SafehouseNarc` is the in-process service that backs `POST
//! /api/narc/judge`. Per-VM Wonks escalate to it when their local policy
//! cannot auto-approve a `/check/*` request; Narc runs a rule engine + LLM
//! judge and returns an `approve` / `deny` / `escalate_to_human` verdict.
//!
//! Narc is designed to be the default decision layer for the whole Smooth
//! fleet:
//!
//! - Every operator VM's Wonk talks to the same Safehouse Narc, so
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
use std::time::{Duration, Instant};

use serde::Deserialize;
use smooth_narc::judge::{rule_engine_decide, Decision, DecisionCache, JudgeDecision, JudgeRequest, Scope};
use smooth_operator::llm::{LlmClient, LlmConfig};

use crate::access::{AccessStore, NewAccessRequest, ResolutionVerdict};
use crate::wonk_grants::SharedWonkGrants;

/// Default confidence floor. LLM verdicts below this are rewritten to
/// `EscalateToHuman` — Narc won't auto-approve something it isn't sure
/// about.
pub const DEFAULT_ESCALATION_THRESHOLD: f32 = 0.7;

/// Maximum number of characters of the task summary we'll include in the
/// judge prompt. Longer summaries are truncated with an ellipsis.
pub const MAX_TASK_SUMMARY_CHARS: usize = 600;

/// How long Narc holds an `Ask` open before failing closed.
///
/// 60s is long enough for the human to alt-tab to the TUI and pick a
/// scope; tools that would block longer should be redesigned. On
/// timeout the judge returns `EscalateToHuman` so the caller sees the
/// legacy fail-closed shape.
pub const ASK_HOLD_TIMEOUT: Duration = Duration::from_secs(60);

/// In-process Safehouse Narc service.
///
/// Cheap to clone — everything inside is `Arc<_>` or a thin config value.
#[derive(Clone)]
pub struct SafehouseNarc {
    inner: Arc<Inner>,
}

struct Inner {
    llm: Option<LlmClient>,
    cache: DecisionCache,
    escalation_threshold: f32,
    /// Shared pending-request queue. `Ask` verdicts are filed here and
    /// the judge awaits a human resolution before returning.
    access: AccessStore,
    /// How long to hold an `Ask` open before timing out. Overridable in
    /// tests; defaults to [`ASK_HOLD_TIMEOUT`].
    ask_hold_timeout: Duration,
    /// Persistent permission grants loaded from `wonk-allow.toml`.
    /// Consulted after the rule engine and before the LLM judge so
    /// approvals from prior sessions short-circuit without a round
    /// trip. `None` for tests / configurations that don't want
    /// persistent grants (the default).
    grants: Option<SharedWonkGrants>,
}

impl SafehouseNarc {
    /// Construct a Narc that will call `llm_config`'s provider for any
    /// decision the rule engine can't short-circuit. Pass `None` to get a
    /// rule-engine-only Narc that escalates every unhandled request.
    ///
    /// `access` is the [`AccessStore`] the judge files `Ask` verdicts
    /// into. Big Smooth keeps a single shared instance in `AppState` so
    /// the same queue powers both the HTTP routes and the SSE stream.
    #[must_use]
    pub fn new(llm_config: Option<LlmConfig>, access: AccessStore) -> Self {
        Self::with_timeout(llm_config, access, ASK_HOLD_TIMEOUT)
    }

    /// Like [`SafehouseNarc::new`] but with an explicit ask-hold timeout —
    /// for tests that want to exercise the timeout fail-closed path without
    /// sleeping a real 60s.
    #[must_use]
    pub fn with_timeout(llm_config: Option<LlmConfig>, access: AccessStore, ask_hold_timeout: Duration) -> Self {
        let llm = llm_config.map(LlmClient::new);
        Self {
            inner: Arc::new(Inner {
                llm,
                cache: DecisionCache::new(),
                escalation_threshold: DEFAULT_ESCALATION_THRESHOLD,
                access,
                ask_hold_timeout,
                grants: None,
            }),
        }
    }

    /// Attach a persistent grants store (`wonk-allow.toml` backed).
    /// When set, the judge consults the grants after the rule engine
    /// and before the LLM — a hit short-circuits to Approve. Chainable.
    #[must_use]
    pub fn with_grants(self, grants: SharedWonkGrants) -> Self {
        // Inner is Arc'd. Rebuild rather than mutate-in-place — the
        // shared count is always 1 here (we just constructed it), so
        // the cost is a single reallocation at startup, never on hot
        // paths.
        let inner = match Arc::try_unwrap(self.inner) {
            Ok(mut inner) => {
                inner.grants = Some(grants);
                inner
            }
            Err(arc) => Inner {
                llm: arc.llm.clone(),
                cache: DecisionCache::new(),
                escalation_threshold: arc.escalation_threshold,
                access: arc.access.clone(),
                ask_hold_timeout: arc.ask_hold_timeout,
                grants: Some(grants),
            },
        };
        Self { inner: Arc::new(inner) }
    }

    /// Construct a Narc with no LLM backend — used in tests and in modes
    /// where Big Smooth has no provider configured. Every request that
    /// isn't short-circuited by the rule engine is escalated to a human.
    #[must_use]
    pub fn without_llm() -> Self {
        Self::new(None, AccessStore::new())
    }

    /// Reference to the underlying [`AccessStore`]. Exposed so HTTP
    /// handlers (or tests) can resolve / list pending requests through
    /// the same store the judge files into.
    #[must_use]
    pub fn access(&self) -> &AccessStore {
        &self.inner.access
    }

    /// Current size of the decision cache. For diagnostics.
    #[must_use]
    pub fn cache_len(&self) -> usize {
        self.inner.cache.len()
    }

    /// The main entry point. Rule engine → cache → moderation pre-filter →
    /// LLM judge → confidence coercion.
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

        // 1. Rule engine short-circuit — fast path for known-safe /
        //    known-dangerous patterns, no LLM call.
        if let Some(decision) = rule_engine_decide(&request) {
            self.record("rule_engine", &request, &decision, started.elapsed().as_millis());
            self.inner.cache.put(&request, &decision);
            return decision;
        }

        // 1b. Persistent user/project grants from wonk-allow.toml.
        //     Checked before the cache so a freshly-approved grant
        //     short-circuits even on a request the cache hasn't seen.
        //     Pearl th-38b72c.
        if let Some(decision) = self.check_persistent_grants(&request) {
            self.record("wonk_grants", &request, &decision, started.elapsed().as_millis());
            self.inner.cache.put(&request, &decision);
            return decision;
        }

        // 2. Cache hit — same (bead, kind, resource) tuple was decided
        //    recently. No network or LLM call.
        if let Some(decision) = self.inner.cache.get(&request) {
            self.record("cache", &request, &decision, started.elapsed().as_millis());
            return decision;
        }

        // 3. Moderation pre-filter. Before burning judge tokens on the
        //    LLM, check whether the resource + agent reason tripwire the
        //    provider's moderation endpoint. Flagged content becomes a
        //    hard deny with a category-tagged reason. Errors during
        //    moderation are treated as "no signal" and fall through to
        //    the LLM judge — we never fail open, but we also don't block
        //    legitimate work just because moderation is temporarily down.
        if let Some(decision) = self.run_moderation_prefilter(&request).await {
            self.record("moderation", &request, &decision, started.elapsed().as_millis());
            self.inner.cache.put(&request, &decision);
            return decision;
        }

        // 4. LLM judge — the most expensive step, only reached when the
        //    rule engine, cache, and moderation pre-filter all passed.
        let decision = match self.run_llm_judge(&request).await {
            Ok(raw) => self.coerce_by_confidence(raw),
            Err(e) => JudgeDecision::escalate(format!("Narc LLM judge failed ({e}); escalating to human")),
        };

        self.record("llm_judge", &request, &decision, started.elapsed().as_millis());

        // If the verdict is Ask, hold the call open and wait for a human
        // resolution before returning. The hold-and-replay semantic is the
        // whole point of the auto-mode UX: instead of the tool call dying
        // and the agent retrying, we pause it at the policy boundary,
        // surface a card in the TUI, and resume against whatever the
        // human picked. Approve / Deny / cache-hit verdicts return as
        // before.
        let decision = if matches!(decision.decision, Decision::Ask) {
            self.hold_for_human(&request, decision).await
        } else {
            decision
        };

        self.inner.cache.put(&request, &decision);
        decision
    }

    /// Consult the persistent grants store, if any. Returns
    /// `Some(approve)` when the request matches a stored grant for
    /// the corresponding kind. Returns `None` if there's no grants
    /// store wired up or no match.
    fn check_persistent_grants(&self, request: &JudgeRequest) -> Option<JudgeDecision> {
        let grants = self.inner.grants.as_ref()?.snapshot();
        let matched = match request.kind {
            smooth_narc::judge::JudgeKind::Network => grants.matches_host(&request.resource),
            smooth_narc::judge::JudgeKind::Tool => grants.matches_tool(&request.resource),
            smooth_narc::judge::JudgeKind::Cli => grants.matches_bash(&request.resource),
            // File / Mcp / Port don't have a [section] in v1 yet —
            // grants don't speak to them. Fall through.
            smooth_narc::judge::JudgeKind::File | smooth_narc::judge::JudgeKind::Mcp | smooth_narc::judge::JudgeKind::Port => false,
        };
        if matched {
            let mut approval = JudgeDecision::approve(format!(
                "matched persistent grant in wonk-allow.toml: {} → {}",
                request.kind.as_str(),
                request.resource
            ));
            // Persistent grants don't expire from the in-memory cache
            // any faster than they expire from the file — pin to the
            // standard hour so subsequent requests skip the file
            // read entirely.
            approval.cache_ttl_seconds = Some(3600);
            Some(approval)
        } else {
            None
        }
    }

    /// File an `Ask` verdict into the [`AccessStore`] and await a human
    /// resolution. On approve, returns an `Approve` decision (with the
    /// optional glob_override threaded back through to Wonk). On deny,
    /// returns a `Deny`. On timeout / dropped resolver, returns
    /// `EscalateToHuman` (legacy fail-closed) so the caller sees the
    /// same denial it would have got pre-Ask.
    async fn hold_for_human(&self, request: &JudgeRequest, ask: JudgeDecision) -> JudgeDecision {
        debug_assert!(matches!(ask.decision, Decision::Ask));
        let new_req = NewAccessRequest {
            bead_id: request.bead_id.clone(),
            operator_id: request.operator_id.clone(),
            kind: request.kind.as_str().to_string(),
            resource: request.resource.clone(),
            detail: request.detail.clone(),
            reason: ask.reason.clone(),
            scope_options: if ask.scope_options.is_empty() {
                Scope::default_options()
            } else {
                ask.scope_options.clone()
            },
        };
        let (id, fut) = self.inner.access.file_pending(new_req);
        tracing::info!(
            id = %id,
            kind = request.kind.as_str(),
            resource = %request.resource,
            timeout_secs = self.inner.ask_hold_timeout.as_secs(),
            "safehouse narc: holding tool call open for human resolution"
        );

        let Some(resolution) = fut.await_resolution_with_timeout(self.inner.ask_hold_timeout).await else {
            // Either the timeout fired or the caller dropped the
            // receiver. Either way: expire the pending entry (best-
            // effort, may already be gone) and fail closed with the
            // legacy EscalateToHuman shape so the caller can distinguish
            // "no human" from a deliberate human deny.
            let _ = self.inner.access.expire(&id);
            tracing::warn!(
                id = %id,
                kind = request.kind.as_str(),
                resource = %request.resource,
                "safehouse narc: ask timed out without human resolution — failing closed"
            );
            return JudgeDecision::escalate(format!("ask timed out after {}s: {}", self.inner.ask_hold_timeout.as_secs(), ask.reason));
        };

        match resolution.verdict {
            ResolutionVerdict::Approve => {
                let mut approved = JudgeDecision::approve(format!("human approved at scope {}: {}", resolution.scope.as_str(), ask.reason));
                // If the human (or UI) bound the approval to a glob,
                // thread it through so Wonk caches the glob in its
                // runtime allowlist instead of just the exact resource.
                approved.add_to_allowlist_glob = resolution.glob_override.clone();
                // Cache scope-aware. `Once` never caches; the others
                // get a session-length TTL. Persistent (project / user)
                // grants live in wonk-allow.toml (Phase C); this cache
                // is purely a per-process speedup.
                approved.cache_ttl_seconds = match resolution.scope {
                    Scope::Once => None,
                    Scope::Session | Scope::PearlProject | Scope::User => Some(3600),
                };
                tracing::info!(
                    id = %id,
                    scope = resolution.scope.as_str(),
                    glob = ?approved.add_to_allowlist_glob,
                    "safehouse narc: human approved"
                );
                approved
            }
            ResolutionVerdict::Deny => {
                tracing::info!(
                    id = %id,
                    scope = resolution.scope.as_str(),
                    "safehouse narc: human denied"
                );
                JudgeDecision::deny(format!("human denied at scope {}: {}", resolution.scope.as_str(), ask.reason))
            }
        }
    }

    /// Run the moderation pre-filter against the provider's OpenAI-compat
    /// `/v1/moderations` endpoint. Returns `Some(deny_decision)` if the
    /// content is flagged, `None` otherwise (either moderation passed or
    /// moderation errored — errors are logged but don't short-circuit
    /// the decision flow).
    async fn run_moderation_prefilter(&self, request: &JudgeRequest) -> Option<JudgeDecision> {
        let llm = self.inner.llm.as_ref()?;

        // Only request types that have user-controlled natural-language
        // content are worth moderating. A raw domain name or a port
        // number carries no content to classify, and moderation on them
        // just wastes a round-trip.
        let moderation_input = match request.kind {
            smooth_narc::judge::JudgeKind::Cli | smooth_narc::judge::JudgeKind::Tool => Some(build_moderation_input(request)),
            smooth_narc::judge::JudgeKind::Network
            | smooth_narc::judge::JudgeKind::File
            | smooth_narc::judge::JudgeKind::Mcp
            | smooth_narc::judge::JudgeKind::Port => {
                // Moderate the agent's stated reason + task summary if
                // either is present — these are the free-text fields that
                // might carry abusive content. If neither is set, skip.
                if request.agent_reason.is_some() || request.task_summary.is_some() {
                    Some(build_moderation_input(request))
                } else {
                    None
                }
            }
        };

        let input = moderation_input?;

        match llm.moderate(&input).await {
            Ok(result) if result.flagged => {
                let categories = result.flagged_categories();
                let category_summary = if categories.is_empty() {
                    "unspecified".to_string()
                } else {
                    categories.join(", ")
                };
                Some(JudgeDecision::deny(format!(
                    "moderation pre-filter flagged content (categories: {category_summary})"
                )))
            }
            Ok(_) => None,
            Err(e) => {
                // Don't fail open — just skip this layer and let the LLM
                // judge (or rule engine) decide. Log the error loudly.
                tracing::warn!(
                    error = %e,
                    kind = request.kind.as_str(),
                    resource = %request.resource,
                    "safehouse narc: moderation pre-filter errored; falling through to LLM judge"
                );
                None
            }
        }
    }

    /// Low-confidence approvals become `Ask` verdicts — Narc never silently
    /// approves a request whose LLM verdict it isn't sure about. The
    /// upstream `judge()` will then hold the call open and surface the
    /// scope ladder to the human, instead of failing closed silently.
    /// (Pre-auto-mode this returned `EscalateToHuman`; the new behavior
    /// gives the user agency over uncertain calls instead of just denying
    /// them. Pearl th-49b4aa.)
    fn coerce_by_confidence(&self, decision: JudgeDecision) -> JudgeDecision {
        if matches!(decision.decision, Decision::Approve) && decision.confidence < self.inner.escalation_threshold {
            return JudgeDecision::ask(
                format!(
                    "LLM judge approved but confidence {:.2} < threshold {:.2}: {}",
                    decision.confidence, self.inner.escalation_threshold, decision.reason
                ),
                Scope::default_options(),
            );
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
            "safehouse narc: access decision"
        );
    }

    async fn run_llm_judge(&self, request: &JudgeRequest) -> anyhow::Result<JudgeDecision> {
        let Some(ref llm) = self.inner.llm else {
            return Err(anyhow::anyhow!("Narc has no LLM backend configured"));
        };

        let system_prompt = include_str!("../prompts/narc_judge.md");

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

/// Build the string passed to the moderation endpoint. We concatenate the
/// fields that might carry agent- or task-controlled natural language
/// (task summary, agent reason, cli command, tool args) so a single
/// moderation call covers every free-text dimension of a request.
///
/// Deliberately excludes structural fields like `operator_id`, `bead_id`,
/// and `kind` — those come from our own pipeline and aren't user content.
fn build_moderation_input(request: &JudgeRequest) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ref summary) = request.task_summary {
        parts.push(format!("Task: {summary}"));
    }
    if let Some(ref reason) = request.agent_reason {
        parts.push(format!("Agent reason: {reason}"));
    }
    // For CLI and Tool requests, the resource itself IS the free-text
    // content (shell command, tool arguments) so include it verbatim.
    if matches!(request.kind, smooth_narc::judge::JudgeKind::Cli | smooth_narc::judge::JudgeKind::Tool) {
        parts.push(format!("{}: {}", request.kind.as_str(), request.resource));
    }
    if let Some(ref detail) = request.detail {
        if !detail.is_empty() {
            parts.push(format!("Detail: {detail}"));
        }
    }
    if parts.is_empty() {
        // Fall back to the resource name so moderation has *something* to
        // classify. For network requests this is a domain, which is
        // almost never flagged by moderation — matching our expectation
        // that network decisions are mostly handled by the rule engine
        // and LLM judge, not by moderation.
        parts.push(request.resource.clone());
    }
    parts.join("\n\n")
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
        // `ask` is the new auto-mode form (pearl th-49b4aa). The judge
        // prompt may or may not have learned about it yet, so accept both
        // `ask` and the legacy `escalate*` family as the same human-gated
        // verdict. The shaping decision (which one to emit) lives in
        // `coerce_by_confidence` and the upcoming caller-side mapping —
        // here we just decode what the model gave us.
        "ask" | "ask_human" | "askhuman" => Decision::Ask,
        "escalate" | "escalate_to_human" | "human" | "uncertain" => Decision::EscalateToHuman,
        other => return Err(anyhow::anyhow!("unknown decision value: {other}")),
    };

    let confidence = raw.confidence.unwrap_or(0.0).clamp(0.0, 1.0);
    let reason = raw.reason.unwrap_or_else(|| "(no reason provided)".into());

    let cache_ttl_seconds = match decision {
        Decision::Approve => Some(3600),
        Decision::Deny => Some(300),
        // Human-gated verdicts are bound to a specific request and a
        // specific human resolution — caching them would mask future
        // policy intent. Always re-ask.
        Decision::Ask | Decision::EscalateToHuman => None,
    };

    Ok(JudgeDecision {
        decision,
        confidence,
        reason,
        add_to_allowlist_glob: raw.add_to_allowlist_glob,
        cache_ttl_seconds,
        // The LLM judge doesn't pick scope options yet — that's a Phase B
        // refinement once the TUI surfaces them. For now, any Ask the
        // judge emits offers the full default ladder.
        scope_options: if matches!(decision, Decision::Ask) {
            smooth_narc::judge::Scope::default_options()
        } else {
            Vec::new()
        },
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
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let start_idx = start?;
                    return Some(&s[start_idx..=i]);
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
        let narc = SafehouseNarc::without_llm();
        let decision = narc.judge(req_network("registry.npmjs.org")).await;
        assert_eq!(decision.decision, Decision::Approve);
    }

    #[tokio::test]
    async fn without_llm_unknown_domain_escalates() {
        let narc = SafehouseNarc::without_llm();
        let decision = narc.judge(req_network("playwright.azureedge.net")).await;
        assert_eq!(decision.decision, Decision::EscalateToHuman);
    }

    #[tokio::test]
    async fn cli_dangerous_pattern_denies() {
        let narc = SafehouseNarc::without_llm();
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
    fn confidence_coercion_asks_human_on_uncertain_approvals() {
        // Pre-auto-mode this returned EscalateToHuman (silent fail-closed).
        // The auto-mode flow makes Narc surface the uncertainty as an Ask
        // so the human can pick a scope inline. Pearl th-49b4aa.
        let narc = SafehouseNarc::without_llm();
        let approval = JudgeDecision {
            decision: Decision::Approve,
            confidence: 0.5, // below default threshold 0.7
            reason: "kinda safe".into(),
            add_to_allowlist_glob: None,
            cache_ttl_seconds: Some(60),
            scope_options: Vec::new(),
        };
        let coerced = narc.coerce_by_confidence(approval);
        assert_eq!(coerced.decision, Decision::Ask);
        // The coerced Ask offers the full scope ladder so the TUI can
        // present every option.
        assert_eq!(coerced.scope_options.len(), 4);
        // Asks never auto-cache — every uncertain decision gets re-asked.
        assert!(coerced.cache_ttl_seconds.is_none());
    }

    #[test]
    fn confidence_coercion_keeps_high_confidence_approvals() {
        let narc = SafehouseNarc::without_llm();
        let approval = JudgeDecision {
            decision: Decision::Approve,
            confidence: 0.95,
            reason: "clearly fine".into(),
            add_to_allowlist_glob: None,
            cache_ttl_seconds: Some(60),
            scope_options: Vec::new(),
        };
        let coerced = narc.coerce_by_confidence(approval);
        assert_eq!(coerced.decision, Decision::Approve);
    }

    #[tokio::test]
    async fn cache_dedup_returns_same_decision_twice() {
        let narc = SafehouseNarc::without_llm();
        // Rule engine approves registry.npmjs.org on first call AND caches it.
        let first = narc.judge(req_network("registry.npmjs.org")).await;
        let second = narc.judge(req_network("registry.npmjs.org")).await;
        assert_eq!(first.decision, Decision::Approve);
        assert_eq!(second.decision, Decision::Approve);
    }

    #[tokio::test]
    async fn hold_for_human_resolves_to_approve_when_human_approves() {
        // Build a Narc with a long ask-hold timeout so we know any test
        // flake isn't a race with the timeout fail-closed path.
        let access = AccessStore::new();
        let narc = SafehouseNarc::with_timeout(None, access.clone(), Duration::from_secs(5));

        // Construct an Ask verdict directly and drive hold_for_human.
        let ask = JudgeDecision::ask("test ask", Scope::default_options());
        let request = req_network("playwright.azureedge.net");

        // Spawn the await; concurrently approve the pending request.
        let resolver = {
            let access = access.clone();
            tokio::spawn(async move {
                // Poll until the pending request shows up — the await
                // call needs to file before we can resolve.
                for _ in 0..50 {
                    if let Some(pending) = access.list_pending().first().cloned() {
                        return access
                            .resolve(&pending.id, ResolutionVerdict::Approve, Scope::Session, Some("*.azureedge.net".into()))
                            .expect("resolve");
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                panic!("pending request never appeared");
            })
        };

        let decision = narc.hold_for_human(&request, ask).await;
        let _ = resolver.await.expect("resolver task");

        assert_eq!(decision.decision, Decision::Approve);
        // The glob_override threaded through so Wonk can cache by glob.
        assert_eq!(decision.add_to_allowlist_glob.as_deref(), Some("*.azureedge.net"));
        // Session scope caches for the runtime allowlist window.
        assert!(decision.cache_ttl_seconds.is_some());
        // Reason carries enough breadcrumbs for log inspection.
        assert!(decision.reason.contains("session"));
    }

    #[tokio::test]
    async fn hold_for_human_resolves_to_deny_when_human_denies() {
        let access = AccessStore::new();
        let narc = SafehouseNarc::with_timeout(None, access.clone(), Duration::from_secs(5));
        let ask = JudgeDecision::ask("test ask", Scope::default_options());
        let request = req_network("attacker.example");

        let resolver = {
            let access = access.clone();
            tokio::spawn(async move {
                for _ in 0..50 {
                    if let Some(pending) = access.list_pending().first().cloned() {
                        return access.resolve(&pending.id, ResolutionVerdict::Deny, Scope::Once, None).expect("resolve");
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                panic!("pending request never appeared");
            })
        };

        let decision = narc.hold_for_human(&request, ask).await;
        let _ = resolver.await.expect("resolver task");

        assert_eq!(decision.decision, Decision::Deny);
    }

    #[tokio::test]
    async fn hold_for_human_times_out_to_escalate_when_no_resolver() {
        // Short timeout — no one resolves, so we should fail closed in
        // ~100ms with an EscalateToHuman verdict.
        let access = AccessStore::new();
        let narc = SafehouseNarc::with_timeout(None, access.clone(), Duration::from_millis(100));
        let ask = JudgeDecision::ask("test ask", Scope::default_options());
        let request = req_network("nobody.cares");

        let start = std::time::Instant::now();
        let decision = narc.hold_for_human(&request, ask).await;
        let elapsed = start.elapsed();

        assert_eq!(decision.decision, Decision::EscalateToHuman);
        assert!(elapsed >= Duration::from_millis(100));
        // Timed-out request was expired — no garbage in the pending list.
        assert_eq!(access.pending_count(), 0);
    }

    #[tokio::test]
    async fn judge_holds_for_human_on_low_confidence_llm_approval() {
        // We can't easily mock the LLM judge from this test boundary, but
        // we can drive the same path by passing a low-confidence approval
        // through coerce_by_confidence -> Ask -> hold_for_human.
        //
        // This test asserts the end-to-end shape: a coerced Ask is the
        // verdict the judge() flow would emit, and hold_for_human resolves
        // it through the AccessStore.
        let access = AccessStore::new();
        let narc = SafehouseNarc::with_timeout(None, access.clone(), Duration::from_secs(5));

        let low_conf_approval = JudgeDecision {
            decision: Decision::Approve,
            confidence: 0.3,
            reason: "kinda safe".into(),
            add_to_allowlist_glob: None,
            cache_ttl_seconds: None,
            scope_options: Vec::new(),
        };
        let coerced = narc.coerce_by_confidence(low_conf_approval);
        assert_eq!(coerced.decision, Decision::Ask);

        let resolver = {
            let access = access.clone();
            tokio::spawn(async move {
                for _ in 0..50 {
                    if let Some(pending) = access.list_pending().first().cloned() {
                        return access
                            .resolve(&pending.id, ResolutionVerdict::Approve, Scope::PearlProject, None)
                            .expect("resolve");
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                panic!("pending request never appeared");
            })
        };

        let decision = narc.hold_for_human(&req_network("uncertain.example"), coerced).await;
        let _ = resolver.await.expect("resolver task");
        assert_eq!(decision.decision, Decision::Approve);
    }

    #[tokio::test]
    async fn judge_short_circuits_on_persistent_grant() {
        use crate::wonk_grants::{SharedWonkGrants, WonkGrants};

        let mut grants = WonkGrants::new();
        grants.add_host("custom.example");
        let shared = SharedWonkGrants::new(grants);

        let narc = SafehouseNarc::without_llm().with_grants(shared);
        // `custom.example` is NOT in the rule engine's OBVIOUSLY_SAFE
        // list, so without a persistent grant this would fall to the
        // LLM judge and (without an LLM) get coerced to Ask. The
        // grant should short-circuit to Approve.
        let decision = narc.judge(req_network("custom.example")).await;
        assert_eq!(decision.decision, Decision::Approve);
        assert!(decision.reason.contains("wonk-allow"));
    }

    #[tokio::test]
    async fn judge_persistent_grant_misses_fall_through() {
        use crate::wonk_grants::{SharedWonkGrants, WonkGrants};

        let mut grants = WonkGrants::new();
        grants.add_host("granted.example");
        let shared = SharedWonkGrants::new(grants);

        let narc = SafehouseNarc::without_llm().with_grants(shared);
        // A different host, not in grants or rule engine. Falls through
        // to the LLM judge (missing here) → escalate.
        let decision = narc.judge(req_network("ungranted.example")).await;
        assert_eq!(decision.decision, Decision::EscalateToHuman);
    }

    #[tokio::test]
    async fn judge_persistent_grant_matches_tool_kind() {
        use crate::wonk_grants::{SharedWonkGrants, WonkGrants};
        use smooth_narc::judge::JudgeKind;

        let mut grants = WonkGrants::new();
        grants.add_tool("custom_tool");
        let shared = SharedWonkGrants::new(grants);

        let narc = SafehouseNarc::without_llm().with_grants(shared);
        let req = JudgeRequest {
            kind: JudgeKind::Tool,
            operator_id: "op".into(),
            bead_id: "pearl".into(),
            phase: "execute".into(),
            resource: "custom_tool".into(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        };
        let decision = narc.judge(req).await;
        assert_eq!(decision.decision, Decision::Approve);
    }

    #[tokio::test]
    async fn judge_persistent_grant_matches_bash_prefix() {
        use crate::wonk_grants::{SharedWonkGrants, WonkGrants};
        use smooth_narc::judge::JudgeKind;

        let mut grants = WonkGrants::new();
        grants.add_bash_pattern("pnpm ");
        let shared = SharedWonkGrants::new(grants);

        let narc = SafehouseNarc::without_llm().with_grants(shared);
        let req = JudgeRequest {
            kind: JudgeKind::Cli,
            operator_id: "op".into(),
            bead_id: "pearl".into(),
            phase: "execute".into(),
            resource: "pnpm install".into(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        };
        let decision = narc.judge(req).await;
        assert_eq!(decision.decision, Decision::Approve);
    }
}

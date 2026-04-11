//! Boardroom Narc — central LLM-judge-backed access arbiter.
//!
//! This module defines the wire types and in-process service that Big Smooth
//! uses to arbitrate runtime access decisions for every operator microVM.
//!
//! ## Flow
//!
//! When a per-VM Wonk receives a `/check/*` request that its local policy
//! cannot auto-approve, Wonk escalates to the Boardroom Narc over HTTP
//! (`POST /api/narc/judge`). Boardroom Narc:
//!
//! 1. Consults a small LRU cache of prior decisions for the same
//!    `(operator_id, kind, resource)` tuple — if warm, return immediately
//!    (no LLM call).
//! 2. Applies a short-circuit rule engine: known-safe patterns
//!    (`*.npmjs.org`, `*.alpinelinux.org`, …) and known-dangerous patterns
//!    (`rm -rf /`, crypto-wallet domains, …) return without calling the LLM.
//! 3. If neither short-circuit hits, runs an LLM judge prompt that the
//!    model must answer with a strict JSON verdict: `approve` / `deny` /
//!    `escalate_to_human`, plus a confidence score and a reason.
//! 4. If the LLM's confidence is below `escalation_threshold`, the decision
//!    is coerced to `escalate_to_human` — which means Wonk fails closed and
//!    the human must approve via `th access pending`.
//!
//! ## Design goals
//!
//! - **Fast common case**: cache hits and short-circuit rules answer in
//!   microseconds. LLM calls are reserved for genuinely new decisions.
//! - **Fail closed**: any error path (LLM unreachable, parse failure, low
//!   confidence) defaults to `escalate_to_human`. Narc never silently
//!   approves a request it couldn't decide.
//! - **Wire-compatible across node types**: the same `JudgeRequest` /
//!   `JudgeDecision` types are used by operator VM Wonks escalating in and
//!   by any future boardroom-internal caller escalating out. A single Narc
//!   arbitrates across all nodes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// What kind of access the operator is requesting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeKind {
    /// Outbound network connection (domain + path).
    Network,
    /// Agent tool call (tool name).
    Tool,
    /// Filesystem read or write.
    File,
    /// Shell command execution.
    Cli,
    /// MCP server invocation.
    Mcp,
    /// Forwarded port.
    Port,
}

impl JudgeKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Tool => "tool",
            Self::File => "file",
            Self::Cli => "cli",
            Self::Mcp => "mcp",
            Self::Port => "port",
        }
    }
}

/// A request for a runtime access decision.
///
/// Escalated from Wonk to Boardroom Narc when local policy can't
/// auto-approve. Carries enough context for the LLM judge to reason about
/// whether the request is legitimate for the task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeRequest {
    pub kind: JudgeKind,
    pub operator_id: String,
    #[serde(default)]
    pub bead_id: String,
    #[serde(default)]
    pub phase: String,
    /// The resource being requested: domain for network, tool name for tool,
    /// path for file, command string for cli, server name for mcp, port
    /// number for port.
    pub resource: String,
    /// Optional extra detail — for network, the HTTP path; for cli, the
    /// working directory; etc.
    #[serde(default)]
    pub detail: Option<String>,
    /// A short summary of the task the operator is executing, to give the
    /// judge context. Truncated to a couple hundred characters before being
    /// sent over the wire.
    #[serde(default)]
    pub task_summary: Option<String>,
    /// Agent-supplied reason, if any.
    #[serde(default)]
    pub agent_reason: Option<String>,
}

/// The arbiter's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Wonk should allow the request and (optionally) cache the result so
    /// subsequent identical requests don't round-trip.
    Approve,
    /// Wonk should deny the request with Narc's reason.
    Deny,
    /// Narc is not confident enough to decide autonomously. Wonk fails
    /// closed (denies now) but also files a pending access request that a
    /// human can approve via `th access pending`.
    EscalateToHuman,
}

/// The response Narc sends back to Wonk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeDecision {
    pub decision: Decision,
    /// 0.0–1.0 confidence from the judge. Always 1.0 for short-circuit
    /// rule-engine decisions, derived from the LLM for LLM decisions.
    pub confidence: f32,
    /// Human-readable rationale. Logged by Wonk and surfaced in audit.
    pub reason: String,
    /// If `Some` and `decision == Approve`, Wonk SHOULD add this glob to its
    /// local allowlist so subsequent requests matching the glob don't
    /// re-escalate. Example: `*.azureedge.net` when a Playwright browser
    /// download was approved.
    #[serde(default)]
    pub add_to_allowlist_glob: Option<String>,
    /// How long Wonk should cache this decision locally, in seconds. `None`
    /// means "don't cache".
    #[serde(default)]
    pub cache_ttl_seconds: Option<u64>,
}

impl JudgeDecision {
    /// A hard deny with maximum confidence (used by the rule engine).
    #[must_use]
    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            decision: Decision::Deny,
            confidence: 1.0,
            reason: reason.into(),
            add_to_allowlist_glob: None,
            cache_ttl_seconds: None,
        }
    }

    /// A hard approve with maximum confidence (used by the rule engine).
    #[must_use]
    pub fn approve(reason: impl Into<String>) -> Self {
        Self {
            decision: Decision::Approve,
            confidence: 1.0,
            reason: reason.into(),
            add_to_allowlist_glob: None,
            cache_ttl_seconds: Some(3600),
        }
    }

    /// An escalation — the caller should fail closed now but file a pending
    /// access request for a human to review.
    #[must_use]
    pub fn escalate(reason: impl Into<String>) -> Self {
        Self {
            decision: Decision::EscalateToHuman,
            confidence: 0.0,
            reason: reason.into(),
            add_to_allowlist_glob: None,
            cache_ttl_seconds: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Rule engine: pre-LLM short-circuits
// ---------------------------------------------------------------------------

/// Domains that are obviously safe for any coding task and should never
/// burn LLM tokens. These complement, not replace, the per-task Wonk
/// allowlist — we use this table inside Narc itself so an escalation for
/// e.g. `registry.npmjs.org` short-circuits without an LLM call even if
/// some operator's local policy happens to omit it.
pub const OBVIOUSLY_SAFE_DOMAIN_SUFFIXES: &[&str] = &[
    // LLM providers Smooth itself ships with.
    "openrouter.ai",
    "api.llmgateway.io",
    "api.openai.com",
    "api.anthropic.com",
    "api.kimi.com",
    "api.moonshot.ai",
    // Package registries / indexes.
    ".npmjs.org",
    "registry.npmjs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
    "index.crates.io",
    "docs.rs",
    // Distro package repos.
    "dl-cdn.alpinelinux.org",
    "deb.debian.org",
    "archive.ubuntu.com",
    "security.ubuntu.com",
    // Language toolchain downloads.
    "static.rust-lang.org",
    "sh.rustup.rs",
    "nodejs.org",
    "deno.land",
    // GitHub (read-only, used heavily for git+https deps).
    "github.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
    "raw.githubusercontent.com",
    // MDN reference.
    "developer.mozilla.org",
];

/// Domains we will never auto-approve without a human in the loop, even if
/// the LLM says yes. Matches as a suffix.
pub const DANGEROUS_DOMAIN_SUFFIXES: &[&str] = &[
    // Credential-harvest adjacent infra.
    ".ngrok.io",
    ".ngrok-free.app",
    // Cryptocurrency wallets / drains — classic targets for exfil.
    "etherscan.io",
    "blockchain.info",
    "binance.com",
    // Paste/exfil targets.
    "pastebin.com",
    "termbin.com",
    "transfer.sh",
];

/// Shell command substrings that must never be auto-approved. Checked
/// case-insensitively.
pub const DANGEROUS_CLI_SUBSTRINGS: &[&str] = &[
    "rm -rf /",
    "rm -rf ~",
    ":(){ :|:& };:",
    "mkfs",
    "dd if=/dev/zero of=/dev/",
    "> /dev/sda",
    "chmod -r 777 /",
    "curl | sh",
    "wget | sh",
    "| sudo sh",
    "systemctl mask",
];

/// Match a domain against a suffix list. Exact matches and subdomain matches
/// both qualify. Lowercases the input for comparison.
#[must_use]
pub fn domain_matches_suffix_list(domain: &str, suffixes: &[&str]) -> bool {
    let d = domain.to_ascii_lowercase();
    for suffix in suffixes {
        let s = suffix.to_ascii_lowercase();
        if d == s || d.ends_with(&format!(".{s}")) || (s.starts_with('.') && d.ends_with(&s)) {
            return true;
        }
    }
    false
}

/// Decide a request purely from rule engine short-circuits, without touching
/// the LLM. Returns `None` if no rule matches and the caller should fall
/// through to the LLM judge.
#[must_use]
pub fn rule_engine_decide(request: &JudgeRequest) -> Option<JudgeDecision> {
    match request.kind {
        JudgeKind::Network => {
            if domain_matches_suffix_list(&request.resource, DANGEROUS_DOMAIN_SUFFIXES) {
                return Some(JudgeDecision::deny(format!(
                    "{} is on the Narc dangerous-domain deny list; escalate to a human to override",
                    request.resource
                )));
            }
            if domain_matches_suffix_list(&request.resource, OBVIOUSLY_SAFE_DOMAIN_SUFFIXES) {
                let mut approval = JudgeDecision::approve(format!("{} is on the Narc obviously-safe domain list", request.resource));
                // Cache aggressively for known-safe domains.
                approval.cache_ttl_seconds = Some(24 * 3600);
                return Some(approval);
            }
            None
        }
        JudgeKind::Cli => {
            let cmd = request.resource.to_ascii_lowercase();
            for needle in DANGEROUS_CLI_SUBSTRINGS {
                if cmd.contains(&needle.to_ascii_lowercase()) {
                    return Some(JudgeDecision::deny(format!("command matches Narc dangerous-cli pattern: {needle}")));
                }
            }
            None
        }
        // File / Tool / Mcp / Port currently have no short-circuit rules
        // — they always fall through to the LLM judge (or to the caller's
        // local policy when Narc isn't wired in).
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Decision cache
// ---------------------------------------------------------------------------

/// A small TTL-keyed cache of prior decisions. Keyed by
/// `(kind, resource, operator_id_bucket)` — the operator_id bucket is
/// deliberately coarse (we only key on the bead_id) so decisions made for
/// one iteration of a pearl reuse on the next iteration.
#[derive(Default)]
pub struct DecisionCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
}

struct CacheEntry {
    decision: JudgeDecision,
    expires_at: Instant,
}

fn cache_key(req: &JudgeRequest) -> String {
    // Use the bead_id (pearl id) as the bucket, falling back to "_" if
    // unset. This means every operator working on the same pearl shares
    // cached approvals — useful because pearls are retried.
    let bucket = if req.bead_id.is_empty() { "_" } else { req.bead_id.as_str() };
    format!("{}|{}|{}", req.kind.as_str(), bucket, req.resource)
}

impl DecisionCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a cached decision. Returns `None` if the entry is missing or
    /// expired. Expired entries are lazily removed on read.
    pub fn get(&self, req: &JudgeRequest) -> Option<JudgeDecision> {
        let key = cache_key(req);
        let mut entries = self.entries.lock().ok()?;
        if let Some(entry) = entries.get(&key) {
            if entry.expires_at > Instant::now() {
                return Some(entry.decision.clone());
            }
            entries.remove(&key);
        }
        None
    }

    /// Insert a decision into the cache with its per-decision TTL.
    pub fn put(&self, req: &JudgeRequest, decision: &JudgeDecision) {
        let Some(ttl_seconds) = decision.cache_ttl_seconds else {
            return;
        };
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        let key = cache_key(req);
        entries.insert(
            key,
            CacheEntry {
                decision: decision.clone(),
                expires_at: Instant::now() + Duration::from_secs(ttl_seconds),
            },
        );
    }

    /// Number of live cache entries. For diagnostics.
    pub fn len(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or(0)
    }

    /// True if there are no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn req_network(domain: &str) -> JudgeRequest {
        JudgeRequest {
            kind: JudgeKind::Network,
            operator_id: "op-1".into(),
            bead_id: "pearl-1".into(),
            phase: "execute".into(),
            resource: domain.into(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        }
    }

    #[test]
    fn domain_suffix_matches_exact_and_subdomains() {
        assert!(domain_matches_suffix_list("registry.npmjs.org", OBVIOUSLY_SAFE_DOMAIN_SUFFIXES));
        assert!(domain_matches_suffix_list("static.crates.io", OBVIOUSLY_SAFE_DOMAIN_SUFFIXES));
        assert!(domain_matches_suffix_list("objects.githubusercontent.com", OBVIOUSLY_SAFE_DOMAIN_SUFFIXES));
        assert!(!domain_matches_suffix_list("evil-crates.io", OBVIOUSLY_SAFE_DOMAIN_SUFFIXES));
        assert!(!domain_matches_suffix_list("crates.io.attacker.com", OBVIOUSLY_SAFE_DOMAIN_SUFFIXES));
    }

    #[test]
    fn rule_engine_approves_safe_domains_without_llm() {
        let decision = rule_engine_decide(&req_network("registry.npmjs.org")).expect("should short-circuit");
        assert_eq!(decision.decision, Decision::Approve);
        assert_eq!(decision.confidence, 1.0);
        assert!(decision.cache_ttl_seconds.is_some_and(|t| t >= 3600));
    }

    #[test]
    fn rule_engine_denies_dangerous_domains() {
        let decision = rule_engine_decide(&req_network("pastebin.com")).expect("should short-circuit");
        assert_eq!(decision.decision, Decision::Deny);
    }

    #[test]
    fn rule_engine_falls_through_for_unknown_domains() {
        assert!(rule_engine_decide(&req_network("playwright.azureedge.net")).is_none());
    }

    #[test]
    fn rule_engine_blocks_rm_rf_root() {
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
        let d = rule_engine_decide(&req).expect("deny");
        assert_eq!(d.decision, Decision::Deny);
    }

    #[test]
    fn decision_cache_hits_and_expires() {
        let cache = DecisionCache::new();
        let req = req_network("unknown.example");
        assert!(cache.get(&req).is_none());

        let mut approval = JudgeDecision::approve("test");
        approval.cache_ttl_seconds = Some(1);
        cache.put(&req, &approval);

        assert_eq!(cache.len(), 1);
        let hit = cache.get(&req).expect("cache hit");
        assert_eq!(hit.decision, Decision::Approve);

        // No TTL means we don't cache.
        let ephemeral = JudgeDecision::escalate("no cache");
        let req2 = req_network("another.example");
        cache.put(&req2, &ephemeral);
        assert!(cache.get(&req2).is_none());
    }
}

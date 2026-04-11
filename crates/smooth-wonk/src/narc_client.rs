//! Wonk → Boardroom Narc escalation client.
//!
//! When Wonk's local policy can't auto-approve a `/check/*` request, it
//! escalates the decision to Boardroom Narc by POSTing a [`JudgeRequest`]
//! to `{narc_url}/api/narc/judge`. This module wraps that HTTP call with a
//! bounded timeout and a clean failure mode: any network error is
//! converted to an `EscalateToHuman` decision so Wonk fails closed.
//!
//! `NarcClient` is cheap to clone — the inner `reqwest::Client` uses
//! connection pooling. Per-VM Wonk instantiates one at startup and keeps it
//! for the lifetime of the runner.

use std::time::Duration;

use smooth_narc::judge::{Decision, JudgeDecision, JudgeRequest};

/// HTTP client that speaks to the Boardroom Narc `/api/narc/judge` endpoint.
#[derive(Debug, Clone)]
pub struct NarcClient {
    base_url: String,
    client: reqwest::Client,
}

impl NarcClient {
    /// Build a client pointed at `base_url`. The URL must be the root of
    /// the Narc service (e.g. `http://host.containers.internal:4400`) —
    /// this client appends `/api/narc/judge` when making the actual call.
    ///
    /// The inner `reqwest::Client` is configured with a short connect
    /// timeout and a modest total timeout: Narc decisions should be fast
    /// (<1s for cache hits, a few seconds for LLM judge calls). Wonk
    /// callers block on this, so we don't want to stall indefinitely.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { base_url, client }
    }

    /// Escalate a request to Narc and return its decision.
    ///
    /// Any network-level or parse error is converted to an
    /// `EscalateToHuman` decision — Wonk treats this as "fail closed",
    /// denying the request now but surfacing it as a pending access
    /// request that a human can approve. Narc is never expected to
    /// silently approve a request it couldn't reach.
    pub async fn judge(&self, request: &JudgeRequest) -> JudgeDecision {
        let url = format!("{}/api/narc/judge", self.base_url);
        match self.client.post(&url).json(request).send().await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    return JudgeDecision {
                        decision: Decision::EscalateToHuman,
                        confidence: 0.0,
                        reason: format!("Narc returned HTTP {status}; failing closed"),
                        add_to_allowlist_glob: None,
                        cache_ttl_seconds: None,
                    };
                }
                match resp.json::<JudgeDecision>().await {
                    Ok(decision) => decision,
                    Err(e) => JudgeDecision {
                        decision: Decision::EscalateToHuman,
                        confidence: 0.0,
                        reason: format!("failed to parse Narc response: {e}"),
                        add_to_allowlist_glob: None,
                        cache_ttl_seconds: None,
                    },
                }
            }
            Err(e) => JudgeDecision {
                decision: Decision::EscalateToHuman,
                confidence: 0.0,
                reason: format!("Narc unreachable at {}: {e}", self.base_url),
                add_to_allowlist_glob: None,
                cache_ttl_seconds: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smooth_narc::judge::JudgeKind;

    fn req(domain: &str) -> JudgeRequest {
        JudgeRequest {
            kind: JudgeKind::Network,
            operator_id: "op".into(),
            bead_id: String::new(),
            phase: String::new(),
            resource: domain.into(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        }
    }

    #[tokio::test]
    async fn unreachable_narc_yields_escalate_to_human() {
        // 127.0.0.1:1 is the classic unused port — the OS will refuse
        // connection fast so this test doesn't hang on the 2s connect
        // timeout.
        let client = NarcClient::new("http://127.0.0.1:1");
        let decision = client.judge(&req("example.com")).await;
        assert_eq!(decision.decision, Decision::EscalateToHuman);
        assert!(decision.reason.contains("Narc") || decision.reason.contains("unreachable"));
    }

    #[test]
    fn base_url_is_trimmed() {
        let c = NarcClient::new("http://localhost:4400/");
        assert_eq!(c.base_url, "http://localhost:4400");
    }
}

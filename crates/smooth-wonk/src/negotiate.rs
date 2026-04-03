use serde::{Deserialize, Serialize};

use crate::policy::PolicyHolder;

/// Handles access negotiation with Big Smooth.
/// When an operator needs access to a resource not in policy, Wonk sends
/// a request to Big Smooth, which either auto-approves or escalates to human.
#[derive(Clone)]
pub struct Negotiator {
    leader_url: String,
    client: reqwest::Client,
    #[allow(dead_code)]
    policy_holder: PolicyHolder,
}

#[derive(Debug, Serialize)]
pub struct AccessRequest {
    pub operator_id: String,
    pub bead_id: String,
    pub resource_type: String, // "network", "tool", "bead"
    pub resource: String,      // domain, tool name, or bead id
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct AccessResponse {
    pub approved: bool,
    pub reason: String,
    #[serde(default)]
    pub updated_policy_toml: Option<String>,
}

impl Negotiator {
    pub fn new(leader_url: &str, policy_holder: PolicyHolder) -> Self {
        Self {
            leader_url: leader_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            policy_holder,
        }
    }

    /// Request expanded access from Big Smooth.
    ///
    /// # Errors
    /// Returns error if Big Smooth is unreachable or returns an invalid response.
    pub async fn request_access(&self, request: &AccessRequest, auth_token: &str) -> anyhow::Result<AccessResponse> {
        let url = format!("{}/api/access/request", self.leader_url);

        let resp = self.client.post(&url).bearer_auth(auth_token).json(request).send().await?;

        if resp.status().is_success() {
            let response: AccessResponse = resp.json().await?;

            // If Big Smooth sent an updated policy, apply it
            if let Some(ref toml_str) = response.updated_policy_toml {
                match smooth_policy::Policy::from_toml(toml_str) {
                    Ok(new_policy) => {
                        self.policy_holder.update(new_policy);
                        tracing::info!("policy updated from access negotiation");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "received invalid policy from Big Smooth");
                    }
                }
            }

            Ok(response)
        } else {
            Ok(AccessResponse {
                approved: false,
                reason: format!("Big Smooth returned status {}", resp.status()),
                updated_policy_toml: None,
            })
        }
    }

    /// Get the leader URL.
    #[allow(dead_code)]
    pub fn leader_url(&self) -> &str {
        &self.leader_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_request_serializes() {
        let req = AccessRequest {
            operator_id: "op-123".into(),
            bead_id: "smooth-abc".into(),
            resource_type: "network".into(),
            resource: "api.stripe.com".into(),
            reason: "need payment API access".into(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("api.stripe.com"));
        assert!(json.contains("network"));
    }

    #[test]
    fn access_response_approved() {
        let json = r#"{"approved": true, "reason": "auto-approved: domain in auto_approve_domains"}"#;
        let resp: AccessResponse = serde_json::from_str(json).expect("deserialize");
        assert!(resp.approved);
        assert!(resp.updated_policy_toml.is_none());
    }

    #[test]
    fn access_response_with_updated_policy() {
        let json = r#"{"approved": true, "reason": "approved", "updated_policy_toml": "[metadata]\noperator_id = \"op\"\n[auth]\ntoken = \"t\""}"#;
        let resp: AccessResponse = serde_json::from_str(json).expect("deserialize");
        assert!(resp.approved);
        assert!(resp.updated_policy_toml.is_some());
    }

    #[test]
    fn access_response_denied() {
        let json = r#"{"approved": false, "reason": "pending human approval"}"#;
        let resp: AccessResponse = serde_json::from_str(json).expect("deserialize");
        assert!(!resp.approved);
    }

    #[test]
    fn negotiator_url_normalization() {
        let policy = smooth_policy::Policy::from_toml(
            r#"
[metadata]
operator_id = "test"
[auth]
token = "test"
[network]
"#,
        )
        .expect("parse");
        let holder = PolicyHolder::from_policy(policy);
        let neg = Negotiator::new("http://localhost:4400/", holder);
        assert_eq!(neg.leader_url(), "http://localhost:4400");
    }
}

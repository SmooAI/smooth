use serde::{Deserialize, Serialize};

/// Client for the local Wonk API (127.0.0.1:8400).
/// Goalie asks Wonk "is this allowed?" for every request.
#[derive(Debug, Clone)]
pub struct WonkClient {
    base_url: String,
    client: reqwest::Client,
    /// Operator token from the policy's [auth] section. Sent on every
    /// request as `Authorization: Bearer <token>`. Wonk's middleware
    /// rejects unauthenticated callers with 401.
    auth_token: String,
}

#[derive(Debug, Serialize)]
pub struct NetworkCheckRequest {
    pub domain: String,
    pub path: String,
    pub method: String,
}

#[derive(Debug, Deserialize)]
pub struct WonkDecision {
    pub allowed: bool,
    pub reason: String,
}

impl WonkClient {
    pub fn new(base_url: &str) -> Self {
        Self::with_auth(base_url, String::new())
    }

    /// Construct a client that sends `Authorization: Bearer <token>` on
    /// every request.
    pub fn with_auth(base_url: &str, auth_token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            auth_token: auth_token.into(),
        }
    }

    /// Ask Wonk whether a network request is allowed.
    ///
    /// # Errors
    /// Returns error if Wonk is unreachable or returns an invalid response.
    pub async fn check_network(&self, domain: &str, path: &str, method: &str) -> anyhow::Result<WonkDecision> {
        let url = format!("{}/check/network", self.base_url);
        let req = NetworkCheckRequest {
            domain: domain.to_string(),
            path: path.to_string(),
            method: method.to_string(),
        };

        let mut builder = self.client.post(&url).json(&req);
        if !self.auth_token.is_empty() {
            builder = builder.bearer_auth(&self.auth_token);
        }
        let resp = builder.send().await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            // If Wonk is down or erroring, fail closed (deny)
            Ok(WonkDecision {
                allowed: false,
                reason: format!("Wonk returned status {}", resp.status()),
            })
        }
    }

    /// Get the base URL for this Wonk instance.
    #[allow(dead_code)]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wonk_client_url_normalization() {
        let client = WonkClient::new("http://127.0.0.1:8400/");
        assert_eq!(client.base_url(), "http://127.0.0.1:8400");

        let client2 = WonkClient::new("http://127.0.0.1:8400");
        assert_eq!(client2.base_url(), "http://127.0.0.1:8400");
    }

    #[test]
    fn network_check_request_serializes() {
        let req = NetworkCheckRequest {
            domain: "api.github.com".into(),
            path: "/repos/SmooAI/smooth".into(),
            method: "GET".into(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("api.github.com"));
        assert!(json.contains("GET"));
    }

    #[test]
    fn wonk_decision_deserializes() {
        let json = r#"{"allowed": true, "reason": "domain in allowlist"}"#;
        let decision: WonkDecision = serde_json::from_str(json).expect("deserialize");
        assert!(decision.allowed);
        assert_eq!(decision.reason, "domain in allowlist");
    }

    #[test]
    fn wonk_decision_denied() {
        let json = r#"{"allowed": false, "reason": "domain not in allowlist"}"#;
        let decision: WonkDecision = serde_json::from_str(json).expect("deserialize");
        assert!(!decision.allowed);
    }
}

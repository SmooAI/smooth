//! Jira integration — env-var-driven bidirectional sync.
//!
//! Activated when all four env vars are set:
//! - `JIRA_URL` — e.g. `https://smooai.atlassian.net`
//! - `JIRA_PROJECT` — e.g. `SMOODEV`
//! - `JIRA_API_TOKEN`
//! - `JIRA_EMAIL`

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Jira configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct JiraConfig {
    pub url: String,
    pub project: String,
    pub email: String,
    pub api_token: String,
}

impl JiraConfig {
    /// Load from environment variables. Returns `None` if any are missing.
    pub fn from_env() -> Option<Self> {
        Some(Self {
            url: std::env::var("JIRA_URL").ok()?,
            project: std::env::var("JIRA_PROJECT").ok()?,
            email: std::env::var("JIRA_EMAIL").ok()?,
            api_token: std::env::var("JIRA_API_TOKEN").ok()?,
        })
    }
}

/// Simplified Jira issue.
#[derive(Debug, Serialize, Deserialize)]
pub struct JiraIssue {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub description: Option<String>,
}

/// Result of creating a Jira ticket.
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateResult {
    pub key: String,
    pub id: String,
}

/// Jira REST API client.
#[derive(Clone)]
pub struct JiraClient {
    config: JiraConfig,
    http: reqwest::Client,
}

impl JiraClient {
    /// Create a new Jira client. Returns `None` if env vars are not set.
    pub fn from_env() -> Option<Self> {
        let config = JiraConfig::from_env()?;
        Some(Self {
            config,
            http: reqwest::Client::new(),
        })
    }

    /// Create with explicit config.
    pub fn new(config: JiraConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Check if Jira is reachable with the configured credentials.
    pub async fn check_connection(&self) -> bool {
        let url = format!("{}/rest/api/3/myself", self.config.url);
        self.http
            .get(&url)
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    /// Create a Jira ticket for a pearl.
    pub async fn create_ticket(&self, summary: &str, description: &str) -> Result<CreateResult> {
        let url = format!("{}/rest/api/3/issue", self.config.url);
        let body = serde_json::json!({
            "fields": {
                "project": { "key": &self.config.project },
                "summary": summary,
                "description": {
                    "type": "doc",
                    "version": 1,
                    "content": [{
                        "type": "paragraph",
                        "content": [{
                            "type": "text",
                            "text": description
                        }]
                    }]
                },
                "issuetype": { "name": "Task" }
            }
        });

        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .json(&body)
            .send()
            .await
            .context("jira: send create request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("jira: create ticket failed ({status}): {body}");
        }

        resp.json::<CreateResult>().await.context("jira: parse create response")
    }

    /// Transition a Jira ticket to a target status.
    pub async fn transition_ticket(&self, ticket_key: &str, target_status: &str) -> Result<()> {
        // First get available transitions
        let url = format!("{}/rest/api/3/issue/{ticket_key}/transitions", self.config.url);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .send()
            .await
            .context("jira: get transitions")?;

        let body: serde_json::Value = resp.json().await.context("jira: parse transitions")?;
        let transitions = body["transitions"].as_array().context("jira: transitions not an array")?;

        // Find matching transition
        let target_lower = target_status.to_lowercase();
        let transition_id = transitions
            .iter()
            .find(|t| t["name"].as_str().unwrap_or("").to_lowercase().contains(&target_lower))
            .and_then(|t| t["id"].as_str())
            .map(String::from);

        let Some(id) = transition_id else {
            tracing::warn!(ticket = %ticket_key, target = %target_status, "jira: no matching transition found");
            return Ok(());
        };

        // Execute transition
        let url = format!("{}/rest/api/3/issue/{ticket_key}/transitions", self.config.url);
        let body = serde_json::json!({ "transition": { "id": id } });
        self.http
            .post(&url)
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .json(&body)
            .send()
            .await
            .context("jira: execute transition")?;

        tracing::info!(ticket = %ticket_key, status = %target_status, "jira: transitioned ticket");
        Ok(())
    }

    /// Add a comment to a Jira ticket.
    pub async fn add_comment(&self, ticket_key: &str, comment: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{ticket_key}/comment", self.config.url);
        let body = serde_json::json!({
            "body": {
                "type": "doc",
                "version": 1,
                "content": [{
                    "type": "paragraph",
                    "content": [{
                        "type": "text",
                        "text": comment
                    }]
                }]
            }
        });

        self.http
            .post(&url)
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .json(&body)
            .send()
            .await
            .context("jira: add comment")?;

        Ok(())
    }

    /// Get the project key.
    pub fn project(&self) -> &str {
        &self.config.project
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_returns_none_when_missing() {
        // Clear env vars to ensure None
        std::env::remove_var("JIRA_URL");
        let config = JiraConfig::from_env();
        assert!(config.is_none());
    }

    #[test]
    fn create_result_roundtrip() {
        let result = CreateResult {
            key: "SMOODEV-42".to_string(),
            id: "10042".to_string(),
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let parsed: CreateResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.key, "SMOODEV-42");
    }

    #[test]
    fn client_from_env_none_without_vars() {
        std::env::remove_var("JIRA_URL");
        assert!(JiraClient::from_env().is_none());
    }
}

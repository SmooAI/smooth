//! HTTP client for the Diver pearl lifecycle service.
//!
//! When running in Boardroom mode, Big Smooth calls Diver's HTTP API
//! instead of touching PearlStore directly. This keeps pearl lifecycle
//! management centralized in Diver (with Jira sync, cost tracking, etc.)

use anyhow::{Context, Result};

/// Lightweight HTTP client for the Diver service.
#[derive(Clone)]
pub struct DiverClient {
    base_url: String,
    http: reqwest::Client,
}

impl DiverClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// Dispatch a task through Diver. Returns the pearl ID.
    pub async fn dispatch(&self, title: &str, description: &str, operator_id: Option<&str>) -> Result<String> {
        let body = serde_json::json!({
            "title": title,
            "description": description,
            "pearl_type": "task",
            "priority": 2,
            "operator_id": operator_id,
        });
        let resp = self
            .http
            .post(format!("{}/dispatch", self.base_url))
            .json(&body)
            .send()
            .await
            .context("diver dispatch request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("diver dispatch failed ({status}): {text}");
        }

        let result: serde_json::Value = resp.json().await.context("parse diver dispatch response")?;
        let pearl_id = result["pearl"]["id"].as_str().context("diver dispatch: no pearl.id in response")?;
        Ok(pearl_id.to_string())
    }

    /// Complete a task through Diver.
    pub async fn complete(&self, pearl_id: &str, summary: Option<&str>, cost_usd: Option<f64>) -> Result<()> {
        let body = serde_json::json!({
            "pearl_id": pearl_id,
            "summary": summary,
            "cost_usd": cost_usd,
        });
        let resp = self
            .http
            .post(format!("{}/complete", self.base_url))
            .json(&body)
            .send()
            .await
            .context("diver complete request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            tracing::warn!("diver complete failed ({status}): {text}");
        }
        Ok(())
    }

    /// Check if Diver is reachable.
    pub async fn health(&self) -> bool {
        self.http
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_trims_trailing_slash() {
        let client = DiverClient::new("http://localhost:1234/");
        assert_eq!(client.base_url, "http://localhost:1234");
    }

    #[tokio::test]
    async fn health_returns_false_when_unreachable() {
        let client = DiverClient::new("http://127.0.0.1:1");
        assert!(!client.health().await);
    }
}

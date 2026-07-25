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

    /// Fetch every issue in the configured project (all statuses), paginated.
    ///
    /// The description is flattened to the first paragraph's plain text.
    ///
    /// # Errors
    /// Fails if a search request cannot be sent, returns a non-success status, or the response is not valid JSON.
    pub async fn list_project_issues(&self) -> Result<Vec<JiraIssue>> {
        let mut issues = Vec::new();
        let mut next_page: Option<String> = None;
        loop {
            let base = format!(
                "{}/rest/api/3/search/jql?jql=project%3D{}+ORDER+BY+key+DESC&maxResults=100&fields=key,summary,status,description",
                self.config.url, self.config.project
            );
            let url = match next_page {
                Some(ref token) => format!("{base}&nextPageToken={token}"),
                None => base,
            };
            let resp = self
                .http
                .get(&url)
                .basic_auth(&self.config.email, Some(&self.config.api_token))
                .send()
                .await
                .context("jira: search issues")?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("jira: search failed ({status}): {body}");
            }
            let body: serde_json::Value = resp.json().await.context("jira: parse search response")?;
            for issue in body["issues"].as_array().unwrap_or(&Vec::new()) {
                issues.push(JiraIssue {
                    key: issue["key"].as_str().unwrap_or_default().to_string(),
                    summary: issue["fields"]["summary"].as_str().unwrap_or_default().to_string(),
                    status: issue["fields"]["status"]["name"].as_str().unwrap_or_default().to_string(),
                    description: adf_first_paragraph_text(&issue["fields"]["description"]),
                });
            }
            if body["isLast"].as_bool().unwrap_or(true) {
                break;
            }
            next_page = body["nextPageToken"].as_str().map(String::from);
        }
        Ok(issues)
    }
}

/// Pull the first paragraph's plain text out of an ADF description document.
fn adf_first_paragraph_text(doc: &serde_json::Value) -> Option<String> {
    let text = doc["content"].as_array()?.first()?["content"].as_array()?.first()?["text"].as_str()?;
    Some(text.to_string())
}

/// A pearl as the sync planner needs it: id, status (`open`/`in_progress`/`closed`), title.
#[derive(Debug, Clone)]
pub struct SyncPearl {
    pub id: String,
    pub status: String,
    pub title: String,
}

/// What a `jira sync` run would do. Computed purely from local pearls +
/// fetched Jira issues so it can be previewed (`--dry-run`) and unit-tested.
#[derive(Debug, Default)]
pub struct SyncPlan {
    /// Active pearls whose every referenced issue key is Done in Jira → close.
    pub close_pearls: Vec<SyncPearl>,
    /// Issue keys where every referencing pearl is closed but Jira is still open → transition to Done.
    pub transition_keys: Vec<String>,
    /// Open Jira keys no pearl references → pearl created only with `--pull`.
    pub untracked_jira: Vec<String>,
    /// Active pearls with no issue key in the title → Jira ticket created only with `--push`.
    pub unkeyed_pearls: Vec<SyncPearl>,
}

/// Extract `PROJECT-123` issue keys from a title.
///
/// Requires at least one digit after the dash and a non-alphanumeric boundary
/// before the project name, so placeholders like `SMOODEV-XXX` or
/// `XSMOODEV-1` never match.
pub fn extract_keys(title: &str, project: &str) -> Vec<String> {
    let needle = format!("{project}-");
    let mut keys = Vec::new();
    let mut from = 0;
    while let Some(pos) = title[from..].find(&needle) {
        let start = from + pos;
        from = start + needle.len();
        let boundary_ok = start == 0 || !title[..start].chars().next_back().is_some_and(char::is_alphanumeric);
        let digits: String = title[from..].chars().take_while(char::is_ascii_digit).collect();
        if boundary_ok && !digits.is_empty() {
            let key = format!("{project}-{digits}");
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

/// Compute the reconciliation plan between local pearls and Jira issues.
pub fn plan_sync(pearls: &[SyncPearl], jira: &[JiraIssue], project: &str) -> SyncPlan {
    use std::collections::HashMap;
    let jira_status: HashMap<&str, &str> = jira.iter().map(|i| (i.key.as_str(), i.status.as_str())).collect();

    let mut plan = SyncPlan::default();
    // active/closed pearl counts per referenced key
    let mut refs: HashMap<String, (u32, u32)> = HashMap::new();
    for pearl in pearls {
        let keys = extract_keys(&pearl.title, project);
        let active = pearl.status != "closed";
        for key in &keys {
            let entry = refs.entry(key.clone()).or_default();
            if active {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
        if active {
            if keys.is_empty() {
                plan.unkeyed_pearls.push(pearl.clone());
            } else if keys.iter().all(|k| jira_status.get(k.as_str()) == Some(&"Done")) {
                plan.close_pearls.push(pearl.clone());
            }
        }
    }
    for issue in jira {
        if issue.status == "Done" {
            continue;
        }
        match refs.get(&issue.key) {
            None => plan.untracked_jira.push(issue.key.clone()),
            Some((0, closed)) if *closed > 0 => plan.transition_keys.push(issue.key.clone()),
            Some(_) => {}
        }
    }
    plan.transition_keys.sort();
    plan.untracked_jira.sort();
    plan
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

    #[test]
    fn extract_keys_matches_real_keys_only() {
        assert_eq!(extract_keys("SMOODEV-42: fix thing", "SMOODEV"), vec!["SMOODEV-42"]);
        assert_eq!(extract_keys("re-fix (SMOODEV-7) and SMOODEV-8", "SMOODEV"), vec!["SMOODEV-7", "SMOODEV-8"]);
        // no digits, embedded prefix, wrong project → no match
        assert!(extract_keys("SMOODEV-XXX: placeholder", "SMOODEV").is_empty());
        assert!(extract_keys("XSMOODEV-1: not ours", "SMOODEV").is_empty());
        assert!(extract_keys("OTHER-12: different project", "SMOODEV").is_empty());
        // duplicates collapse
        assert_eq!(extract_keys("SMOODEV-5 dup SMOODEV-5", "SMOODEV"), vec!["SMOODEV-5"]);
    }

    fn pearl(id: &str, status: &str, title: &str) -> SyncPearl {
        SyncPearl {
            id: id.into(),
            status: status.into(),
            title: title.into(),
        }
    }

    fn issue(key: &str, status: &str) -> JiraIssue {
        JiraIssue {
            key: key.into(),
            summary: String::new(),
            status: status.into(),
            description: None,
        }
    }

    #[test]
    fn plan_closes_pearls_only_when_every_key_is_done() {
        let pearls = [
            pearl("th-1", "open", "SMOODEV-1: done in jira"),
            pearl("th-2", "in_progress", "SMOODEV-1 + SMOODEV-2 spans two"),
            pearl("th-3", "open", "SMOODEV-9: key missing from jira"),
        ];
        let jira = [issue("SMOODEV-1", "Done"), issue("SMOODEV-2", "To Do")];
        let plan = plan_sync(&pearls, &jira, "SMOODEV");
        let ids: Vec<_> = plan.close_pearls.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["th-1"]);
    }

    #[test]
    fn plan_transitions_jira_only_when_all_referencing_pearls_closed() {
        let pearls = [
            pearl("th-1", "closed", "SMOODEV-1: shipped"),
            pearl("th-2", "closed", "SMOODEV-2: shipped"),
            pearl("th-3", "open", "SMOODEV-2: follow-up still active"),
        ];
        let jira = [issue("SMOODEV-1", "In Progress"), issue("SMOODEV-2", "In Progress"), issue("SMOODEV-3", "Done")];
        let plan = plan_sync(&pearls, &jira, "SMOODEV");
        assert_eq!(plan.transition_keys, vec!["SMOODEV-1"]);
    }

    #[test]
    fn plan_reports_untracked_and_unkeyed_without_acting_on_them() {
        let pearls = [pearl("th-1", "open", "no key here"), pearl("th-2", "closed", "also unkeyed, but closed")];
        let jira = [issue("SMOODEV-1", "To Do"), issue("SMOODEV-2", "Done")];
        let plan = plan_sync(&pearls, &jira, "SMOODEV");
        assert_eq!(plan.untracked_jira, vec!["SMOODEV-1"]); // Done tickets never pulled
        let ids: Vec<_> = plan.unkeyed_pearls.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["th-1"]); // closed pearls never pushed
        assert!(plan.close_pearls.is_empty());
        assert!(plan.transition_keys.is_empty());
    }

    #[test]
    fn adf_description_flattens_first_paragraph() {
        let doc = serde_json::json!({"content":[{"content":[{"text":"hello world"}]}]});
        assert_eq!(adf_first_paragraph_text(&doc), Some("hello world".to_string()));
        assert_eq!(adf_first_paragraph_text(&serde_json::Value::Null), None);
    }
}

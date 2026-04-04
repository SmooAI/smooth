//! Jira REST API client — bidirectional sync with Beads.

use serde::{Deserialize, Serialize};

/// Jira configuration (from SQLite config table).
#[derive(Debug, Clone)]
pub struct JiraConfig {
    pub url: String,
    pub project: String,
    pub email: String,
    pub api_token: String,
}

impl JiraConfig {
    /// Load Jira config from database + environment.
    pub fn from_db(db: &crate::db::Database) -> Option<Self> {
        let url = db.get_config("jira.url").ok()??;
        let project = db.get_config("jira.project").ok()??;
        let email = std::env::var("JIRA_EMAIL").ok().or_else(|| db.get_config("jira.email").ok()?)?;
        let api_token = std::env::var("JIRA_API_TOKEN").ok().or_else(|| db.get_config("jira.api_token").ok()?)?;
        Some(Self {
            url,
            project,
            email,
            api_token,
        })
    }
}

/// Jira issue (simplified).
#[derive(Debug, Serialize, Deserialize)]
pub struct JiraIssue {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub priority: Option<String>,
}

/// Jira sync result.
#[derive(Debug, Serialize)]
pub struct SyncResult {
    pub pulled: u32,
    pub pushed: u32,
    pub conflicts: u32,
}

/// Sync status.
#[derive(Debug, Serialize)]
pub struct SyncStatus {
    pub connected: bool,
    pub last_sync: Option<String>,
    pub pending_changes: u32,
}

/// Check Jira connection.
pub async fn check_connection(config: &JiraConfig) -> bool {
    let url = format!("{}/rest/api/3/myself", config.url);
    let client = reqwest::Client::new();
    client
        .get(&url)
        .basic_auth(&config.email, Some(&config.api_token))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_result_serializes() {
        let result = SyncResult {
            pulled: 5,
            pushed: 2,
            conflicts: 0,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"pulled\":5"));
    }
}

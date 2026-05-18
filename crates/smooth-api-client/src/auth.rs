//! `th login` device-flow handshake.
//!
//! Two-step flow because `th` may be running over SSH with no
//! browser:
//!
//! 1. `start_login(base_url)` POSTs to `/cli-auth/start`. The server
//!    creates a short-lived device code and returns a `user_code` plus
//!    a `verification_url`. The CLI prints "open <verification_url>
//!    and enter code: ABCD-1234".
//! 2. `poll_until_complete(...)` polls `/cli-auth/status?device_code=…`
//!    every few seconds. When the user clicks "Authorize this CLI" in
//!    the web app, the endpoint flips to `approved` and returns the
//!    actual access/refresh tokens. The CLI persists them via
//!    `CredentialsStore::save`.
//!
//! The endpoints aren't part of the public OpenAPI spec yet (they're
//! still being added in smooai/packages/backend/src/routes/cli-auth.ts
//! — pearl SMOODEV-cli-auth, separate ticket). Until they ship the
//! `start_login` call will 404; the calls are stubbed here so the
//! client-side surface lands now and we can wire the server side in
//! parallel.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::credentials::Credentials;

/// Response from `POST /cli-auth/start`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginStart {
    /// Long opaque token the CLI keeps; included in every poll.
    pub device_code: String,
    /// Short human-readable code the user types into the web page.
    pub user_code: String,
    /// URL the CLI prints / opens in the browser.
    pub verification_url: String,
    /// How long the device_code is valid. Default 600s.
    pub expires_in: u64,
    /// How often to poll (seconds). Default 5.
    #[serde(default = "default_interval")]
    pub interval: u64,
}

const fn default_interval() -> u64 {
    5
}

/// Possible states reported by `/cli-auth/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoginStatus {
    /// User hasn't clicked "Authorize" yet. Keep polling.
    Pending,
    /// Authorized. Tokens are inline.
    Approved {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_at: Option<DateTime<Utc>>,
        #[serde(default)]
        user: Option<String>,
        #[serde(default)]
        active_org_id: Option<String>,
    },
    /// User clicked "Deny" or the device code expired.
    Denied,
    /// Code expired before any decision.
    Expired,
}

/// Kick off the device-flow handshake.
///
/// # Errors
/// Network errors and 4xx responses both surface.
pub async fn start_login(base_url: &str, http: &reqwest::Client) -> Result<LoginStart> {
    let resp = http
        .post(format!("{}/cli-auth/start", base_url.trim_end_matches('/')))
        .send()
        .await
        .context("POST /cli-auth/start")?;
    if !resp.status().is_success() {
        anyhow::bail!("/cli-auth/start returned HTTP {}", resp.status());
    }
    let body: LoginStart = resp.json().await.context("parse /cli-auth/start response")?;
    Ok(body)
}

/// Poll `/cli-auth/status` until the user approves (or denies, or the
/// code expires). Sleeps `start.interval` seconds between polls.
///
/// # Errors
/// Network errors propagate. Denied / expired states return an error
/// with a human-readable message.
pub async fn poll_until_complete(base_url: &str, http: &reqwest::Client, start: &LoginStart) -> Result<Credentials> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(start.expires_in);
    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("device code expired before approval");
        }
        tokio::time::sleep(std::time::Duration::from_secs(start.interval)).await;

        let resp = http
            .get(format!("{}/cli-auth/status", base_url.trim_end_matches('/')))
            .query(&[("device_code", start.device_code.as_str())])
            .send()
            .await
            .context("GET /cli-auth/status")?;
        if !resp.status().is_success() {
            anyhow::bail!("/cli-auth/status returned HTTP {}", resp.status());
        }
        match resp.json::<LoginStatus>().await.context("parse /cli-auth/status response")? {
            LoginStatus::Pending => continue,
            LoginStatus::Denied => anyhow::bail!("login denied"),
            LoginStatus::Expired => anyhow::bail!("device code expired"),
            LoginStatus::Approved {
                access_token,
                refresh_token,
                expires_at,
                user,
                active_org_id,
            } => {
                return Ok(Credentials {
                    access_token,
                    refresh_token,
                    expires_at,
                    user,
                    active_org_id,
                    created_at: Utc::now(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_status_pending_parses() {
        let raw = r#"{"status":"pending"}"#;
        let parsed: LoginStatus = serde_json::from_str(raw).expect("parse pending");
        assert!(matches!(parsed, LoginStatus::Pending));
    }

    #[test]
    fn login_status_approved_parses_with_minimum_fields() {
        let raw = r#"{"status":"approved","access_token":"abc"}"#;
        let parsed: LoginStatus = serde_json::from_str(raw).expect("parse approved");
        match parsed {
            LoginStatus::Approved { access_token, refresh_token, .. } => {
                assert_eq!(access_token, "abc");
                assert!(refresh_token.is_none());
            }
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    #[test]
    fn login_status_approved_parses_with_full_payload() {
        let raw = r#"{
            "status":"approved",
            "access_token":"jwt",
            "refresh_token":"rfsh",
            "expires_at":"2030-01-01T00:00:00Z",
            "user":"brent@smoo.ai",
            "active_org_id":"org_abc"
        }"#;
        let parsed: LoginStatus = serde_json::from_str(raw).expect("parse approved full");
        match parsed {
            LoginStatus::Approved {
                access_token,
                refresh_token,
                expires_at,
                user,
                active_org_id,
            } => {
                assert_eq!(access_token, "jwt");
                assert_eq!(refresh_token.as_deref(), Some("rfsh"));
                assert_eq!(user.as_deref(), Some("brent@smoo.ai"));
                assert_eq!(active_org_id.as_deref(), Some("org_abc"));
                assert!(expires_at.is_some());
            }
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    #[test]
    fn login_start_default_interval_is_5s() {
        let raw = r#"{"device_code":"x","user_code":"AB-12","verification_url":"https://x","expires_in":600}"#;
        let parsed: LoginStart = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.interval, 5);
    }
}

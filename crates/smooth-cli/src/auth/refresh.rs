//! Silent auto-refresh helpers for both auth flavors.
//!
//! - [`refresh_user_session`] exchanges a stored Supabase `refresh_token`
//!   for a fresh `access_token` at `{supabase}/auth/v1/token`.
//! - [`refresh_m2m_session`] re-runs the OAuth 2.0 `client_credentials`
//!   grant at `auth.smoo.ai/token` using a stored client_id / client_secret.
//!
//! Both used to live tucked away in `config.rs`. They're reused by
//! `th auth whoami` and the `ConfigClient` so an expired session
//! re-mints transparently when refresh material is on disk.

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;
use smooai_client_shared::auth::storage::{CredentialKind, Credentials, CredentialsStore};

use crate::auth::{supabase_url, PROD_SUPABASE_ANON_KEY};

/// Load the `th auth login` user session, silently refreshing it when
/// expired and persisting the rotated `refresh_token` (pearl th-32d00e —
/// login is once-per-machine; the session lives as long as its refresh
/// token). Errors, each with a `th auth login` hint, only when: no
/// session exists, the session is expired with no refresh material, or
/// the refresh grant itself fails.
pub async fn fresh_user_credentials(http: &reqwest::Client) -> Result<Credentials> {
    let store = CredentialsStore::default_user().context("locate user credentials store")?;
    fresh_user_credentials_from(http, &store).await
}

/// [`fresh_user_credentials`] against an explicit store (testable).
pub async fn fresh_user_credentials_from(http: &reqwest::Client, store: &CredentialsStore) -> Result<Credentials> {
    let creds = store
        .load()
        .context("load user session")?
        .ok_or_else(|| anyhow::anyhow!("not logged in as a user — run `th auth login` first"))?;
    if !creds.is_expired() {
        return Ok(creds);
    }
    if creds.refresh_token.is_none() {
        anyhow::bail!("user session expired and has no refresh token — run `th auth login` again");
    }
    let refreshed = refresh_user_session(http, &creds)
        .await
        .context("silent session refresh failed — run `th auth login` again")?;
    // Best-effort persist: the in-memory token still serves this run
    // even if the write fails (read-only FS, perms).
    let _ = store.save(&refreshed);
    Ok(refreshed)
}

/// Exchange the stored Supabase `refresh_token` for a fresh
/// `access_token` + new `refresh_token`. Preserves the user-display
/// fields (`user`, `active_org_id`) from the previous credentials.
///
/// # Errors
/// - Credentials carry no `refresh_token`
/// - Network failure POSTing to `/auth/v1/token`
/// - Supabase returns non-2xx (refresh_token revoked, anon key wrong,
///   Supabase project paused, etc.)
/// - Response missing `access_token`
pub async fn refresh_user_session(http: &reqwest::Client, previous: &Credentials) -> Result<Credentials> {
    let refresh_token = previous
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("session has no refresh_token — re-run `th auth login`"))?;
    let supabase_url = supabase_url();
    let anon_key = std::env::var("SMOOAI_SUPABASE_ANON_KEY").unwrap_or_else(|_| PROD_SUPABASE_ANON_KEY.to_string());

    let url = format!("{}/auth/v1/token?grant_type=refresh_token", supabase_url.trim_end_matches('/'));
    let resp = http
        .post(&url)
        .header("apikey", &anon_key)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "refresh_token": refresh_token }))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("refresh_token grant returned HTTP {status}: {text} (re-run `th auth login`)");
    }
    let body: Value = serde_json::from_str(&text).with_context(|| format!("parse refresh response: {text}"))?;
    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("refresh response missing access_token: {text}"))?
        .to_string();
    let new_refresh = body.get("refresh_token").and_then(|v| v.as_str()).map(str::to_string);
    let expires_in = body.get("expires_in").and_then(serde_json::Value::as_u64);
    let expires_at = expires_in.map(|s| Utc::now() + chrono::Duration::seconds(i64::try_from(s).unwrap_or(3600)));
    let user_display = body
        .get("user")
        .and_then(|u| u.get("email").and_then(|e| e.as_str()).or_else(|| u.get("id").and_then(|i| i.as_str())))
        .map(str::to_string)
        .or_else(|| previous.user.clone());
    Ok(Credentials {
        access_token,
        refresh_token: new_refresh.or_else(|| previous.refresh_token.clone()),
        expires_at,
        user: user_display,
        active_org_id: previous.active_org_id.clone(),
        client_id: None,
        client_secret: None,
        kind: CredentialKind::User,
        created_at: previous.created_at,
    })
}

/// Re-run the OAuth `client_credentials` grant against `auth.smoo.ai`
/// using the stored client_id + client_secret. Preserves
/// `active_org_id` from the previous credentials.
///
/// # Errors
/// - Credentials carry no `client_id` / `client_secret`
/// - Network failure / non-2xx from `auth.smoo.ai`
pub async fn refresh_m2m_session(http: &reqwest::Client, previous: &Credentials) -> Result<Credentials> {
    let (Some(cid), Some(csecret)) = (previous.client_id.as_deref(), previous.client_secret.as_deref()) else {
        anyhow::bail!("M2M session has no stored client_id / client_secret — re-run `th auth login --m2m`");
    };
    use smooai_client_shared::auth::m2m::client_credentials_grant;
    let mut refreshed = client_credentials_grant(http, cid, csecret).await.context("client_credentials grant")?;
    refreshed.active_org_id = previous.active_org_id.clone();
    Ok(refreshed)
}

#[cfg(test)]
mod fresh_user_credentials_tests {
    use super::*;

    fn write_creds(dir: &std::path::Path, expires_at: Option<chrono::DateTime<Utc>>, refresh: Option<&str>) -> CredentialsStore {
        let store = CredentialsStore::at(dir.join("smooai-user.json"));
        store
            .save(&Credentials {
                access_token: "tok".into(),
                refresh_token: refresh.map(str::to_string),
                expires_at,
                user: Some("u@example.com".into()),
                active_org_id: None,
                client_id: None,
                client_secret: None,
                kind: CredentialKind::User,
                created_at: Utc::now(),
            })
            .unwrap();
        store
    }

    fn http() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[tokio::test]
    async fn valid_session_passes_through_without_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let store = write_creds(dir.path(), Some(Utc::now() + chrono::Duration::hours(1)), None);
        let creds = fresh_user_credentials_from(&http(), &store).await.unwrap();
        assert_eq!(creds.access_token, "tok");
    }

    #[tokio::test]
    async fn missing_session_says_login() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsStore::at(dir.path().join("smooai-user.json"));
        let err = fresh_user_credentials_from(&http(), &store).await.unwrap_err();
        assert!(format!("{err:#}").contains("th auth login"));
    }

    #[tokio::test]
    async fn expired_without_refresh_token_says_login_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = write_creds(dir.path(), Some(Utc::now() - chrono::Duration::hours(1)), None);
        let err = fresh_user_credentials_from(&http(), &store).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("no refresh token") && msg.contains("th auth login"), "got: {msg}");
    }

    // The expired-with-refresh-token path exercises the live Supabase
    // grant; its request/response shape is covered by the whoami and
    // browser_login tests. Here we only pin that the branch is taken
    // (a bogus refresh_token fails with the login hint, not a panic).
    #[tokio::test]
    async fn expired_with_bad_refresh_token_fails_with_login_hint() {
        let dir = tempfile::tempdir().unwrap();
        // Point Supabase at a closed port so the test never leaves the
        // box; save/restore the env var like supabase_url_honors_env_override.
        let prev = std::env::var("SMOOAI_SUPABASE_URL").ok();
        std::env::set_var("SMOOAI_SUPABASE_URL", "http://127.0.0.1:9");
        let store = write_creds(dir.path(), Some(Utc::now() - chrono::Duration::hours(1)), Some("bogus"));
        let result = fresh_user_credentials_from(&http(), &store).await;
        match prev {
            Some(v) => std::env::set_var("SMOOAI_SUPABASE_URL", v),
            None => std::env::remove_var("SMOOAI_SUPABASE_URL"),
        }
        assert!(format!("{:#}", result.unwrap_err()).contains("th auth login"));
    }
}

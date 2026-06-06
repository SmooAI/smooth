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
use smooai_client_shared::auth::storage::{CredentialKind, Credentials};

use crate::auth::{supabase_url, PROD_SUPABASE_ANON_KEY};

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

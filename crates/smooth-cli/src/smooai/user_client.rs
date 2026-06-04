//! User-session HTTP client for `api.smoo.ai` routes that require a
//! *user* bearer (Supabase JWT), not an M2M `client_credentials`
//! token.
//!
//! `SmoothApiClient` (used by most `th api …` commands) is built
//! around M2M and auto-refreshes via the client_credentials grant.
//! A handful of routes — `/organizations`, `/organizations/{id}`,
//! and the CRM contacts endpoints when you want writes attributed to
//! a real person — require the user kind and 401 under M2M
//! ("auth kind does not satisfy route requirement"). This client
//! loads the session `th auth login` creates
//! (`~/.smooth/auth/smooai-user.json`) and sends it as a bearer.
//!
//! No auto-refresh — Supabase tokens expire after ~1h; on 401 the
//! user re-runs `th auth login`. This mirrors `admin::client::
//! AdminClient` but with neutral (non-admin) error messages so it
//! can front any user-authenticated API surface. SMOODEV-1735.

use anyhow::{Context, Result};
use serde_json::Value;
use smooai_client_shared::auth::storage::CredentialsStore;

use crate::admin::client::api_url;

/// Authenticated client for user-bearer API calls.
pub struct UserClient {
    base: String,
    bearer: String,
    http: reqwest::Client,
}

impl UserClient {
    /// Build by loading the user JWT from
    /// `~/.smooth/auth/smooai-user.json`. Errors with a
    /// `th auth login` hint if no (valid) session is present.
    pub fn from_user_session() -> Result<Self> {
        let store = CredentialsStore::default_user().context("locate user credentials store")?;
        let creds = store
            .load()
            .context("load user session")?
            .ok_or_else(|| anyhow::anyhow!("not logged in as a user — run `th auth login` first"))?;
        if creds.is_expired() {
            anyhow::bail!("user session expired — run `th auth login` again");
        }
        Ok(Self {
            base: api_url(),
            bearer: creds.access_token,
            http: reqwest::Client::builder().user_agent(format!("th/{}", env!("CARGO_PKG_VERSION"))).build()?,
        })
    }

    /// Identity (email) behind the loaded session, best-effort — used
    /// for the "importing as <user>" banner.
    pub fn user_label() -> Option<String> {
        let store = CredentialsStore::default_user().ok()?;
        store.load().ok().flatten().and_then(|c| c.user)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let resp = self.http.get(&url).bearer_auth(&self.bearer).send().await.with_context(|| format!("GET {url}"))?;
        Self::body(resp, "GET", &url).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.bearer)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        Self::body(resp, "POST", &url).await
    }

    pub async fn patch(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let resp = self
            .http
            .patch(&url)
            .bearer_auth(&self.bearer)
            .json(body)
            .send()
            .await
            .with_context(|| format!("PATCH {url}"))?;
        Self::body(resp, "PATCH", &url).await
    }

    async fn body(resp: reqwest::Response, method: &str, url: &str) -> Result<Value> {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            if status.as_u16() == 401 {
                anyhow::bail!("{method} {url} returned 401 — run `th auth login` to refresh your user session");
            }
            anyhow::bail!("{method} {url} returned HTTP {status}: {text}");
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).with_context(|| format!("parse response from {method} {url}: {text}"))
    }
}

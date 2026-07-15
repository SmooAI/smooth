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
//! Auto-refreshes an expired session via the stored Supabase
//! `refresh_token` (pearl th-32d00e), so `th auth login` is
//! once-per-machine. This mirrors `admin::client::AdminClient` but
//! with neutral (non-admin) error messages so it can front any
//! user-authenticated API surface. SMOODEV-1735.

use anyhow::{Context, Result};
use serde_json::Value;
use smooai_client_shared::auth::storage::CredentialsStore;

/// `https://api.smoo.ai` by default; override with `SMOOAI_API_URL`.
///
/// Inlined here (rather than reused from the admin module) so this
/// user-JWT client compiles in builds that exclude the `admin`
/// feature. Both helpers read the same `SMOOAI_API_URL` env var.
fn api_url() -> String {
    std::env::var("SMOOAI_API_URL").unwrap_or_else(|_| "https://api.smoo.ai".to_string())
}

/// Authenticated client for user-bearer API calls.
pub struct UserClient {
    base: String,
    bearer: String,
    http: reqwest::Client,
}

impl UserClient {
    /// Build by loading the user JWT from
    /// `~/.smooth/auth/smooai-user.json`, silently refreshing an
    /// expired session via its Supabase `refresh_token` (pearl
    /// th-32d00e). Errors with a `th auth login` hint only when no
    /// session exists or there's no refresh material.
    pub async fn from_user_session() -> Result<Self> {
        let http = reqwest::Client::builder().user_agent(format!("th/{}", env!("CARGO_PKG_VERSION"))).build()?;
        let creds = crate::auth::refresh::fresh_user_credentials(&http).await?;
        Ok(Self {
            base: api_url(),
            bearer: creds.access_token,
            http,
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
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.bearer)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
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

    pub async fn put(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let resp = self
            .http
            .put(&url)
            .bearer_auth(&self.bearer)
            .json(body)
            .send()
            .await
            .with_context(|| format!("PUT {url}"))?;
        Self::body(resp, "PUT", &url).await
    }

    pub async fn delete(&self, path: &str) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let resp = self
            .http
            .delete(&url)
            .bearer_auth(&self.bearer)
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        Self::body(resp, "DELETE", &url).await
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

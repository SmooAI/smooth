//! Thin HTTP client for the `/admin/*` endpoints on `api.smoo.ai`.
//!
//! Loads the Supabase user JWT from
//! `~/.smooth/auth/smooai-user.json` (the session `th auth login`
//! creates), sends every request with `Authorization: Bearer
//! <jwt>`. No auto-refresh — Supabase tokens expire after ~1h; on
//! 401 the user re-runs `th auth login`.
//!
//! Distinct from `smooth-api-client::SmoothApiClient` which is
//! built around M2M `client_credentials` and auto-refreshes via
//! that grant. The two flows have different refresh semantics, so
//! they don't share an HTTP layer.

use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use serde::Serialize;
use smooai_client_shared::auth::storage::CredentialsStore;

/// `https://api.smoo.ai` by default; override with `SMOOAI_API_URL`.
pub const DEFAULT_API_URL: &str = "https://api.smoo.ai";

/// Resolve the API base URL.
#[must_use]
pub fn api_url() -> String {
    std::env::var("SMOOAI_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string())
}

/// Authenticated client for `/admin/*` calls.
pub struct AdminClient {
    base: String,
    bearer: String,
    http: reqwest::Client,
}

impl AdminClient {
    /// Build by loading the user JWT from
    /// `~/.smooth/auth/smooai-user.json`. Errors with a
    /// `th auth login` hint if no session is present.
    pub fn from_user_session() -> Result<Self> {
        let store = CredentialsStore::default_user().context("locate user credentials store")?;
        let creds = store
            .load()
            .context("load user session")?
            .ok_or_else(|| anyhow::anyhow!("not logged in — run `th auth login` first"))?;
        if creds.is_expired() {
            anyhow::bail!("user session expired (run `th auth login` again)");
        }
        Ok(Self {
            base: api_url(),
            bearer: creds.access_token,
            http: reqwest::Client::builder().user_agent(format!("th/{}", env!("CARGO_PKG_VERSION"))).build()?,
        })
    }

    /// Send a GET and return the parsed JSON body.
    ///
    /// # Errors
    /// Network failures + non-2xx responses (which include the
    /// upstream error body verbatim — typically a clear
    /// `{"error": "..."}` from the backend).
    pub async fn get(&self, path: &str) -> Result<serde_json::Value> {
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

    /// Send a POST with a JSON body and return the parsed response.
    pub async fn post<B: Serialize>(&self, path: &str, body: &B) -> Result<serde_json::Value> {
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

    /// Send a DELETE and return the parsed response (often empty).
    pub async fn delete(&self, path: &str) -> Result<serde_json::Value> {
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

    /// Send a PUT with a JSON body and return the parsed response.
    pub async fn put<B: Serialize>(&self, path: &str, body: &B) -> Result<serde_json::Value> {
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

    async fn body(resp: reqwest::Response, method: &str, url: &str) -> Result<serde_json::Value> {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            if status.as_u16() == 401 {
                anyhow::bail!("{method} {url} returned 401 — run `th auth login` to refresh your session");
            }
            if status.as_u16() == 403 {
                anyhow::bail!("{method} {url} returned 403 — your user lacks the requireSuperAdmin role");
            }
            anyhow::bail!("{method} {url} returned HTTP {status}: {text}");
        }
        if text.trim().is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_str(&text).with_context(|| format!("parse response from {method} {url}: {text}"))
    }
}

/// Print a JSON value as pretty-indented JSON to stdout.
pub fn print_json(value: &serde_json::Value) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(_) => println!("{value}"),
    }
}

/// Print a one-line status hint when an operation succeeded.
pub fn print_ok(msg: impl AsRef<str>) {
    println!("{} {}", "✓".green().bold(), msg.as_ref());
}

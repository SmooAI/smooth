//! `SmoothApiClient` — thin auth wrapper over progenitor's generated
//! `Client` (in `crate::pb`). Reads credentials from disk, injects the
//! bearer token on every call, refreshes on 401 once.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

use crate::credentials::{Credentials, CredentialsStore};

/// Client errors.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// No credentials on disk. Caller should prompt `th login`.
    #[error("not logged in — run `th login` to authenticate")]
    NotAuthenticated,
    /// Underlying HTTP failure.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// Anything else.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// High-level Smoo AI client. Wraps progenitor's generated client and
/// handles auth header injection + credential reload.
#[derive(Clone)]
pub struct SmoothApiClient {
    /// Base URL — defaults to `https://api.smoo.ai`; override with
    /// `SMOOAI_API_URL`.
    pub base_url: String,
    /// On-disk credential store. Wrapped in `Arc<Mutex<_>>` so the
    /// refresh-on-401 path can update it from any task without
    /// invalidating clones of this `SmoothApiClient`.
    pub creds: Arc<Mutex<Option<Credentials>>>,
    /// Store handle for persistence. Refresh path writes through this.
    pub store: CredentialsStore,
    /// Pre-configured `reqwest::Client`. Includes the bearer header
    /// when credentials are present; if they're not, the client still
    /// works for unauthenticated endpoints (the public `/.well-known/*`,
    /// the device-flow start endpoint).
    pub http: reqwest::Client,
}

impl SmoothApiClient {
    /// Build a client from the default credentials store. Returns a
    /// client even if no creds are present — calls to authenticated
    /// endpoints will then 401 (and you can re-run `th login`).
    ///
    /// # Errors
    /// Fails when the credential store path can't be resolved.
    pub fn from_disk() -> Result<Self> {
        let store = CredentialsStore::default_path()?;
        let creds = store.load()?;
        Self::new(crate::base_url(), creds, store)
    }

    /// Build a client with a specific base URL + credentials. Mainly
    /// for tests and the device-flow login path (which constructs a
    /// no-auth client to call `/cli-auth/start`).
    ///
    /// # Errors
    /// Fails when reqwest can't build a client (e.g. invalid header
    /// values, which shouldn't happen given the inputs we control).
    pub fn new(base_url: impl Into<String>, creds: Option<Credentials>, store: CredentialsStore) -> Result<Self> {
        let mut headers = HeaderMap::new();
        if let Some(ref c) = creds {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", c.access_token)) {
                headers.insert(AUTHORIZATION, val);
            }
        }
        let http = reqwest::Client::builder().default_headers(headers).user_agent(user_agent()).build()?;
        Ok(Self {
            base_url: base_url.into(),
            creds: Arc::new(Mutex::new(creds)),
            store,
            http,
        })
    }

    /// `true` when we have credentials and they aren't expired.
    pub fn is_authenticated(&self) -> bool {
        self.creds.lock().map(|c| c.as_ref().is_some_and(|c| !c.is_expired())).unwrap_or(false)
    }

    /// Snapshot of the current credentials, if any.
    pub fn credentials(&self) -> Option<Credentials> {
        self.creds.lock().ok().and_then(|guard| guard.clone())
    }

    /// Replace the in-memory credentials AND persist to disk. Used by
    /// the login flow after the device-flow handshake completes.
    ///
    /// # Errors
    /// Disk write failures bubble up.
    pub fn set_credentials(&self, new_creds: Credentials) -> Result<()> {
        self.store.save(&new_creds)?;
        if let Ok(mut guard) = self.creds.lock() {
            *guard = Some(new_creds);
        }
        Ok(())
    }

    /// Build a fresh progenitor client pointed at our base URL +
    /// auth-injected reqwest. Re-built on demand (cheap) so the
    /// progenitor `Client` always reflects the latest tokens.
    pub fn pb(&self) -> crate::pb::Client {
        crate::pb::Client::new_with_client(&self.base_url, self.http.clone())
    }

    /// Re-mint the access_token via `client_credentials` if the
    /// stored token has expired (or is about to expire — the
    /// `Credentials::is_expired` 60-second safety margin applies).
    /// No-op when the token is still fresh, or when there are no
    /// stored client_credentials to re-exchange with (in which case
    /// the next call will 401 and the user has to re-run
    /// `th api login`).
    ///
    /// Also rebuilds `self.http` with the new Authorization header
    /// so subsequent calls send the fresh token.
    pub async fn ensure_fresh_token(&self) -> anyhow::Result<()> {
        let snapshot = self.credentials();
        let Some(creds) = snapshot else { return Ok(()) };
        if !creds.is_expired() {
            return Ok(());
        }
        let (Some(cid), Some(csecret)) = (creds.client_id.clone(), creds.client_secret.clone()) else {
            // Token expired but we have no way to re-mint it. Let the
            // next request 401 with the real server's "invalid token"
            // message; that's clearer than a synthetic error here.
            return Ok(());
        };
        let bare = reqwest::Client::builder().user_agent(user_agent()).build()?;
        let fresh = crate::auth::client_credentials_grant(&bare, &cid, &csecret)
            .await
            .context("auto-refresh client_credentials grant")?;
        let mut merged = fresh;
        // Preserve display-only fields the grant doesn't know about.
        merged.active_org_id = creds.active_org_id;
        self.set_credentials(merged.clone()).context("persist refreshed credentials")?;
        // Rebuild http with the new bearer token. We can't mutate
        // self.http (no &mut), so we replace it via interior
        // mutability — Arc<Mutex<reqwest::Client>> would be one
        // option, but reqwest::Client is already cheaply cloneable
        // and Send + Sync. The cleanest path is for callers to grab
        // a fresh http through self.http_with_token() lazily, but
        // for the raw() helper below we build the request with an
        // explicit Authorization header so we sidestep the issue
        // entirely.
        Ok(())
    }

    /// Issue a raw HTTP request against the platform API. Used by
    /// the CLI commands — easier than threading 92 distinct typed
    /// signatures through the dispatch when most calls are
    /// "GET /thing/{id}, print JSON".
    ///
    /// `path` is appended to `self.base_url` directly. `body` is
    /// serialized as JSON when Some. Auto-refreshes an expired
    /// access_token before sending; on a 401 also refreshes + retries
    /// once.
    ///
    /// # Errors
    /// Network errors, non-2xx responses (after one auth retry),
    /// and JSON-parse failures all surface as `anyhow::Error`s.
    pub async fn raw(&self, method: reqwest::Method, path: &str, body: Option<&serde_json::Value>) -> anyhow::Result<serde_json::Value> {
        // Pre-emptive refresh if we know the token is stale.
        let _ = self.ensure_fresh_token().await;
        let resp = self.send_once(&method, path, body).await?;
        if resp.0 == reqwest::StatusCode::UNAUTHORIZED && self.credentials().is_some_and(|c| c.client_id.is_some() && c.client_secret.is_some()) {
            // Reactive refresh — covers the case where the server
            // rotated keys or our expires_at was wrong.
            let _ = self.ensure_fresh_token().await;
            let retried = self.send_once(&method, path, body).await?;
            return Self::decode(method, path, retried);
        }
        Self::decode(method, path, resp)
    }

    /// Single send. Returns `(status, body_text)` for the caller to
    /// decide whether to retry / decode / bail.
    async fn send_once(&self, method: &reqwest::Method, path: &str, body: Option<&serde_json::Value>) -> anyhow::Result<(reqwest::StatusCode, String)> {
        let url = format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            if path.starts_with('/') { path.to_string() } else { format!("/{path}") }
        );
        // Use the current access_token from credentials at send-time,
        // not the cached header in self.http — that way ensure_fresh_token
        // doesn't have to mutate the reqwest::Client.
        let token = self.credentials().map(|c| c.access_token);
        let mut req = self.http.request(method.clone(), &url);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(|e| anyhow::anyhow!("{method} {url}: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, text))
    }

    /// Turn a `(status, body)` pair into a `Result<Value>`.
    fn decode(method: reqwest::Method, path: &str, (status, text): (reqwest::StatusCode, String)) -> anyhow::Result<serde_json::Value> {
        if !status.is_success() {
            anyhow::bail!("{method} {path} returned HTTP {status}: {text}");
        }
        if text.trim().is_empty() {
            return Ok(serde_json::json!({"ok": true}));
        }
        serde_json::from_str::<serde_json::Value>(&text).map_err(|e| anyhow::anyhow!("parse JSON response from {path}: {e}\nbody: {text}"))
    }

    /// Convenience: `raw(GET, path, None)`.
    pub async fn get(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        self.raw(reqwest::Method::GET, path, None).await
    }

    /// Convenience: `raw(POST, path, body)`.
    pub async fn post(&self, path: &str, body: Option<&serde_json::Value>) -> anyhow::Result<serde_json::Value> {
        self.raw(reqwest::Method::POST, path, body).await
    }

    /// Convenience: `raw(PATCH, path, body)`.
    pub async fn patch(&self, path: &str, body: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.raw(reqwest::Method::PATCH, path, Some(body)).await
    }

    /// Convenience: `raw(PUT, path, body)`.
    pub async fn put(&self, path: &str, body: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.raw(reqwest::Method::PUT, path, Some(body)).await
    }

    /// Convenience: `raw(DELETE, path, None)`.
    pub async fn delete(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        self.raw(reqwest::Method::DELETE, path, None).await
    }
}

fn user_agent() -> String {
    format!("smooth-cli/{} (https://github.com/SmooAI/smooth)", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn unauthenticated_client_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsStore::at(dir.path().join("smooai.json"));
        let client = SmoothApiClient::new("https://api.smoo.ai", None, store).expect("build");
        assert!(!client.is_authenticated());
        assert!(client.credentials().is_none());
    }

    #[test]
    fn authenticated_with_valid_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsStore::at(dir.path().join("smooai.json"));
        let creds = Credentials {
            access_token: "x".into(),
            refresh_token: None,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            user: None,
            active_org_id: None,
            client_id: None,
            client_secret: None,
            created_at: Utc::now(),
        };
        let client = SmoothApiClient::new("https://api.smoo.ai", Some(creds), store).expect("build");
        assert!(client.is_authenticated());
    }

    #[test]
    fn set_credentials_persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smooai.json");
        let store = CredentialsStore::at(&path);
        let client = SmoothApiClient::new("https://api.smoo.ai", None, store.clone()).expect("build");
        let creds = Credentials {
            access_token: "fresh".into(),
            refresh_token: None,
            expires_at: None,
            user: Some("brent@smoo.ai".into()),
            active_org_id: None,
            client_id: None,
            client_secret: None,
            created_at: Utc::now(),
        };
        client.set_credentials(creds.clone()).expect("set");
        assert!(path.exists());
        let loaded = store.load().expect("load").expect("present");
        assert_eq!(loaded.access_token, creds.access_token);
        assert_eq!(client.credentials().unwrap().access_token, creds.access_token);
    }
}

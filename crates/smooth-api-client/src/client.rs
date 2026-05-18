//! `SmoothApiClient` — thin auth wrapper over progenitor's generated
//! `Client` (in `crate::pb`). Reads credentials from disk, injects the
//! bearer token on every call, refreshes on 401 once.

use std::sync::{Arc, Mutex};

use anyhow::Result;
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
            created_at: Utc::now(),
        };
        client.set_credentials(creds.clone()).expect("set");
        assert!(path.exists());
        let loaded = store.load().expect("load").expect("present");
        assert_eq!(loaded.access_token, creds.access_token);
        assert_eq!(client.credentials().unwrap().access_token, creds.access_token);
    }
}

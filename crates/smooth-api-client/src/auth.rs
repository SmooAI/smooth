//! Smoo AI auth — OAuth 2.0 **client credentials** grant.
//!
//! 1. User creates an API key in the smooai web app → receives a
//!    client_id + client_secret pair.
//! 2. `th api login` POSTs those to `https://auth.smoo.ai/token` with
//!    `grant_type=client_credentials` (form-urlencoded, per RFC 6749).
//! 3. The token endpoint returns
//!    `{access_token, token_type: "Bearer", expires_in}`. We persist
//!    `access_token` + a computed `expires_at` to
//!    `~/.smooth/auth/smooai.json`.
//! 4. Subsequent `th api *` commands send the access_token as
//!    `Authorization: Bearer <token>` against `https://api.smoo.ai`.
//!
//! There's no refresh_token in this flow — client credentials are
//! re-exchanged when the access token expires. The CLI just runs the
//! grant again the next time it needs a fresh token.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::credentials::Credentials;

/// Default `tokenUrl` from the OpenAPI spec's `clientCredentials`
/// security scheme. Override with `SMOOAI_AUTH_URL` (handy for
/// staging / local dev).
pub const DEFAULT_AUTH_URL: &str = "https://auth.smoo.ai/token";

/// Resolve the token URL: env override first, then default.
#[must_use]
pub fn token_url() -> String {
    std::env::var("SMOOAI_AUTH_URL").unwrap_or_else(|_| DEFAULT_AUTH_URL.to_string())
}

/// Wire shape of the token endpoint response.
///
/// The published OpenAPI spec declares `access_token`, `token_type`,
/// and `expires_in` as required. The live server (observed
/// 2026-05-19 against `auth.smoo.ai`) actually returns
/// `{access_token, refresh_token}` with neither `token_type` nor
/// `expires_in`, so every field except `access_token` is optional
/// here. We default `expires_in` to 1 hour when missing — that's the
/// industry-standard short-lived JWT lifetime and erring on the
/// short side just means we re-exchange a few minutes early.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    /// Seconds until the token expires. Defaults to 3600 when the
    /// server omits the field.
    #[serde(default = "default_expires_in")]
    pub expires_in: u64,
    /// Refresh token. Not part of the OpenAPI spec but the live
    /// server returns one for org-scoped M2M grants.
    #[serde(default)]
    pub refresh_token: Option<String>,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

const fn default_expires_in() -> u64 {
    3600
}

/// What we send to `/token` — form-urlencoded per OAuth2 spec, plus
/// the smooai-specific `provider` parameter the server requires.
/// `provider` is undocumented in the published OpenAPI spec; smooai's
/// own internal call sites (see `infra/smoo-config.ts` in the smooai
/// monorepo) set it to `"client_credentials"` for this grant.
#[derive(Debug, Clone, Serialize)]
struct TokenRequest<'a> {
    grant_type: &'static str,
    provider: &'static str,
    client_id: &'a str,
    client_secret: &'a str,
}

/// Exchange a client_id + client_secret pair for an access token.
///
/// # Errors
/// Returns an error for network failures, non-2xx responses, or
/// missing/malformed `access_token` in the body. 4xx surfaces the
/// upstream error message verbatim so the user sees "invalid_client",
/// "invalid_grant", etc. as-is.
pub async fn client_credentials_grant(http: &reqwest::Client, client_id: &str, client_secret: &str) -> Result<Credentials> {
    let url = token_url();
    let req = TokenRequest {
        grant_type: "client_credentials",
        provider: "client_credentials",
        client_id,
        client_secret,
    };
    let resp = http.post(&url).form(&req).send().await.with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("token exchange returned HTTP {status}: {text}");
    }
    let body: TokenResponse = serde_json::from_str(&text).with_context(|| format!("parse token response: {text}"))?;
    let expires_at = Utc::now() + chrono::Duration::seconds(i64::try_from(body.expires_in).unwrap_or(3600));
    Ok(Credentials {
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        expires_at: Some(expires_at),
        // `client_credentials` doesn't identify a user — the API key
        // is its own identity. We store the client_id as a display
        // string so `th api whoami` shows *something* useful.
        user: Some(format!("client:{client_id}")),
        active_org_id: None,
        created_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_response_parses() {
        let raw = r#"{"access_token":"jwt","token_type":"Bearer","expires_in":3600}"#;
        let r: TokenResponse = serde_json::from_str(raw).expect("parse");
        assert_eq!(r.access_token, "jwt");
        assert_eq!(r.expires_in, 3600);
    }

    #[test]
    fn token_response_default_token_type() {
        let raw = r#"{"access_token":"jwt","expires_in":3600}"#;
        let r: TokenResponse = serde_json::from_str(raw).expect("parse");
        assert_eq!(r.token_type, "Bearer");
    }

    #[test]
    fn token_url_honors_env_override() {
        let prev = std::env::var("SMOOAI_AUTH_URL").ok();
        std::env::set_var("SMOOAI_AUTH_URL", "http://localhost:9999/token");
        assert_eq!(token_url(), "http://localhost:9999/token");
        match prev {
            Some(v) => std::env::set_var("SMOOAI_AUTH_URL", v),
            None => std::env::remove_var("SMOOAI_AUTH_URL"),
        }
    }
}

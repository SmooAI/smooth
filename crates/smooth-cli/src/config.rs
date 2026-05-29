//! `th config …` — top-level ergonomic wrappers over the
//! `@smooai/config` value endpoints on `api.smoo.ai`.
//!
//! Three subcommands:
//!
//! - `th config get <key> [--environment=<env>] [--org-id=<id>] [--json]`
//!   GET `/organizations/{org}/config/values/{key}?environment={env}`
//!   → `{ value }`. Prints the raw value (with --json wraps in
//!   `{"value": ...}`).
//!
//! - `th config set <key> <value> [--environment=<env>] [--org-id=<id>]
//!   [--tier=secret|public|featureFlag] [--schema-name=<name>]`
//!   Looks up the env-by-name to get its UUID + lists schemas to pick
//!   one (first by default, or `--schema-name`). PUTs to
//!   `/organizations/{org}/config/values` with
//!   `{schemaId, environmentId, key, value, tier}`. `value` is parsed
//!   as JSON when possible, otherwise stored as a string — matches the
//!   `smooai-config set` CLI behavior.
//!
//! - `th config list [--environment=<env>] [--org-id=<id>] [--json]`
//!   GET `/organizations/{org}/config/values?environment={env}`
//!   → `{ values: { key: value, ... } }`. Pretty-prints key/value pairs
//!   or emits JSON.
//!
//! ## Auth resolution
//!
//! By default reads the Supabase user JWT at
//! `~/.smooth/auth/smooai-user.json` (written by `th auth login`).
//! With `--m2m`, falls back to the M2M client_credentials session at
//! `~/.smooth/auth/smooai.json` (written by `th auth login --m2m` or
//! the legacy `th api login`). Either flow works; user JWT is the
//! default because it carries the dashboard's full scope (writes
//! included).
//!
//! Org id resolution (first present wins): `--org-id` flag, then
//! `SMOOAI_ORG_ID` env, then `active_org_id` field in the credentials
//! file. Same order as the rest of `th api *`.

use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::Value;
use smooai_client_shared::auth::storage::{Credentials, CredentialsStore};

/// Default tier for `th config set` when `--tier` isn't passed. Matches
/// the `smooai-config set` CLI's default.
const DEFAULT_TIER: &str = "public";

/// Default environment when `--environment` isn't passed. Matches the
/// `smooai-config` CLI's default — the dev laptop case is the common
/// case, so this saves a keystroke.
const DEFAULT_ENVIRONMENT: &str = "development";

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Get a single config value by key for an environment. Prints the
    /// raw value by default; `--json` wraps it in `{"value": ...}`.
    Get {
        /// The config key name (e.g. `databaseUrl`).
        key: String,
        /// Environment name. Defaults to `development`.
        #[arg(long, default_value = DEFAULT_ENVIRONMENT)]
        environment: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` env
        /// then the credentials file's `active_org_id`.
        #[arg(long)]
        org_id: Option<String>,
        /// Emit the response as JSON instead of the raw value.
        #[arg(long)]
        json: bool,
        /// Use the M2M session at `~/.smooth/auth/smooai.json`
        /// instead of the user JWT.
        #[arg(long)]
        m2m: bool,
    },
    /// Set (upsert) a single config value. Looks up the environment
    /// and schema by name, then PUTs to /config/values. `value` is
    /// parsed as JSON when possible (so `42`, `true`, `[1,2]` go in as
    /// numbers/bools/arrays), otherwise stored as a plain string.
    Set {
        /// The config key name.
        key: String,
        /// The new value. Parsed as JSON when valid; raw string otherwise.
        value: String,
        /// Environment name. Defaults to `development`.
        #[arg(long, default_value = DEFAULT_ENVIRONMENT)]
        environment: String,
        /// Override the active org.
        #[arg(long)]
        org_id: Option<String>,
        /// Tier: `public`, `secret`, or `feature_flag`. Defaults to `public`.
        #[arg(long, default_value = DEFAULT_TIER)]
        tier: String,
        /// Schema name to write under. Defaults to the first schema
        /// returned by the API (matches `smooai-config set` behavior).
        #[arg(long)]
        schema_name: Option<String>,
        /// Use the M2M session at `~/.smooth/auth/smooai.json`
        /// instead of the user JWT.
        #[arg(long)]
        m2m: bool,
    },
    /// List all config values for an environment as a key→value map.
    List {
        /// Environment name. Defaults to `development`.
        #[arg(long, default_value = DEFAULT_ENVIRONMENT)]
        environment: String,
        /// Override the active org.
        #[arg(long)]
        org_id: Option<String>,
        /// Emit the response as JSON instead of a key/value listing.
        #[arg(long)]
        json: bool,
        /// Use the M2M session at `~/.smooth/auth/smooai.json`
        /// instead of the user JWT.
        #[arg(long)]
        m2m: bool,
    },
}

/// Dispatch a `th config <sub>` invocation.
///
/// # Errors
/// - Missing / expired credentials
/// - Missing active org
/// - Non-2xx response from `api.smoo.ai`
/// - JSON parse failures on response bodies
pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Get {
            key,
            environment,
            org_id,
            json,
            m2m,
        } => cmd_get(key, environment, org_id, json, m2m).await,
        Cmd::Set {
            key,
            value,
            environment,
            org_id,
            tier,
            schema_name,
            m2m,
        } => cmd_set(key, value, environment, org_id, tier, schema_name, m2m).await,
        Cmd::List {
            environment,
            org_id,
            json,
            m2m,
        } => cmd_list(environment, org_id, json, m2m).await,
    }
}

async fn cmd_get(key: String, environment: String, org_id: Option<String>, json: bool, m2m: bool) -> Result<()> {
    let cfg = ConfigClient::load(m2m).await?;
    let org = cfg.resolve_org(org_id)?;
    let path = format!(
        "/organizations/{org}/config/values/{}?environment={}",
        urlencoding::encode(&key),
        urlencoding::encode(&environment)
    );
    let body = cfg.get(&path).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
    } else {
        // The SDK key-lookup endpoint returns `{"value": ...}`. Print
        // the inner value verbatim — strings unquoted, JSON pretty-printed.
        match body.get("value") {
            Some(Value::String(s)) => println!("{s}"),
            Some(v) => println!("{}", serde_json::to_string_pretty(v).unwrap_or_default()),
            None => println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default()),
        }
    }
    Ok(())
}

async fn cmd_list(environment: String, org_id: Option<String>, json: bool, m2m: bool) -> Result<()> {
    let cfg = ConfigClient::load(m2m).await?;
    let org = cfg.resolve_org(org_id)?;
    let path = format!("/organizations/{org}/config/values?environment={}", urlencoding::encode(&environment));
    let body = cfg.get(&path).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
        return Ok(());
    }
    let Some(values) = body.get("values").and_then(|v| v.as_object()) else {
        println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
        return Ok(());
    };
    println!();
    if values.is_empty() {
        println!("  {} {}", "●".dimmed(), format!("no values for environment {environment}").dimmed());
        println!();
        return Ok(());
    }
    let mut keys: Vec<&String> = values.keys().collect();
    keys.sort();
    let max_key_len = keys.iter().map(|k| k.len()).max().unwrap_or(0);
    for k in keys {
        let v = &values[k];
        let rendered = match v {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        println!("  {:<width$}  {}", k.cyan(), rendered.dimmed(), width = max_key_len);
    }
    println!();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cmd_set(key: String, value: String, environment: String, org_id: Option<String>, tier: String, schema_name: Option<String>, m2m: bool) -> Result<()> {
    let cfg = ConfigClient::load(m2m).await?;
    let org = cfg.resolve_org(org_id)?;

    // Resolve env-by-name → UUID. The platform's `/config/environments`
    // endpoint returns either `{data: [...]}` or a bare array; handle
    // both.
    let envs = cfg
        .get(&format!("/organizations/{org}/config/environments"))
        .await
        .context("list config environments")?;
    let env_arr = envs.get("data").and_then(|v| v.as_array()).or_else(|| envs.as_array());
    let env_id = env_arr
        .and_then(|arr| {
            arr.iter().find_map(|e| {
                let name = e.get("name").and_then(|v| v.as_str())?;
                if name == environment {
                    e.get("id").and_then(|v| v.as_str()).map(str::to_string)
                } else {
                    None
                }
            })
        })
        .with_context(|| format!("environment `{environment}` not found"))?;

    // Pick a schema. Same fallback shape as the env response.
    let schemas = cfg.get(&format!("/organizations/{org}/config/schemas")).await.context("list config schemas")?;
    let schema_arr = schemas.get("data").and_then(|v| v.as_array()).or_else(|| schemas.as_array());
    let schema_id = match (schema_arr, &schema_name) {
        (None, _) => anyhow::bail!("no schemas returned from /config/schemas"),
        (Some(arr), _) if arr.is_empty() => {
            anyhow::bail!("org has no config schemas — push one first via the smooai-config CLI or `th api config schemas create`");
        }
        (Some(arr), Some(name)) => arr
            .iter()
            .find_map(|s| {
                let n = s.get("name").and_then(|v| v.as_str())?;
                if n == *name {
                    s.get("id").and_then(|v| v.as_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .with_context(|| {
                let available: Vec<String> = arr.iter().filter_map(|s| s.get("name").and_then(|v| v.as_str()).map(str::to_string)).collect();
                format!("schema `{name}` not found. Available: {}", available.join(", "))
            })?,
        (Some(arr), None) => arr[0]
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .context("first schema has no id field")?,
    };

    // Parse value as JSON, fall back to string. Matches smooai-config.
    let parsed_value = serde_json::from_str::<Value>(&value).unwrap_or_else(|_| Value::String(value.clone()));

    let body = serde_json::json!({
        "schemaId": schema_id,
        "environmentId": env_id,
        "key": key,
        "value": parsed_value,
        "tier": tier,
    });
    let resp = cfg
        .put(&format!("/organizations/{org}/config/values"), &body)
        .await
        .context("PUT /config/values")?;

    println!();
    println!(
        "  {} Set {} = {} in {}",
        "✓".green().bold(),
        key.cyan().bold(),
        display_value(&parsed_value, &tier).dimmed(),
        environment.cyan()
    );
    if let Some(id) = resp.get("id").and_then(|v| v.as_str()) {
        println!("    {}  {}", "id".dimmed(), id.dimmed());
    }
    println!("    {}  {}", "tier".dimmed(), tier.dimmed());
    println!();
    Ok(())
}

/// Render a value for human-friendly display. Strings shown raw,
/// secrets masked, other JSON values compact-serialized.
fn display_value(v: &Value, tier: &str) -> String {
    let raw = match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    if tier == "secret" {
        mask_secret(&raw)
    } else {
        raw
    }
}

/// Inline mirror of `smooai-client-shared::auth::refresh::refresh_session`
/// (the upstream module exists at `client-shared/rust/src/auth/refresh.rs`
/// but isn't re-exported via `pub mod refresh` in that crate's
/// `mod.rs`, so we can't import it). Exchanges the stored Supabase
/// refresh_token for a fresh access_token + new refresh_token, keeping
/// the user/org display fields intact.
async fn refresh_user_session(http: &reqwest::Client, previous: &Credentials) -> Result<Credentials> {
    use chrono::Utc;
    use smooai_client_shared::auth::storage::CredentialKind;

    let refresh_token = previous
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("session has no refresh_token — re-run `th auth login`"))?;
    let supabase_url = std::env::var("SMOOAI_SUPABASE_URL").unwrap_or_else(|_| "https://db.smoo.ai".to_string());
    let anon_key = std::env::var("SMOOAI_SUPABASE_ANON_KEY").unwrap_or_else(|_| crate::auth::PROD_SUPABASE_ANON_KEY.to_string());

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

/// Mask all but the last 4 characters of a secret for log/display use.
fn mask_secret(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n <= 4 {
        "*".repeat(n)
    } else {
        let suffix: String = chars[n - 4..].iter().collect();
        format!("{}{suffix}", "*".repeat(n - 4))
    }
}

/// HTTP client + token wrapper. We don't reuse `SmoothApiClient` here
/// because that crate's `CredentialsStore` is hard-wired to the M2M
/// file path. The user-JWT support lives in the `smooai-client-shared`
/// crate, so we build a tiny HTTP shim around it.
struct ConfigClient {
    base_url: String,
    creds: Credentials,
    http: reqwest::Client,
}

impl ConfigClient {
    async fn load(m2m: bool) -> Result<Self> {
        let store = if m2m {
            CredentialsStore::default_m2m().context("locate ~/.smooth/auth/smooai.json")?
        } else {
            CredentialsStore::default_user().context("locate ~/.smooth/auth/smooai-user.json")?
        };
        let creds = match store.load().context("read credentials")? {
            Some(c) => c,
            None => {
                let hint = if m2m {
                    "not logged in — run `th auth login --m2m` (or `th api login`) first"
                } else {
                    "not logged in as a user — run `th auth login` first, or pass --m2m to use the M2M session"
                };
                anyhow::bail!(hint);
            }
        };

        let http = reqwest::Client::builder()
            .user_agent(format!("smooth-cli/{} (https://github.com/SmooAI/smooth)", env!("CARGO_PKG_VERSION")))
            .build()
            .context("build HTTP client")?;

        // Auto-refresh expired creds. Two paths:
        //
        // - M2M: re-mint via `client_credentials_grant` (same shape
        //   as `SmoothApiClient::ensure_fresh_token`).
        // - User: exchange the stored Supabase `refresh_token` for a
        //   fresh access_token at `{supabase}/auth/v1/token`. The
        //   refresh helper itself lives in `client-shared`'s
        //   `auth::refresh` module but isn't `pub`-exported, so we
        //   inline a minimal version here.
        let creds = if creds.is_expired() {
            if m2m {
                if let (Some(cid), Some(csecret)) = (creds.client_id.clone(), creds.client_secret.clone()) {
                    use smooai_client_shared::auth::m2m::client_credentials_grant;
                    let mut refreshed = client_credentials_grant(&http, &cid, &csecret)
                        .await
                        .context("auto-refresh M2M client_credentials grant")?;
                    refreshed.active_org_id = creds.active_org_id;
                    store.save(&refreshed).context("persist refreshed credentials")?;
                    refreshed
                } else {
                    anyhow::bail!("session expired — re-run `th auth login --m2m`");
                }
            } else if creds.refresh_token.is_some() {
                let refreshed = refresh_user_session(&http, &creds).await.context("auto-refresh user session")?;
                store.save(&refreshed).context("persist refreshed credentials")?;
                refreshed
            } else {
                anyhow::bail!("session expired — re-run `th auth login`");
            }
        } else {
            creds
        };

        Ok(Self {
            base_url: std::env::var("SMOOAI_API_URL").unwrap_or_else(|_| "https://api.smoo.ai".to_string()),
            creds,
            http,
        })
    }

    fn resolve_org(&self, override_org: Option<String>) -> Result<String> {
        if let Some(o) = override_org.filter(|s| !s.trim().is_empty()) {
            return Ok(o);
        }
        if let Ok(o) = std::env::var("SMOOAI_ORG_ID") {
            if !o.trim().is_empty() {
                return Ok(o);
            }
        }
        self.creds
            .active_org_id
            .clone()
            .context("no active org set — pass `--org-id <id>`, set SMOOAI_ORG_ID, or run `th api orgs switch <id>`")
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.send(reqwest::Method::GET, path, None).await
    }

    async fn put(&self, path: &str, body: &Value) -> Result<Value> {
        self.send(reqwest::Method::PUT, path, Some(body)).await
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            if path.starts_with('/') { path.to_string() } else { format!("/{path}") }
        );
        let mut req = self.http.request(method.clone(), &url).bearer_auth(&self.creds.access_token);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.with_context(|| format!("{method} {url}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("{method} {path} returned HTTP {status}: {text}");
        }
        if text.trim().is_empty() {
            return Ok(serde_json::json!({"ok": true}));
        }
        serde_json::from_str::<Value>(&text).with_context(|| format!("parse JSON response from {path}: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_secret_short() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("ab"), "**");
        assert_eq!(mask_secret("abcd"), "****");
    }

    #[test]
    fn mask_secret_long() {
        assert_eq!(mask_secret("abcdef"), "**cdef");
        assert_eq!(mask_secret("very-secret-value"), "*************alue");
    }

    #[test]
    fn display_value_string_public() {
        assert_eq!(display_value(&Value::String("hello".to_string()), "public"), "hello");
    }

    #[test]
    fn display_value_string_secret_is_masked() {
        let masked = display_value(&Value::String("abcdefgh".to_string()), "secret");
        assert_eq!(masked, "****efgh");
    }

    #[test]
    fn display_value_object_serializes_to_json() {
        let v = serde_json::json!({"a": 1});
        assert_eq!(display_value(&v, "public"), "{\"a\":1}");
    }

    #[test]
    fn display_value_number_serializes() {
        assert_eq!(display_value(&Value::from(42), "public"), "42");
        assert_eq!(display_value(&Value::Bool(true), "public"), "true");
    }

    fn fixture_creds(expired: bool) -> Credentials {
        use chrono::Utc;
        use smooai_client_shared::auth::storage::CredentialKind;
        Credentials {
            access_token: "tok".into(),
            refresh_token: None,
            expires_at: Some(if expired {
                Utc::now() - chrono::Duration::hours(1)
            } else {
                Utc::now() + chrono::Duration::hours(1)
            }),
            user: Some("brent@smoo.ai".into()),
            active_org_id: Some("org_abc".into()),
            client_id: None,
            client_secret: None,
            kind: CredentialKind::User,
            created_at: Utc::now(),
        }
    }

    fn make_client(creds: Credentials) -> ConfigClient {
        ConfigClient {
            base_url: "https://api.smoo.ai".into(),
            creds,
            http: reqwest::Client::new(),
        }
    }

    #[test]
    fn resolve_org_uses_override_flag_first() {
        let c = make_client(fixture_creds(false));
        let res = c.resolve_org(Some("org_override".into())).expect("ok");
        assert_eq!(res, "org_override");
    }

    #[test]
    fn resolve_org_falls_back_to_active_org() {
        let c = make_client(fixture_creds(false));
        // Make sure env var doesn't leak from another test.
        std::env::remove_var("SMOOAI_ORG_ID");
        let res = c.resolve_org(None).expect("ok");
        assert_eq!(res, "org_abc");
    }

    #[test]
    fn resolve_org_empty_override_is_ignored() {
        let c = make_client(fixture_creds(false));
        std::env::remove_var("SMOOAI_ORG_ID");
        let res = c.resolve_org(Some("   ".into())).expect("ok");
        assert_eq!(res, "org_abc");
    }

    #[test]
    fn resolve_org_errors_when_nothing_set() {
        let mut creds = fixture_creds(false);
        creds.active_org_id = None;
        let c = make_client(creds);
        std::env::remove_var("SMOOAI_ORG_ID");
        let err = c.resolve_org(None).unwrap_err();
        assert!(err.to_string().contains("no active org set"), "got: {err}");
    }
}

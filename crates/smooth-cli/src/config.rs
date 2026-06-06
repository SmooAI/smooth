//! `th config …` — top-level ergonomic wrappers over the
//! `@smooai/config` endpoints on `api.smoo.ai`.
//!
//! Subcommands:
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
//! - `th config push [--org-id=<id>] [--schema-name=<name>]
//!   [--description=<msg>] [--dry-run]`
//!   Reads `.smooai-config/schema.json` from the cwd, compares it to
//!   the remote schema for `<org_id>` (matched by `--schema-name` or
//!   `$smooaiName` in the file, falling back to the first remote
//!   schema), prints the per-tier diff, and POSTs a new version to
//!   `/organizations/{org}/config/schemas/{schemaId}/push`. With
//!   `--dry-run`, prints the diff and stops. If no matching remote
//!   schema exists, creates one via POST `/config/schemas`.
//!
//! - `th config pull [--org-id=<id>] [--schema-name=<name>] [--force]`
//!   Fetches the remote schema and writes `.smooai-config/schema.json`
//!   to the cwd. If the file already exists, refuses with a clear
//!   error unless `--force` is passed. Pull is intended for the
//!   bootstrap / sync case — once the consumer adds custom layout to
//!   `config.ts`, the source-of-truth flips and pushes drive the wire
//!   format.
//!
//! - `th config diff [--org-id=<id>] [--schema-name=<name>] [--json]`
//!   Same comparison as the dry-run side of `push`, but read-only.
//!   Prints added / removed / tier-changed keys. With `--json`, emits
//!   structured JSON.
//!
//! - `th config init [--directory=<path>] [--force]`
//!   Scaffolds a fresh `.smooai-config/` at `<path>` (default: cwd)
//!   with a TypeScript `config.ts`, `default.ts`, and `package.json`.
//!   Refuses to overwrite an existing directory unless `--force`.
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
    /// Push the local `.smooai-config/schema.json` to the org's remote
    /// schema. Prints a per-tier diff first; with `--dry-run`, stops
    /// after printing. Creates a new remote schema if none matches.
    Push {
        /// Override the active org.
        #[arg(long)]
        org_id: Option<String>,
        /// Schema name to push under. Defaults to `$smooaiName` from
        /// schema.json, falling back to the first remote schema.
        #[arg(long)]
        schema_name: Option<String>,
        /// Optional change description recorded with the new version.
        #[arg(long)]
        description: Option<String>,
        /// Compute + print the diff, but do not POST the new version.
        #[arg(long)]
        dry_run: bool,
        /// Use the M2M session at `~/.smooth/auth/smooai.json`
        /// instead of the user JWT.
        #[arg(long)]
        m2m: bool,
    },
    /// Fetch the remote schema for an org and write it to
    /// `.smooai-config/schema.json` in the cwd. Refuses to clobber an
    /// existing file unless `--force`.
    Pull {
        /// Override the active org.
        #[arg(long)]
        org_id: Option<String>,
        /// Schema name to pull. Defaults to the first remote schema.
        #[arg(long)]
        schema_name: Option<String>,
        /// Overwrite an existing `.smooai-config/schema.json`.
        #[arg(long)]
        force: bool,
        /// Use the M2M session at `~/.smooth/auth/smooai.json`
        /// instead of the user JWT.
        #[arg(long)]
        m2m: bool,
    },
    /// Compare local `.smooai-config/schema.json` to the remote schema
    /// for the org. Prints added / removed / tier-changed keys.
    Diff {
        /// Override the active org.
        #[arg(long)]
        org_id: Option<String>,
        /// Schema name to compare against. Defaults to the first
        /// remote schema.
        #[arg(long)]
        schema_name: Option<String>,
        /// Emit the diff as structured JSON instead of pretty-print.
        #[arg(long)]
        json: bool,
        /// Use the M2M session at `~/.smooth/auth/smooai.json`
        /// instead of the user JWT.
        #[arg(long)]
        m2m: bool,
    },
    /// Scaffold a fresh `.smooai-config/` directory with TypeScript
    /// `config.ts`, `default.ts`, and `package.json` templates.
    Init {
        /// Target directory to scaffold into. Defaults to the cwd.
        #[arg(long)]
        directory: Option<String>,
        /// Overwrite an existing `.smooai-config/` directory.
        #[arg(long)]
        force: bool,
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
        Cmd::Push {
            org_id,
            schema_name,
            description,
            dry_run,
            m2m,
        } => cmd_push(org_id, schema_name, description, dry_run, m2m).await,
        Cmd::Pull {
            org_id,
            schema_name,
            force,
            m2m,
        } => cmd_pull(org_id, schema_name, force, m2m).await,
        Cmd::Diff {
            org_id,
            schema_name,
            json,
            m2m,
        } => cmd_diff(org_id, schema_name, json, m2m).await,
        Cmd::Init { directory, force } => cmd_init(directory, force),
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
            let refreshed = if m2m {
                crate::auth::refresh::refresh_m2m_session(&http, &creds)
                    .await
                    .context("auto-refresh M2M client_credentials grant")?
            } else if creds.refresh_token.is_some() {
                crate::auth::refresh::refresh_user_session(&http, &creds)
                    .await
                    .context("auto-refresh user session")?
            } else {
                anyhow::bail!("session expired — re-run `th auth login`");
            };
            store.save(&refreshed).context("persist refreshed credentials")?;
            refreshed
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

// ---------------------------------------------------------------------------
// Lane D: push / pull / diff / init (SMOODEV-1410)
//
// The schema files (`config.ts`, `default.ts`, `package.json`, `.gitignore`)
// emitted by `th config init` are baked into the binary via `include_str!`
// from `src/config_templates/`. They mirror the TypeScript-language path
// of the upstream `smooai-config init` command (config repo). We don't
// regenerate `.smooai-config/schema.json` on `init` — that gets emitted by
// the package's build step (Lane B). The local schema.json is the source
// of truth for push/diff/pull.
// ---------------------------------------------------------------------------

const TEMPLATE_CONFIG_TS: &str = include_str!("config_templates/config.ts");
const TEMPLATE_DEFAULT_TS: &str = include_str!("config_templates/default.ts");
const TEMPLATE_PACKAGE_JSON: &str = include_str!("config_templates/package.json");
const TEMPLATE_GITIGNORE: &str = include_str!("config_templates/gitignore");

/// Per-tier schema diff. Sorted within each list for stable output.
#[derive(Debug, Default, serde::Serialize, PartialEq, Eq)]
struct SchemaDiff {
    added: Vec<TieredKey>,
    removed: Vec<TieredKey>,
    tier_changed: Vec<TierChange>,
}

#[derive(Debug, serde::Serialize, PartialEq, Eq, Clone)]
struct TieredKey {
    key: String,
    tier: String,
}

#[derive(Debug, serde::Serialize, PartialEq, Eq, Clone)]
struct TierChange {
    key: String,
    from: String,
    to: String,
}

impl SchemaDiff {
    fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.tier_changed.is_empty()
    }
}

/// Flatten a schema-manifest JSON document (the shape emitted by Lane B
/// into `.smooai-config/schema.json`) into a `key → tier` map. Tiers
/// are the three top-level array properties: `public`, `secret`,
/// `featureFlag`. Unknown shapes return an empty map — caller decides
/// how to surface that.
fn flatten_schema(schema: &Value) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for tier in ["public", "secret", "featureFlag"] {
        if let Some(arr) = schema.get(tier).and_then(|v| v.as_array()) {
            for k in arr {
                if let Some(s) = k.as_str() {
                    out.insert(s.to_string(), tier.to_string());
                }
            }
        }
    }
    out
}

/// Compare local vs remote flattened maps. A key in both maps but with
/// different tiers is reported as a tier change, not an add+remove.
fn compute_diff(local: &Value, remote: &Value) -> SchemaDiff {
    let local_map = flatten_schema(local);
    let remote_map = flatten_schema(remote);

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut tier_changed = Vec::new();

    for (k, tier) in &local_map {
        match remote_map.get(k) {
            None => added.push(TieredKey {
                key: k.clone(),
                tier: tier.clone(),
            }),
            Some(rt) if rt != tier => tier_changed.push(TierChange {
                key: k.clone(),
                from: rt.clone(),
                to: tier.clone(),
            }),
            Some(_) => {}
        }
    }
    for (k, tier) in &remote_map {
        if !local_map.contains_key(k) {
            removed.push(TieredKey {
                key: k.clone(),
                tier: tier.clone(),
            });
        }
    }

    added.sort_by(|a, b| a.key.cmp(&b.key));
    removed.sort_by(|a, b| a.key.cmp(&b.key));
    tier_changed.sort_by(|a, b| a.key.cmp(&b.key));

    SchemaDiff { added, removed, tier_changed }
}

/// Load `.smooai-config/schema.json` from the cwd. Errors loud on
/// missing file (push/diff/pull can't proceed without it).
fn load_local_schema_json() -> Result<(std::path::PathBuf, Value)> {
    let cwd = std::env::current_dir().context("get current dir")?;
    let path = cwd.join(".smooai-config").join("schema.json");
    if !path.exists() {
        anyhow::bail!(
            "no `.smooai-config/schema.json` in {}.\n\
             Build the schema first (the @smooai/config build step writes it from config.ts), \
             or scaffold a new package with `th config init`.",
            cwd.display()
        );
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let parsed = serde_json::from_str::<Value>(&raw).with_context(|| format!("parse JSON from {}", path.display()))?;
    Ok((path, parsed))
}

/// Resolve which remote schema to act on. Order:
/// 1. `--schema-name` flag (errors if no match)
/// 2. `$smooaiName` inside the local schema.json (errors if no match)
/// 3. First remote schema in the list (or `None` if there are none)
fn pick_remote_schema<'a>(remote_schemas: &'a [Value], flag: Option<&str>, local_schema: Option<&Value>) -> Result<Option<&'a Value>> {
    let by_name = |name: &str| -> Option<&Value> { remote_schemas.iter().find(|s| s.get("name").and_then(|v| v.as_str()) == Some(name)) };
    if let Some(name) = flag {
        return Ok(Some(by_name(name).with_context(|| {
            let available: Vec<String> = remote_schemas
                .iter()
                .filter_map(|s| s.get("name").and_then(|v| v.as_str()).map(str::to_string))
                .collect();
            format!("schema `{name}` not found. Available: {}", available.join(", "))
        })?));
    }
    if let Some(local) = local_schema {
        if let Some(name) = local.get("$smooaiName").and_then(|v| v.as_str()) {
            // If the local declares a name and the remote has it, use it.
            // If the local declares a name but the remote DOESN'T, that's
            // the push-creates-new-schema path — return None so the
            // caller knows to create.
            return Ok(by_name(name));
        }
    }
    Ok(remote_schemas.first())
}

/// List remote schemas for an org, normalising both `[...]` and
/// `{data: [...]}` envelopes (matches `cmd_set` defensive parsing).
async fn list_schemas(client: &ConfigClient, org: &str) -> Result<Vec<Value>> {
    let body = client
        .get(&format!("/organizations/{org}/config/schemas"))
        .await
        .context("list config schemas")?;
    let arr = body
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .or_else(|| body.as_array().cloned())
        .unwrap_or_default();
    Ok(arr)
}

async fn cmd_push(org_id: Option<String>, schema_name: Option<String>, description: Option<String>, dry_run: bool, m2m: bool) -> Result<()> {
    let (local_path, local_schema) = load_local_schema_json()?;

    let cfg = ConfigClient::load(m2m).await?;
    let org = cfg.resolve_org(org_id)?;
    let remote_schemas = list_schemas(&cfg, &org).await?;

    let picked = pick_remote_schema(&remote_schemas, schema_name.as_deref(), Some(&local_schema))?;

    // Resolve the schema name we'd push under. Priority:
    // 1. --schema-name flag
    // 2. picked remote's name (when matched)
    // 3. local $smooaiName
    let resolved_name = schema_name
        .clone()
        .or_else(|| picked.and_then(|s| s.get("name").and_then(|v| v.as_str()).map(str::to_string)))
        .or_else(|| local_schema.get("$smooaiName").and_then(|v| v.as_str()).map(str::to_string));

    let empty_remote = serde_json::json!({});
    let remote_for_diff = picked.and_then(|s| s.get("jsonSchema")).unwrap_or(&empty_remote);
    let diff = compute_diff(&local_schema, remote_for_diff);

    print_diff_pretty(&diff, picked.is_none(), resolved_name.as_deref());

    if dry_run {
        println!("  {} dry-run — no changes pushed", "●".dimmed());
        println!();
        return Ok(());
    }

    if diff.is_empty() && picked.is_some() {
        println!("  {} already in sync", "✓".green().bold());
        println!();
        return Ok(());
    }

    match picked {
        Some(remote) => {
            let schema_id = remote.get("id").and_then(|v| v.as_str()).context("remote schema entry has no id")?;
            let body = serde_json::json!({
                "jsonSchema": local_schema,
                "changeDescription": description,
            });
            cfg.post(&format!("/organizations/{org}/config/schemas/{schema_id}/push"), &body)
                .await
                .context("POST /config/schemas/{id}/push")?;
            println!(
                "  {} pushed new version of {} from {}",
                "✓".green().bold(),
                resolved_name.as_deref().unwrap_or("(unnamed)").cyan().bold(),
                local_path.display().to_string().dimmed()
            );
            println!();
        }
        None => {
            let name = resolved_name
                .context("no remote schema matched and no name to create one under. Pass `--schema-name <name>` or add `$smooaiName` to schema.json.")?;
            let body = serde_json::json!({
                "name": name,
                "jsonSchema": local_schema,
                "description": description,
            });
            let resp = cfg
                .post(&format!("/organizations/{org}/config/schemas"), &body)
                .await
                .context("POST /config/schemas")?;
            let id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            println!("  {} created new schema {} ({})", "✓".green().bold(), name.cyan().bold(), id.dimmed());
            println!();
        }
    }
    Ok(())
}

async fn cmd_pull(org_id: Option<String>, schema_name: Option<String>, force: bool, m2m: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("get current dir")?;
    let dir = cwd.join(".smooai-config");
    let path = dir.join("schema.json");
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists. Pass --force to overwrite (this only replaces the wire JSON; \
             your `config.ts` / `default.ts` are not touched).",
            path.display()
        );
    }

    let cfg = ConfigClient::load(m2m).await?;
    let org = cfg.resolve_org(org_id)?;
    let remote_schemas = list_schemas(&cfg, &org).await?;
    let picked = pick_remote_schema(&remote_schemas, schema_name.as_deref(), None)?.context("no remote schemas found for this org")?;

    let json_schema = picked.get("jsonSchema").cloned().context("remote schema entry has no jsonSchema field")?;

    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let pretty = serde_json::to_string_pretty(&json_schema).context("serialize jsonSchema")?;
    std::fs::write(&path, format!("{pretty}\n")).with_context(|| format!("write {}", path.display()))?;

    let name = picked.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)");
    println!();
    println!(
        "  {} wrote {} ({} keys)",
        "✓".green().bold(),
        path.display().to_string().cyan(),
        flatten_schema(&json_schema).len()
    );
    println!("    {}  {}", "schema".dimmed(), name.dimmed());
    println!();
    Ok(())
}

async fn cmd_diff(org_id: Option<String>, schema_name: Option<String>, json: bool, m2m: bool) -> Result<()> {
    let (_local_path, local_schema) = load_local_schema_json()?;

    let cfg = ConfigClient::load(m2m).await?;
    let org = cfg.resolve_org(org_id)?;
    let remote_schemas = list_schemas(&cfg, &org).await?;
    let picked = pick_remote_schema(&remote_schemas, schema_name.as_deref(), Some(&local_schema))?;

    let empty_remote = serde_json::json!({});
    let remote_for_diff = picked.and_then(|s| s.get("jsonSchema")).unwrap_or(&empty_remote);
    let diff = compute_diff(&local_schema, remote_for_diff);

    if json {
        let payload = serde_json::json!({
            "hasRemote": picked.is_some(),
            "remoteSchemaName": picked.and_then(|s| s.get("name").and_then(|v| v.as_str())),
            "diff": diff,
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        return Ok(());
    }

    let name = picked.and_then(|s| s.get("name").and_then(|v| v.as_str()));
    print_diff_pretty(&diff, picked.is_none(), name);
    if diff.is_empty() && picked.is_some() {
        println!("  {} in sync", "✓".green().bold());
        println!();
    }
    Ok(())
}

/// Pretty-print a schema diff. When `is_new` is true (no remote
/// matched), all keys are reported as "would create" rather than
/// "added", which is the more accurate framing.
fn print_diff_pretty(diff: &SchemaDiff, is_new: bool, schema_name: Option<&str>) {
    println!();
    match schema_name {
        Some(n) => println!("  {} {}", "Schema:".dimmed(), n.cyan()),
        None => println!("  {} {}", "Schema:".dimmed(), "(none — would create)".yellow()),
    }
    if diff.is_empty() {
        return;
    }

    if !diff.added.is_empty() {
        let label = if is_new { "would create" } else { "added" };
        println!("  {} {}", "+".green().bold(), format!("{} ({}):", label, diff.added.len()).green());
        for k in &diff.added {
            println!("      {} {} {}", "+".green(), k.key.cyan(), format!("[{}]", k.tier).dimmed());
        }
    }
    if !diff.removed.is_empty() {
        println!("  {} {}", "-".red().bold(), format!("removed ({}):", diff.removed.len()).red());
        for k in &diff.removed {
            println!("      {} {} {}", "-".red(), k.key.cyan(), format!("[{}]", k.tier).dimmed());
        }
    }
    if !diff.tier_changed.is_empty() {
        println!("  {} {}", "~".yellow().bold(), format!("tier changed ({}):", diff.tier_changed.len()).yellow());
        for c in &diff.tier_changed {
            println!("      {} {}: {} → {}", "~".yellow(), c.key.cyan(), c.from.dimmed(), c.to.cyan());
        }
    }
    println!();
}

fn cmd_init(directory: Option<String>, force: bool) -> Result<()> {
    let base = match directory {
        Some(d) => std::path::PathBuf::from(d),
        None => std::env::current_dir().context("get current dir")?,
    };
    let dir = base.join(".smooai-config");
    if dir.exists() && !force {
        anyhow::bail!("{} already exists. Pass --force to overwrite, or pick a fresh --directory.", dir.display());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

    let files = [
        ("config.ts", TEMPLATE_CONFIG_TS),
        ("default.ts", TEMPLATE_DEFAULT_TS),
        ("package.json", TEMPLATE_PACKAGE_JSON),
        (".gitignore", TEMPLATE_GITIGNORE),
    ];
    let mut written = Vec::new();
    for (name, body) in files {
        let p = dir.join(name);
        if p.exists() && !force {
            anyhow::bail!("{} already exists. Pass --force to overwrite.", p.display());
        }
        std::fs::write(&p, body).with_context(|| format!("write {}", p.display()))?;
        written.push(p);
    }

    println!();
    println!("  {} scaffolded {}", "✓".green().bold(), dir.display().to_string().cyan());
    for p in &written {
        if let Some(name) = p.file_name() {
            println!("    {} {}", "+".green(), name.to_string_lossy().dimmed());
        }
    }
    println!();
    println!("  {} {}", "next:".dimmed(), "edit config.ts to add keys, then `th config push`".dimmed());
    println!();
    Ok(())
}

// `ConfigClient::post` is added in Lane D — Lane C only needed GET/PUT.
impl ConfigClient {
    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.send(reqwest::Method::POST, path, Some(body)).await
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

    // ----- Lane D tests ----------------------------------------------------

    #[test]
    fn flatten_schema_handles_tiered_arrays() {
        let s = serde_json::json!({
            "$schema": "x",
            "public": ["A", "B"],
            "secret": ["C"],
            "featureFlag": ["F"],
        });
        let m = flatten_schema(&s);
        assert_eq!(m.get("A").map(String::as_str), Some("public"));
        assert_eq!(m.get("B").map(String::as_str), Some("public"));
        assert_eq!(m.get("C").map(String::as_str), Some("secret"));
        assert_eq!(m.get("F").map(String::as_str), Some("featureFlag"));
        assert_eq!(m.len(), 4);
    }

    #[test]
    fn flatten_schema_ignores_unknown_shape() {
        assert!(flatten_schema(&serde_json::json!({})).is_empty());
        assert!(flatten_schema(&serde_json::json!({"public": "not-an-array"})).is_empty());
    }

    #[test]
    fn compute_diff_reports_added_removed_changed() {
        let local = serde_json::json!({
            "public": ["A", "B"],
            "secret": ["C", "D"],
            "featureFlag": [],
        });
        let remote = serde_json::json!({
            "public": ["A"],
            "secret": ["D"],
            "featureFlag": ["C"], // C moved secret → featureFlag in local
        });
        let d = compute_diff(&local, &remote);
        // B is genuinely new
        assert_eq!(d.added.len(), 1, "added: {:?}", d.added);
        assert_eq!(d.added[0].key, "B");
        assert_eq!(d.added[0].tier, "public");
        // Nothing removed
        assert!(d.removed.is_empty(), "removed: {:?}", d.removed);
        // C changed tiers (featureFlag in remote → secret in local)
        assert_eq!(d.tier_changed.len(), 1, "changed: {:?}", d.tier_changed);
        assert_eq!(d.tier_changed[0].key, "C");
        assert_eq!(d.tier_changed[0].from, "featureFlag");
        assert_eq!(d.tier_changed[0].to, "secret");
    }

    #[test]
    fn compute_diff_empty_for_identical_schemas() {
        let s = serde_json::json!({"public": ["A"], "secret": [], "featureFlag": []});
        let d = compute_diff(&s, &s);
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn compute_diff_against_empty_remote_lists_all_as_added() {
        let local = serde_json::json!({"public": ["X", "Y"], "secret": ["Z"]});
        let remote = serde_json::json!({});
        let d = compute_diff(&local, &remote);
        assert_eq!(d.added.len(), 3);
        // sorted alphabetically
        assert_eq!(d.added[0].key, "X");
        assert_eq!(d.added[1].key, "Y");
        assert_eq!(d.added[2].key, "Z");
    }

    #[test]
    fn pick_remote_schema_flag_wins() {
        let schemas = vec![serde_json::json!({"id": "1", "name": "alpha"}), serde_json::json!({"id": "2", "name": "beta"})];
        let picked = pick_remote_schema(&schemas, Some("beta"), None).expect("ok").expect("some");
        assert_eq!(picked.get("id").and_then(|v| v.as_str()), Some("2"));
    }

    #[test]
    fn pick_remote_schema_flag_missing_errors() {
        let schemas = vec![serde_json::json!({"id": "1", "name": "alpha"})];
        let err = pick_remote_schema(&schemas, Some("nope"), None).unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    #[test]
    fn pick_remote_schema_uses_local_name_when_present() {
        let schemas = vec![serde_json::json!({"id": "1", "name": "alpha"}), serde_json::json!({"id": "2", "name": "beta"})];
        let local = serde_json::json!({"$smooaiName": "beta"});
        let picked = pick_remote_schema(&schemas, None, Some(&local)).expect("ok").expect("some");
        assert_eq!(picked.get("id").and_then(|v| v.as_str()), Some("2"));
    }

    #[test]
    fn pick_remote_schema_local_name_no_match_returns_none() {
        // Push-creates-new path: local says "gamma" but remote doesn't have it
        let schemas = vec![serde_json::json!({"id": "1", "name": "alpha"})];
        let local = serde_json::json!({"$smooaiName": "gamma"});
        let picked = pick_remote_schema(&schemas, None, Some(&local)).expect("ok");
        assert!(picked.is_none(), "expected None, got {picked:?}");
    }

    #[test]
    fn pick_remote_schema_falls_back_to_first() {
        let schemas = vec![serde_json::json!({"id": "1", "name": "alpha"}), serde_json::json!({"id": "2", "name": "beta"})];
        let picked = pick_remote_schema(&schemas, None, None).expect("ok").expect("some");
        assert_eq!(picked.get("id").and_then(|v| v.as_str()), Some("1"));
    }

    #[test]
    fn init_scaffolds_into_fresh_directory() {
        let tmp = tempfile::tempdir().expect("tmp");
        let target = tmp.path().to_string_lossy().to_string();
        cmd_init(Some(target.clone()), false).expect("init ok");

        let dir = tmp.path().join(".smooai-config");
        for name in ["config.ts", "default.ts", "package.json", ".gitignore"] {
            let p = dir.join(name);
            assert!(p.exists(), "missing {}", p.display());
        }

        let config_ts = std::fs::read_to_string(dir.join("config.ts")).unwrap();
        assert!(config_ts.contains("defineConfig"), "config.ts content unexpected");
        assert!(config_ts.contains("publicConfigSchema"));
        assert!(config_ts.contains("secretConfigSchema"));
        assert!(config_ts.contains("featureFlagSchema"));
    }

    #[test]
    fn init_refuses_existing_dir_without_force() {
        let tmp = tempfile::tempdir().expect("tmp");
        let target = tmp.path().to_string_lossy().to_string();
        // First init succeeds
        cmd_init(Some(target.clone()), false).expect("first init ok");
        // Second init without --force errors
        let err = cmd_init(Some(target.clone()), false).unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {err}");
        // With --force, succeeds
        cmd_init(Some(target), true).expect("force-init ok");
    }
}

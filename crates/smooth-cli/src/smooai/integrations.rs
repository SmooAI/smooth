//! `smoo integrations …` — the org's third-party integrations
//! (SMOODEV-3531). Also reachable as `smoo api integrations …`.
//!
//! - `list` / `status [<provider>]` read `/organizations/{org}/integrations/summary`
//!   (and `/mine` for the caller's user-scoped accounts).
//! - `connect <provider>` is a **browser handoff**: for OAuth providers it asks
//!   `…/integrations/<p>/oauth/authorize` for the consent URL (user bearer only —
//!   an M2M token gets 401), opens it, then polls the summary until the provider
//!   goes active. Providers whose connect needs a dialog (API keys, BYO Twilio,
//!   Stripe Connect, …) open the dashboard at
//!   `/apps/integrations?focus=<p>&connect=<p>` instead.
//! - `connect twilio --provision` creates a Smoo-managed Twilio subaccount
//!   (billed usage) behind a confirmation.
//! - `disconnect <provider>` deletes the connection behind a confirmation.
//! - `sendgrid …` is the older SendGrid-only CRUD, kept as is.
//!
//! Secrets are never accepted on argv: OAuth consent happens in the browser,
//! API-key providers are entered in the dashboard dialog, and the SendGrid
//! key comes from `SENDGRID_API_KEY` or a masked prompt.

use std::collections::HashSet;
use std::future::Future;
use std::io::IsTerminal;
use std::time::{Duration, Instant};

use anstream::{eprintln, println};
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use dialoguer::{theme::ColorfulTheme, Password};
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// List every integration provider and whether the org has it connected.
    /// `--mine` lists the accounts YOU own (user session required).
    #[command(visible_alias = "ls")]
    List {
        /// Only the caller's user-scoped accounts (`/integrations/mine`).
        #[arg(long)]
        mine: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw API response.
        #[arg(long)]
        json: bool,
    },
    /// Connection status — every provider, or one provider with its accounts.
    Status {
        /// Provider type (e.g. `google`, `hubspot`). Omit for all.
        provider: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw API response (one provider's entry when a provider is given).
        #[arg(long)]
        json: bool,
    },
    /// The providers this CLI can connect, and how (`oauth` = consent URL
    /// opened by the CLI, `web` = the dashboard's connect dialog).
    Providers {
        /// Print as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Connect a provider through your browser, then wait until it's active.
    ///
    /// OAuth providers open the provider's consent screen (requires a USER
    /// session — `smoo auth login`). Providers whose connect takes API keys or
    /// a multi-step dialog open the dashboard's connect dialog instead.
    /// Secrets are never taken on the command line.
    Connect(ConnectArgs),
    /// Disconnect a provider (deletes the stored connection). Confirms first.
    Disconnect {
        /// Provider type (e.g. `slack`).
        provider: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the target and exit without disconnecting.
        #[arg(long)]
        dry_run: bool,
        /// Skip the interactive confirmation. Required in scripts/CI.
        #[arg(long)]
        yes: bool,
    },
    /// SendGrid email integration (get / create / delete / test).
    Sendgrid {
        #[command(subcommand)]
        cmd: SendgridCmd,
    },
}

/// Permission tier requested from an OAuth provider. The server defaults to
/// `read_only` when omitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Tier {
    #[value(name = "read_only", alias = "read-only")]
    ReadOnly,
    #[value(name = "full_access", alias = "full-access")]
    FullAccess,
}

impl Tier {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::FullAccess => "full_access",
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct ConnectArgs {
    /// Provider type (see `smoo integrations providers`).
    pub provider: String,
    /// Which consent to request. google (required): features | analytics | ads |
    /// gmail_restricted | drive_restricted | directory. microsoft: features |
    /// directory. meta: publish | ads | leads | whatsapp.
    #[arg(long)]
    pub purpose: Option<String>,
    /// Permission tier for OAuth providers (server default: read_only).
    #[arg(long, value_enum)]
    pub tier: Option<Tier>,
    /// bluesky: your handle (e.g. `acme.bsky.social`) so the right PDS is used.
    #[arg(long)]
    pub handle: Option<String>,
    /// shopify (required): the store, `acme` or `acme.myshopify.com`.
    #[arg(long)]
    pub shop: Option<String>,
    /// salesforce: connect a sandbox org (test.salesforce.com).
    #[arg(long)]
    pub sandbox: bool,
    /// jira: Jira cloudId or site URL (e.g. `https://acme.atlassian.net`).
    #[arg(long)]
    pub site: Option<String>,
    /// zendesk (required): your Zendesk subdomain (`acme` for acme.zendesk.com).
    #[arg(long)]
    pub subdomain: Option<String>,
    /// twilio: provision a Smoo-managed Twilio subaccount instead of linking
    /// your own (usage is billed through Smoo AI). Confirms first.
    #[arg(long)]
    pub provision: bool,
    /// Skip the `--provision` confirmation. Required in scripts/CI.
    #[arg(long)]
    pub yes: bool,
    /// Don't try to open a browser — just print the URL.
    #[arg(long)]
    pub no_browser: bool,
    /// Don't wait for the connection to go active.
    #[arg(long)]
    pub no_wait: bool,
    /// Seconds to wait for the connection to go active.
    #[arg(long, default_value_t = 300)]
    pub timeout: u64,
    /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
    #[arg(long = "org-id", visible_alias = "org")]
    pub org: Option<String>,
}

#[derive(Subcommand)]
pub enum SendgridCmd {
    /// Show the org's SendGrid integration (API key redacted).
    Get {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create the SendGrid integration. The API key is read from
    /// `SENDGRID_API_KEY` or prompted for — never passed on argv.
    Create {
        /// Verified sender address SendGrid sends from.
        #[arg(long = "from-email")]
        from_email: String,
        /// Address inbound-parse routes replies to.
        #[arg(long = "inbound-email")]
        inbound_email: String,
        /// Optional friendly From name.
        #[arg(long = "from-name")]
        from_name: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete the org's SendGrid integration. Prints the target (org +
    /// host) and confirms before acting; refuses when not attached to a
    /// terminal.
    Delete {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the target and exit without deleting.
        #[arg(long)]
        dry_run: bool,
        /// Skip the interactive confirmation. Required in scripts/CI.
        #[arg(long)]
        yes: bool,
    },
    /// Send a test email to verify the configuration.
    Test {
        /// Recipient address for the test email.
        #[arg(long = "to")]
        to: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Provider catalog
// ---------------------------------------------------------------------------

/// How `connect` reaches a provider's consent / credential step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectKind {
    /// `GET …/integrations/<p>/oauth/authorize` → `{url}`; the CLI opens it.
    OAuth,
    /// The dashboard's connect dialog (API keys, BYO Twilio, Stripe Connect,
    /// WhatsApp embedded signup, …).
    Web,
}

impl ConnectKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::OAuth => "oauth",
            Self::Web => "web",
        }
    }
}

pub struct Provider {
    /// The `type` string the API and the web app use.
    pub name: &'static str,
    pub label: &'static str,
    pub kind: ConnectKind,
    /// A `DELETE /organizations/:org/integrations/<p>` route exists.
    pub disconnectable: bool,
    /// Reported by `/integrations/summary` (so `connect` can wait on it).
    pub in_summary: bool,
}

const fn oauth(name: &'static str, label: &'static str) -> Provider {
    Provider {
        name,
        label,
        kind: ConnectKind::OAuth,
        disconnectable: true,
        in_summary: true,
    }
}

const fn web(name: &'static str, label: &'static str) -> Provider {
    Provider {
        name,
        label,
        kind: ConnectKind::Web,
        disconnectable: true,
        in_summary: true,
    }
}

/// Mirrors `INTEGRATION_DEFINITIONS` (apps/web/components/services/integration-service.ts)
/// cross-checked against the backend: every `OAuth` entry has a
/// `…/oauth/authorize` handler in packages/backend/src/routes/integrations/.
/// Stripe is `oauth` in the web catalog but connects through a
/// `POST …/stripe/connect` Express account link, so it is a web handoff here.
pub const PROVIDERS: &[Provider] = &[
    oauth("asana", "Asana"),
    web("automation", "Automation (webhooks)"),
    oauth("bluesky", "Bluesky"),
    oauth("calendly", "Calendly"),
    oauth("confluence", "Confluence"),
    oauth("discord", "Discord"),
    oauth("github", "GitHub"),
    oauth("gohighlevel", "GoHighLevel"),
    oauth("google", "Google"),
    oauth("hubspot", "HubSpot"),
    // iCloud calendars (Apple ID + app-specific password) are added from the
    // booking page's conflict calendars, not the integrations page, and the
    // summary does not report them.
    Provider {
        name: "icloud",
        label: "iCloud Calendar",
        kind: ConnectKind::Web,
        disconnectable: false,
        in_summary: false,
    },
    oauth("jira", "Jira"),
    oauth("linkedin", "LinkedIn"),
    oauth("mailchimp", "Mailchimp"),
    oauth("meta", "Meta (Facebook / Instagram)"),
    oauth("microsoft", "Microsoft"),
    oauth("monday", "monday.com"),
    oauth("notion", "Notion"),
    web("odoo", "Odoo"),
    oauth("quickbooks", "QuickBooks"),
    oauth("reddit", "Reddit"),
    // The backend has authorize + delete handlers, but api-prime has no
    // manifest route for salesforce yet — `connect` falls back to the
    // dashboard on a 404, and there is no disconnect route to call.
    Provider {
        name: "salesforce",
        label: "Salesforce",
        kind: ConnectKind::OAuth,
        disconnectable: false,
        in_summary: true,
    },
    web("sendgrid", "SendGrid"),
    oauth("shopify", "Shopify"),
    oauth("slack", "Slack"),
    web("stripe", "Stripe"),
    oauth("threads", "Threads"),
    oauth("tiktok", "TikTok"),
    web("twilio", "Twilio"),
    web("whatsapp", "WhatsApp"),
    web("woocommerce", "WooCommerce"),
    oauth("youtube", "YouTube"),
    oauth("zendesk", "Zendesk"),
    oauth("zoom", "Zoom"),
];

/// Look a provider up by its type string (case-insensitive, `-`/`_` tolerant).
///
/// # Errors
/// Unknown provider — the message lists the valid names.
pub fn find_provider(name: &str) -> Result<&'static Provider> {
    let wanted = name.trim().to_ascii_lowercase().replace(['-', '_', ' '], "");
    PROVIDERS.iter().find(|p| p.name.replace('_', "") == wanted).ok_or_else(|| {
        let names: Vec<&str> = PROVIDERS.iter().map(|p| p.name).collect();
        anyhow::anyhow!("unknown integration provider `{name}` — expected one of: {}", names.join(", "))
    })
}

const GOOGLE_PURPOSES: &[&str] = &["features", "analytics", "ads", "gmail_restricted", "drive_restricted", "directory"];
const MICROSOFT_PURPOSES: &[&str] = &["features", "directory"];
const META_PURPOSES: &[&str] = &["publish", "ads", "leads", "whatsapp"];

fn check_purpose(provider: &str, purpose: &str, allowed: &[&str]) -> Result<String> {
    let p = purpose.trim().to_ascii_lowercase().replace('-', "_");
    if allowed.contains(&p.as_str()) {
        Ok(p)
    } else {
        bail!("--purpose `{purpose}` is not valid for {provider} — expected one of: {}", allowed.join(", "))
    }
}

/// Normalise `--shop` to a `*.myshopify.com` domain (the authorize route
/// rejects anything else).
fn shop_domain(shop: &str) -> Result<String> {
    let s = shop
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    if s.is_empty() {
        bail!("--shop must not be empty");
    }
    let domain = if s.contains('.') { s } else { format!("{s}.myshopify.com") };
    if !domain.ends_with(".myshopify.com") {
        bail!("--shop must be a *.myshopify.com store (got `{shop}`)");
    }
    Ok(domain)
}

/// Validate the provider-specific flags and build the `oauth/authorize`
/// query, in a stable order. Pure, so every provider rule is unit tested.
///
/// # Errors
/// A flag that doesn't apply to `provider`, a missing required flag, or an
/// invalid value.
pub fn authorize_query(provider: &Provider, a: &ConnectArgs) -> Result<Vec<(&'static str, String)>> {
    let name = provider.name;
    let reject = |set: bool, flag: &str, only: &str| -> Result<()> {
        if set {
            bail!("{flag} only applies to {only}, not {name}");
        }
        Ok(())
    };
    reject(
        a.purpose.is_some() && !matches!(name, "google" | "microsoft" | "meta"),
        "--purpose",
        "google, microsoft and meta",
    )?;
    reject(a.handle.is_some() && name != "bluesky", "--handle", "bluesky")?;
    reject(a.shop.is_some() && name != "shopify", "--shop", "shopify")?;
    reject(a.sandbox && name != "salesforce", "--sandbox", "salesforce")?;
    reject(a.site.is_some() && name != "jira", "--site", "jira")?;
    reject(a.subdomain.is_some() && name != "zendesk", "--subdomain", "zendesk")?;
    reject(a.provision && name != "twilio", "--provision", "twilio")?;

    let mut q: Vec<(&'static str, String)> = Vec::new();
    let tier = a.tier.map(Tier::as_str);
    match name {
        "google" => {
            // Google's consents are distinct permission TIERS on the authorize
            // route (google-client.ts GOOGLE_PERMISSION_TIERS); `purpose` there
            // only means a booking-blocking calendar.
            let Some(purpose) = a.purpose.as_deref() else {
                bail!("google needs --purpose: one of {}", GOOGLE_PURPOSES.join(", "));
            };
            match check_purpose(name, purpose, GOOGLE_PURPOSES)?.as_str() {
                "features" => q.push(("tier", tier.unwrap_or("read_only").to_string())),
                "directory" => {
                    reject(tier.is_some(), "--tier", "google --purpose features")?;
                    q.push(("tier", "admin".to_string()));
                }
                other => {
                    reject(tier.is_some(), "--tier", "google --purpose features")?;
                    q.push(("tier", other.to_string()));
                }
            }
        }
        "microsoft" => match a.purpose.as_deref().map(|p| check_purpose(name, p, MICROSOFT_PURPOSES)).transpose()?.as_deref() {
            Some("directory") => {
                reject(tier.is_some(), "--tier", "microsoft --purpose features")?;
                q.push(("tier", "admin".to_string()));
            }
            _ => q.push(("tier", tier.unwrap_or("read_only").to_string())),
        },
        _ => {
            if provider.kind == ConnectKind::OAuth {
                q.push(("tier", tier.unwrap_or("read_only").to_string()));
            } else if tier.is_some() {
                bail!("--tier only applies to OAuth providers; {name} is connected in the dashboard");
            }
        }
    }
    match name {
        "meta" => {
            if let Some(p) = a.purpose.as_deref() {
                q.push(("purpose", check_purpose(name, p, META_PURPOSES)?));
            }
        }
        "bluesky" => {
            if let Some(h) = a.handle.as_deref().map(str::trim).filter(|h| !h.is_empty()) {
                q.push(("handle", h.trim_start_matches('@').to_string()));
            }
        }
        "shopify" => {
            let Some(shop) = a.shop.as_deref() else {
                bail!("shopify needs --shop <store> (e.g. --shop acme or --shop acme.myshopify.com)");
            };
            q.push(("shop", shop_domain(shop)?));
        }
        "salesforce" if a.sandbox => q.push(("sandbox", "true".to_string())),
        "jira" => {
            if let Some(site) = a.site.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                q.push(("site", site.to_string()));
            }
        }
        "zendesk" => {
            let Some(sub) = a.subdomain.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
                bail!("zendesk needs --subdomain <name> (`acme` for acme.zendesk.com)");
            };
            if !sub.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') || sub.starts_with('-') {
                bail!("--subdomain must be letters, digits and dashes (got `{sub}`)");
            }
            q.push(("subdomain", sub.to_string()));
        }
        "hubspot" => {
            // Same defaults the dashboard sends (integration-service.ts getOAuthUrl,
            // SMOODEV-2877): without them HubSpot connects at reduced fidelity.
            q.push(("schemas", "true".to_string()));
            q.push(("owners", "true".to_string()));
            if tier == Some("full_access") {
                q.push(("dealSchemaWrite", "true".to_string()));
            }
        }
        _ => {}
    }
    Ok(q)
}

/// `/organizations/{org}/integrations/{p}/oauth/authorize?…`
pub fn authorize_path(org: &str, provider: &str, query: &[(&str, String)]) -> String {
    let mut path = format!("/organizations/{}/integrations/{provider}/oauth/authorize", urlencoding::encode(org));
    for (i, (k, v)) in query.iter().enumerate() {
        path.push(if i == 0 { '?' } else { '&' });
        path.push_str(k);
        path.push('=');
        path.push_str(&urlencoding::encode(v));
    }
    path
}

/// The dashboard page that connects `provider`. `?focus=` scrolls to the
/// card; `?connect=` opens its dialog (SMOODEV-3533 — ignored until it ships).
pub fn handoff_url(web_base: &str, provider: &str) -> String {
    let base = web_base.trim_end_matches('/');
    if provider == "icloud" {
        return format!("{base}/apps/booking");
    }
    format!("{base}/apps/integrations?focus={provider}&connect={provider}")
}

// ---------------------------------------------------------------------------
// Summary parsing
// ---------------------------------------------------------------------------

/// The `integrations[]` entry for `provider` in a summary response.
pub fn summary_entry<'a>(summary: &'a Value, provider: &str) -> Option<&'a Value> {
    summary
        .get("integrations")?
        .as_array()?
        .iter()
        .find(|e| e.get("type").and_then(Value::as_str) == Some(provider))
}

/// Which summary `purpose` a connect is waiting on. Only google and microsoft
/// tag accounts with a purpose; meta's purposes share one connection.
fn summary_purpose(provider: &str, purpose: Option<&str>) -> Option<String> {
    match provider {
        "google" | "microsoft" => Some(purpose.map_or_else(|| "features".to_string(), |p| p.to_ascii_lowercase().replace('-', "_"))),
        _ => None,
    }
}

/// Ids of the ACTIVE accounts for `provider` (narrowed to `purpose` when
/// given). `None` when the summary doesn't report the provider at all
/// (unknown, or hidden by a feature gate).
pub fn active_account_ids(summary: &Value, provider: &str, purpose: Option<&str>) -> Option<Vec<String>> {
    let entry = summary_entry(summary, provider)?;
    let accounts = entry.get("accounts").and_then(Value::as_array).cloned().unwrap_or_default();
    Some(
        accounts
            .iter()
            .filter(|a| a.get("status").and_then(Value::as_str) == Some("active"))
            .filter(|a| purpose.is_none_or(|p| a.get("purpose").and_then(Value::as_str).is_none_or(|ap| ap == p)))
            .filter_map(|a| a.get("id").and_then(Value::as_str).map(str::to_string))
            .collect(),
    )
}

#[derive(Debug, PartialEq, Eq)]
pub enum PollOutcome {
    /// A new active account appeared; its ids.
    Connected(Vec<String>),
    /// Deadline passed with no new active account.
    TimedOut,
}

/// Poll `fetch` (the summary) every `interval` until an active account for
/// `provider`/`purpose` appears that wasn't in `baseline`, or `timeout`
/// passes. A failed poll is tolerated (the next one retries) — a transient
/// 5xx mid-consent shouldn't abort a connect the user is completing.
pub async fn poll_until_connected<F, Fut>(
    mut fetch: F,
    provider: &str,
    purpose: Option<&str>,
    baseline: &HashSet<String>,
    interval: Duration,
    timeout: Duration,
) -> PollOutcome
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(summary) = fetch().await {
            if let Some(ids) = active_account_ids(&summary, provider, purpose) {
                let new: Vec<String> = ids.into_iter().filter(|id| !baseline.contains(id)).collect();
                if !new.is_empty() {
                    return PollOutcome::Connected(new);
                }
            }
        }
        if Instant::now() + interval > deadline {
            return PollOutcome::TimedOut;
        }
        tokio::time::sleep(interval).await;
    }
}

fn account_line(a: &Value) -> String {
    let who = a
        .get("userEmail")
        .and_then(Value::as_str)
        .or_else(|| a.get("accountLabel").and_then(Value::as_str))
        .unwrap_or("(account)");
    let mut extra = Vec::new();
    for key in ["purpose", "permissionTier"] {
        if let Some(v) = a.get(key).and_then(Value::as_str) {
            extra.push(v.to_string());
        }
    }
    if a.get("isDefault").and_then(Value::as_bool) == Some(true) {
        extra.push("default".to_string());
    }
    let status = a.get("status").and_then(Value::as_str).unwrap_or("?");
    format!(
        "{who} [{status}]{}",
        if extra.is_empty() {
            String::new()
        } else {
            format!(" ({})", extra.join(", "))
        }
    )
}

fn label_for(provider: &str) -> &str {
    PROVIDERS.iter().find(|p| p.name == provider).map_or(provider, |p| p.label)
}

fn print_summary(summary: &Value, only: Option<&str>) {
    let mut entries: Vec<&Value> = summary
        .get("integrations")
        .and_then(Value::as_array)
        .map(|v| v.iter().collect())
        .unwrap_or_default();
    if let Some(p) = only {
        entries.retain(|e| e.get("type").and_then(Value::as_str) == Some(p));
    }
    entries.sort_by_key(|e| e.get("type").and_then(Value::as_str).unwrap_or("").to_string());
    println!();
    for e in entries {
        let ty = e.get("type").and_then(Value::as_str).unwrap_or("?");
        let connected = e.get("connected").and_then(Value::as_bool).unwrap_or(false);
        let accounts = e.get("accounts").and_then(Value::as_array).cloned().unwrap_or_default();
        if connected {
            println!("  {} {:<14} {}", "●".green(), ty.bold(), "connected".green());
        } else if accounts.is_empty() {
            println!("  {} {:<14} {}", "○".dimmed(), ty, "not connected".dimmed());
        } else {
            println!("  {} {:<14} {}", "●".yellow(), ty.bold(), "inactive".yellow());
        }
        if only.is_some() || connected || !accounts.is_empty() {
            for a in &accounts {
                println!("      {}", account_line(a).dimmed());
            }
        }
    }
    println!();
}

/// Turn an authorize-call failure into the message a person can act on.
fn authorize_error(provider: &str, err: &anyhow::Error) -> anyhow::Error {
    let msg = format!("{err:#}");
    if msg.contains("HTTP 401") {
        anyhow::anyhow!(
            "{provider} consent needs a signed-in Smoo USER — an org API key (M2M) can't start an OAuth connect. \
             Run `smoo auth login`, then retry."
        )
    } else if msg.contains("HTTP 403") {
        anyhow::anyhow!("not allowed to connect {provider}: {msg}")
    } else {
        anyhow::anyhow!("could not start the {provider} connect: {msg}")
    }
}

fn open_in_browser(url: &str, what: &str, no_browser: bool) {
    if no_browser {
        println!("Open this URL in a browser to connect {what}:");
        println!("  {url}");
        return;
    }
    println!("Opening browser to connect {what}...");
    println!("  {url}");
    if let Err(e) = open::that(url) {
        eprintln!("  (couldn't auto-open the browser: {e}. Copy the URL above into a browser to continue.)");
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::List { mine, org, json } => {
            if mine {
                let client = super::require_user_session()
                    .await
                    .context("`--mine` needs a user session — run `smoo auth login`")?;
                let o = require_active_org(&client, org)?;
                let body = client
                    .get(&format!("/organizations/{o}/integrations/mine"))
                    .await
                    .context("GET integrations/mine")?;
                if json {
                    print_json(&body);
                } else {
                    let accounts = body.get("accounts").and_then(Value::as_array).cloned().unwrap_or_default();
                    println!();
                    if accounts.is_empty() {
                        println!("  {} {}", "●".dimmed(), "no user-scoped integration accounts".dimmed());
                    }
                    for a in &accounts {
                        let p = a.get("provider").and_then(Value::as_str).unwrap_or("?");
                        println!("  {} {:<14} {}", "●".green(), p.bold(), account_line(a));
                    }
                    println!();
                }
            } else {
                let client = require_authed().await?;
                let o = require_active_org(&client, org)?;
                let body = client
                    .get(&format!("/organizations/{o}/integrations/summary"))
                    .await
                    .context("GET integrations summary")?;
                if json {
                    print_json(&body);
                } else {
                    print_summary(&body, None);
                }
            }
        }
        Cmd::Status { provider, org, json } => {
            let p = provider.as_deref().map(find_provider).transpose()?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!("/organizations/{o}/integrations/summary"))
                .await
                .context("GET integrations summary")?;
            match p {
                None if json => print_json(&body),
                None => print_summary(&body, None),
                Some(p) => match summary_entry(&body, p.name) {
                    Some(entry) if json => print_json(entry),
                    Some(_) => print_summary(&body, Some(p.name)),
                    None => {
                        if json {
                            print_json(&Value::Null);
                        } else if p.in_summary {
                            println!(
                                "\n  {} {} is not visible for this org — it may need a feature the org's plan doesn't include.\n",
                                "○".dimmed(),
                                p.label
                            );
                        } else {
                            println!(
                                "\n  {} {} isn't reported by the integrations summary — check {}\n",
                                "○".dimmed(),
                                p.label,
                                handoff_url(&super::web_url(), p.name)
                            );
                        }
                    }
                },
            }
        }
        Cmd::Providers { json } => {
            if json {
                let list: Vec<Value> = PROVIDERS
                    .iter()
                    .map(|p| json!({ "type": p.name, "name": p.label, "connect": p.kind.as_str(), "disconnect": p.disconnectable }))
                    .collect();
                print_json(&Value::Array(list));
            } else {
                println!();
                for p in PROVIDERS {
                    println!("  {:<14} {:<6} {}", p.name.bold(), p.kind.as_str().dimmed(), p.label);
                }
                println!();
            }
        }
        Cmd::Connect(args) => connect(args).await?,
        Cmd::Disconnect { provider, org, dry_run, yes } => {
            let p = find_provider(&provider)?;
            if !p.disconnectable {
                bail!(
                    "{} can't be disconnected from the CLI — manage it at {}",
                    p.label,
                    handoff_url(&super::web_url(), p.name)
                );
            }
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let proceed = crate::destructive::gate(
                &crate::destructive::Target {
                    verb: "disconnect",
                    noun: "integration",
                    id: p.name,
                    org: &o,
                    severity: crate::destructive::Severity::Standard,
                },
                dry_run,
                yes,
            )?;
            if proceed {
                client
                    .delete(&format!("/organizations/{o}/integrations/{}", p.name))
                    .await
                    .with_context(|| format!("DELETE {} integration", p.name))?;
                println!("\n  {} {} disconnected\n", "✓".green(), p.label);
            }
        }
        Cmd::Sendgrid { cmd } => sendgrid(cmd).await?,
    }
    Ok(())
}

async fn connect(args: ConnectArgs) -> Result<()> {
    let p = find_provider(&args.provider)?;
    // Validate every flag before touching auth or the network.
    let query = authorize_query(p, &args)?;

    if args.provision {
        return provision_twilio(args).await;
    }

    let poll_purpose = summary_purpose(p.name, args.purpose.as_deref());
    let (client, url) = match p.kind {
        ConnectKind::OAuth => {
            let client = super::require_user_session()
                .await
                .with_context(|| format!("connecting {} needs a signed-in Smoo user — run `smoo auth login`", p.label))?;
            let o = require_active_org(&client, args.org.clone())?;
            match client.get(&authorize_path(&o, p.name, &query)).await {
                Ok(body) => {
                    let url = body
                        .get("url")
                        .and_then(Value::as_str)
                        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
                        .with_context(|| format!("{} authorize response had no usable `url`: {body}", p.name))?
                        .to_string();
                    (client, url)
                }
                Err(e) if format!("{e:#}").contains("HTTP 404") => {
                    let url = handoff_url(&super::web_url(), p.name);
                    eprintln!("  ({} has no API connect route yet — finishing in the dashboard instead.)", p.label);
                    (client, url)
                }
                Err(e) => return Err(authorize_error(p.name, &e)),
            }
        }
        ConnectKind::Web => {
            let client = require_authed().await?;
            if p.name == "twilio" {
                println!("Tip: `smoo integrations connect twilio --provision` creates a Smoo-managed Twilio account with no Twilio console.");
            }
            (client, handoff_url(&super::web_url(), p.name))
        }
    };
    let o = require_active_org(&client, args.org.clone())?;
    let summary_path = format!("/organizations/{o}/integrations/summary");

    // Baseline: accounts that were already active, so a reconnect / extra
    // account is detected as a NEW active id rather than an instant "done".
    let baseline: HashSet<String> = if p.in_summary && !args.no_wait {
        client
            .get(&summary_path)
            .await
            .ok()
            .and_then(|s| active_account_ids(&s, p.name, poll_purpose.as_deref()))
            .unwrap_or_default()
            .into_iter()
            .collect()
    } else {
        HashSet::new()
    };

    open_in_browser(&url, p.label, args.no_browser);

    if args.no_wait || !p.in_summary {
        return Ok(());
    }
    println!("Waiting for {} to connect (up to {}s; Ctrl-C to stop waiting)...", p.label, args.timeout);
    let outcome = poll_until_connected(
        || client.get(&summary_path),
        p.name,
        poll_purpose.as_deref(),
        &baseline,
        Duration::from_secs(3),
        Duration::from_secs(args.timeout),
    )
    .await;
    match outcome {
        PollOutcome::Connected(ids) => {
            println!(
                "\n  {} {} connected ({} account{})\n",
                "✓".green(),
                p.label,
                ids.len(),
                if ids.len() == 1 { "" } else { "s" }
            );
            Ok(())
        }
        PollOutcome::TimedOut if !baseline.is_empty() => {
            // A reconnect of a single-account provider updates the row in place,
            // so no new id ever appears — not proof the consent failed.
            println!(
                "\n  {} {} was already connected and no new account appeared. If you re-consented, check `smoo integrations status {}`.\n",
                "●".yellow(),
                p.label,
                p.name
            );
            Ok(())
        }
        PollOutcome::TimedOut => bail!(
            "{} did not connect within {}s — finish in the browser, then check `smoo integrations status {}`",
            p.label,
            args.timeout,
            p.name
        ),
    }
}

async fn provision_twilio(args: ConnectArgs) -> Result<()> {
    let client = require_authed().await?;
    let o = require_active_org(&client, args.org)?;
    println!("This creates a Smoo-managed Twilio subaccount for the org. Calls, texts and numbers are billed through Smoo AI.");
    let proceed = crate::destructive::gate(
        &crate::destructive::Target {
            verb: "provision",
            noun: "Smoo-managed Twilio account",
            id: "twilio",
            org: &o,
            severity: crate::destructive::Severity::Standard,
        },
        false,
        args.yes,
    )?;
    if !proceed {
        return Ok(());
    }
    let body = client
        .post(&format!("/organizations/{o}/integrations/twilio/provision"), None)
        .await
        .context("POST twilio provision")?;
    print_json(&body);
    Ok(())
}

async fn sendgrid(cmd: SendgridCmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        SendgridCmd::Get { org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/integrations/sendgrid"))
                    .await
                    .context("GET sendgrid integration")?,
            );
        }
        SendgridCmd::Create {
            from_email,
            inbound_email,
            from_name,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let api_key = resolve_api_key(std::env::var("SENDGRID_API_KEY").ok())?;
            let body = build_create_body(&from_email, &inbound_email, from_name.as_deref(), &api_key);
            print_json(
                &client
                    .post(&format!("/organizations/{o}/integrations/sendgrid"), Some(&body))
                    .await
                    .context("POST sendgrid integration")?,
            );
        }
        SendgridCmd::Delete { org, dry_run, yes } => {
            let o = require_active_org(&client, org)?;
            let proceed = crate::destructive::gate(
                &crate::destructive::Target {
                    verb: "delete",
                    noun: "SendGrid integration",
                    id: "sendgrid",
                    org: &o,
                    severity: crate::destructive::Severity::Standard,
                },
                dry_run,
                yes,
            )?;
            if proceed {
                print_json(
                    &client
                        .delete(&format!("/organizations/{o}/integrations/sendgrid"))
                        .await
                        .context("DELETE sendgrid integration")?,
                );
            }
        }
        SendgridCmd::Test { to, org } => {
            let o = require_active_org(&client, org)?;
            let body = build_test_body(&to);
            print_json(
                &client
                    .post(&format!("/organizations/{o}/integrations/sendgrid/test"), Some(&body))
                    .await
                    .context("POST sendgrid test")?,
            );
        }
    }
    Ok(())
}

/// Resolve the SendGrid API key without ever reading it from argv:
/// use `SENDGRID_API_KEY` when non-empty, otherwise prompt (masked).
/// Errors when the env var is absent/blank and there's no TTY.
fn resolve_api_key(env_value: Option<String>) -> Result<String> {
    if let Some(k) = env_value {
        let k = k.trim();
        if !k.is_empty() {
            return Ok(k.to_string());
        }
    }
    if !std::io::stdin().is_terminal() {
        bail!("SENDGRID_API_KEY is not set and stdin is not a TTY to prompt — export SENDGRID_API_KEY and retry");
    }
    let key = Password::with_theme(&ColorfulTheme::default())
        .with_prompt("SendGrid API key")
        .interact()
        .context("read SendGrid API key")?;
    if key.trim().is_empty() {
        bail!("SendGrid API key must not be empty");
    }
    Ok(key.trim().to_string())
}

fn build_create_body(from_email: &str, inbound_email: &str, from_name: Option<&str>, api_key: &str) -> serde_json::Value {
    let mut body = json!({
        "apiKey": api_key,
        "fromEmail": from_email,
        "inboundEmail": inbound_email,
    });
    if let Some(name) = from_name {
        body["fromName"] = json!(name);
    }
    body
}

fn build_test_body(to: &str) -> serde_json::Value {
    json!({ "to": to })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use clap::Parser;

    use super::*;

    /// Parse `connect …` argv through clap, exactly as the CLI would.
    fn connect_args(argv: &[&str]) -> ConnectArgs {
        #[derive(Parser)]
        struct T {
            #[command(subcommand)]
            cmd: Cmd,
        }
        let mut full = vec!["t", "connect"];
        full.extend_from_slice(argv);
        match T::try_parse_from(full).expect("parse").cmd {
            Cmd::Connect(a) => a,
            _ => unreachable!(),
        }
    }

    fn query_for(argv: &[&str]) -> Result<Vec<(&'static str, String)>> {
        let a = connect_args(argv);
        authorize_query(find_provider(&a.provider)?, &a)
    }

    fn q(pairs: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
        pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect()
    }

    // ── catalog ────────────────────────────────────────────────────────

    #[test]
    fn find_provider_is_case_and_dash_tolerant() {
        assert_eq!(find_provider("HubSpot").unwrap().name, "hubspot");
        assert_eq!(find_provider(" go-high-level ").unwrap().name, "gohighlevel");
        let err = find_provider("myspace").unwrap_err().to_string();
        assert!(err.contains("unknown integration provider") && err.contains("hubspot"), "{err}");
    }

    #[test]
    fn catalog_names_are_unique_and_kinds_match_the_backend() {
        let mut seen = HashSet::new();
        for p in PROVIDERS {
            assert!(seen.insert(p.name), "duplicate provider {}", p.name);
        }
        // API-key / dialog providers never get an authorize call.
        for name in ["sendgrid", "odoo", "woocommerce", "twilio", "stripe", "icloud", "automation", "whatsapp"] {
            assert_eq!(find_provider(name).unwrap().kind, ConnectKind::Web, "{name}");
        }
        for name in ["google", "hubspot", "slack", "shopify", "bluesky", "salesforce", "zendesk"] {
            assert_eq!(find_provider(name).unwrap().kind, ConnectKind::OAuth, "{name}");
        }
    }

    // ── authorize query / provider arg validation ──────────────────────

    #[test]
    fn plain_oauth_provider_defaults_to_read_only() {
        assert_eq!(query_for(&["slack"]).unwrap(), q(&[("tier", "read_only")]));
        assert_eq!(query_for(&["slack", "--tier", "full_access"]).unwrap(), q(&[("tier", "full_access")]));
        assert_eq!(query_for(&["slack", "--tier", "full-access"]).unwrap(), q(&[("tier", "full_access")]));
    }

    #[test]
    fn google_requires_purpose_and_maps_it_to_a_tier() {
        let err = query_for(&["google"]).unwrap_err().to_string();
        assert!(err.contains("--purpose"), "{err}");
        assert_eq!(query_for(&["google", "--purpose", "features"]).unwrap(), q(&[("tier", "read_only")]));
        assert_eq!(
            query_for(&["google", "--purpose", "features", "--tier", "full_access"]).unwrap(),
            q(&[("tier", "full_access")])
        );
        assert_eq!(query_for(&["google", "--purpose", "analytics"]).unwrap(), q(&[("tier", "analytics")]));
        assert_eq!(query_for(&["google", "--purpose", "ads"]).unwrap(), q(&[("tier", "ads")]));
        assert_eq!(
            query_for(&["google", "--purpose", "gmail-restricted"]).unwrap(),
            q(&[("tier", "gmail_restricted")])
        );
        assert_eq!(
            query_for(&["google", "--purpose", "drive_restricted"]).unwrap(),
            q(&[("tier", "drive_restricted")])
        );
        assert_eq!(query_for(&["google", "--purpose", "directory"]).unwrap(), q(&[("tier", "admin")]));
        assert!(query_for(&["google", "--purpose", "calendar"]).is_err());
        // A narrow consent has a fixed scope set; a tier would be ignored, so refuse it.
        assert!(query_for(&["google", "--purpose", "analytics", "--tier", "full_access"]).is_err());
    }

    #[test]
    fn microsoft_and_meta_purposes() {
        assert_eq!(query_for(&["microsoft"]).unwrap(), q(&[("tier", "read_only")]));
        assert_eq!(query_for(&["microsoft", "--purpose", "directory"]).unwrap(), q(&[("tier", "admin")]));
        assert!(query_for(&["microsoft", "--purpose", "ads"]).is_err());
        assert_eq!(
            query_for(&["meta", "--purpose", "leads"]).unwrap(),
            q(&[("tier", "read_only"), ("purpose", "leads")])
        );
        assert!(query_for(&["meta", "--purpose", "directory"]).is_err());
    }

    #[test]
    fn provider_specific_flags() {
        assert_eq!(
            query_for(&["bluesky", "--handle", "@acme.bsky.social"]).unwrap(),
            q(&[("tier", "read_only"), ("handle", "acme.bsky.social")])
        );
        assert_eq!(query_for(&["bluesky"]).unwrap(), q(&[("tier", "read_only")]));
        assert_eq!(
            query_for(&["shopify", "--shop", "acme"]).unwrap(),
            q(&[("tier", "read_only"), ("shop", "acme.myshopify.com")])
        );
        assert_eq!(
            query_for(&["shopify", "--shop", "https://Acme.myshopify.com/"]).unwrap(),
            q(&[("tier", "read_only"), ("shop", "acme.myshopify.com")])
        );
        assert!(query_for(&["shopify"]).is_err());
        assert!(query_for(&["shopify", "--shop", "acme.com"]).is_err());
        assert_eq!(
            query_for(&["salesforce", "--sandbox"]).unwrap(),
            q(&[("tier", "read_only"), ("sandbox", "true")])
        );
        assert_eq!(query_for(&["salesforce"]).unwrap(), q(&[("tier", "read_only")]));
        assert_eq!(
            query_for(&["jira", "--site", "https://acme.atlassian.net"]).unwrap(),
            q(&[("tier", "read_only"), ("site", "https://acme.atlassian.net")])
        );
        assert_eq!(
            query_for(&["zendesk", "--subdomain", "acme"]).unwrap(),
            q(&[("tier", "read_only"), ("subdomain", "acme")])
        );
        assert!(query_for(&["zendesk"]).is_err());
        assert!(query_for(&["zendesk", "--subdomain", "acme.zendesk.com"]).is_err());
    }

    #[test]
    fn hubspot_sends_the_dashboard_scope_defaults() {
        assert_eq!(
            query_for(&["hubspot"]).unwrap(),
            q(&[("tier", "read_only"), ("schemas", "true"), ("owners", "true")])
        );
        assert_eq!(
            query_for(&["hubspot", "--tier", "full_access"]).unwrap(),
            q(&[("tier", "full_access"), ("schemas", "true"), ("owners", "true"), ("dealSchemaWrite", "true")])
        );
    }

    #[test]
    fn flags_for_the_wrong_provider_are_rejected() {
        for argv in [
            &["slack", "--shop", "acme"][..],
            &["slack", "--purpose", "ads"],
            &["google", "--purpose", "features", "--handle", "x"],
            &["hubspot", "--sandbox"],
            &["slack", "--provision"],
            &["slack", "--subdomain", "acme"],
            &["slack", "--site", "x"],
            &["sendgrid", "--tier", "full_access"],
        ] {
            let err = query_for(argv).unwrap_err().to_string();
            assert!(err.contains("only applies to"), "{argv:?}: {err}");
        }
    }

    #[test]
    fn web_providers_need_no_query_and_twilio_accepts_provision() {
        assert!(query_for(&["sendgrid"]).unwrap().is_empty());
        assert!(query_for(&["twilio", "--provision", "--yes"]).unwrap().is_empty());
    }

    #[test]
    fn secrets_have_no_flag() {
        // There is deliberately no --api-key / --token / --password / --auth-token.
        for flag in ["--api-key", "--token", "--password", "--auth-token", "--secret"] {
            #[derive(Parser)]
            struct T {
                #[command(subcommand)]
                cmd: Cmd,
            }
            assert!(T::try_parse_from(["t", "connect", "sendgrid", flag, "x"]).is_err(), "{flag} must not parse");
        }
    }

    // ── URL building ───────────────────────────────────────────────────

    #[test]
    fn authorize_path_encodes_query_values() {
        assert_eq!(authorize_path("org-1", "slack", &[]), "/organizations/org-1/integrations/slack/oauth/authorize");
        assert_eq!(
            authorize_path("org-1", "jira", &q(&[("tier", "read_only"), ("site", "https://acme.atlassian.net")])),
            "/organizations/org-1/integrations/jira/oauth/authorize?tier=read_only&site=https%3A%2F%2Facme.atlassian.net"
        );
    }

    #[test]
    fn handoff_url_focuses_and_connects_on_the_dashboard() {
        assert_eq!(
            handoff_url("https://smoo.ai/", "sendgrid"),
            "https://smoo.ai/apps/integrations?focus=sendgrid&connect=sendgrid"
        );
        assert_eq!(
            handoff_url("http://localhost:3000", "twilio"),
            "http://localhost:3000/apps/integrations?focus=twilio&connect=twilio"
        );
        assert_eq!(handoff_url("https://smoo.ai", "icloud"), "https://smoo.ai/apps/booking");
    }

    // ── summary parsing ────────────────────────────────────────────────

    fn summary() -> Value {
        json!({ "integrations": [
            { "type": "google", "connected": true, "accounts": [
                { "id": "g1", "userEmail": "a@x.com", "accountLabel": null, "isDefault": true, "permissionTier": "read_only", "purpose": "features", "ownerUserId": null, "status": "active" },
                { "id": "g2", "userEmail": "a@x.com", "accountLabel": null, "isDefault": false, "permissionTier": "analytics", "purpose": "analytics", "ownerUserId": null, "status": "active" },
                { "id": "g3", "userEmail": "b@x.com", "accountLabel": null, "isDefault": false, "permissionTier": "read_only", "purpose": "features", "ownerUserId": null, "status": "error" }
            ]},
            { "type": "slack", "connected": false, "accounts": [] },
            { "type": "hubspot", "connected": true, "accounts": [
                { "id": "h1", "userEmail": null, "accountLabel": null, "isDefault": true, "permissionTier": "full_access", "ownerUserId": null, "status": "active" }
            ]}
        ]})
    }

    #[test]
    fn active_account_ids_filters_status_and_purpose() {
        let s = summary();
        assert_eq!(active_account_ids(&s, "google", Some("features")), Some(vec!["g1".to_string()]));
        assert_eq!(active_account_ids(&s, "google", Some("analytics")), Some(vec!["g2".to_string()]));
        assert_eq!(active_account_ids(&s, "google", None), Some(vec!["g1".to_string(), "g2".to_string()]));
        assert_eq!(active_account_ids(&s, "slack", None), Some(vec![]));
        assert_eq!(active_account_ids(&s, "hubspot", None), Some(vec!["h1".to_string()]));
        assert_eq!(active_account_ids(&s, "zoom", None), None, "provider absent from the summary");
        assert_eq!(active_account_ids(&json!({}), "slack", None), None);
    }

    #[test]
    fn summary_purpose_only_for_multi_purpose_providers() {
        assert_eq!(summary_purpose("google", None).as_deref(), Some("features"));
        assert_eq!(summary_purpose("google", Some("gmail-restricted")).as_deref(), Some("gmail_restricted"));
        assert_eq!(summary_purpose("microsoft", Some("directory")).as_deref(), Some("directory"));
        assert_eq!(summary_purpose("meta", Some("ads")), None);
    }

    #[test]
    fn account_line_renders_identity_and_flags() {
        let s = summary();
        let a = &summary_entry(&s, "google").unwrap()["accounts"][0];
        assert_eq!(account_line(a), "a@x.com [active] (features, read_only, default)");
    }

    #[test]
    fn authorize_error_names_the_fix_for_m2m() {
        let e = authorize_error("slack", &anyhow::anyhow!("GET /x returned HTTP 401 Unauthorized: {{}}"));
        assert!(e.to_string().contains("smoo auth login"), "{e}");
        let e = authorize_error("slack", &anyhow::anyhow!("GET /x returned HTTP 500 Internal Server Error: boom"));
        assert!(e.to_string().contains("could not start the slack connect"), "{e}");
    }

    // ── poll termination ───────────────────────────────────────────────

    fn slack_summary(ids: &[&str]) -> Value {
        let accounts: Vec<Value> = ids
            .iter()
            .map(|id| json!({ "id": id, "status": "active", "isDefault": true, "permissionTier": "read_only" }))
            .collect();
        json!({ "integrations": [{ "type": "slack", "connected": !ids.is_empty(), "accounts": accounts }] })
    }

    #[tokio::test]
    async fn poll_stops_when_the_provider_goes_active() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let out = poll_until_connected(
            move || {
                let n = c.fetch_add(1, Ordering::SeqCst);
                async move {
                    match n {
                        0 => Ok(slack_summary(&[])),
                        1 => Err(anyhow::anyhow!("GET returned HTTP 502")), // transient — tolerated
                        _ => Ok(slack_summary(&["s1"])),
                    }
                }
            },
            "slack",
            None,
            &HashSet::new(),
            Duration::from_millis(1),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(out, PollOutcome::Connected(vec!["s1".to_string()]));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn poll_ignores_accounts_that_were_already_active() {
        let baseline: HashSet<String> = ["s1".to_string()].into_iter().collect();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let out = poll_until_connected(
            move || {
                let n = c.fetch_add(1, Ordering::SeqCst);
                async move { Ok(if n < 2 { slack_summary(&["s1"]) } else { slack_summary(&["s1", "s2"]) }) }
            },
            "slack",
            None,
            &baseline,
            Duration::from_millis(1),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(out, PollOutcome::Connected(vec!["s2".to_string()]));
    }

    #[tokio::test]
    async fn poll_times_out_when_nothing_connects() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let started = Instant::now();
        let out = poll_until_connected(
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                async { Ok(slack_summary(&[])) }
            },
            "slack",
            None,
            &HashSet::new(),
            Duration::from_millis(5),
            Duration::from_millis(40),
        )
        .await;
        assert_eq!(out, PollOutcome::TimedOut);
        assert!(calls.load(Ordering::SeqCst) >= 2, "polled more than once");
        assert!(started.elapsed() < Duration::from_secs(5), "returned promptly after the deadline");
    }

    #[tokio::test]
    async fn poll_waits_on_the_requested_google_purpose_only() {
        // An analytics grant landing must not satisfy a `features` connect.
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let out = poll_until_connected(
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                async {
                    Ok(json!({ "integrations": [{ "type": "google", "connected": true, "accounts": [
                        { "id": "g2", "status": "active", "purpose": "analytics" }
                    ]}]}))
                }
            },
            "google",
            Some("features"),
            &HashSet::new(),
            Duration::from_millis(2),
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(out, PollOutcome::TimedOut);
    }

    // ── sendgrid (unchanged) ───────────────────────────────────────────

    #[test]
    fn create_body_has_required_fields_and_omits_absent_from_name() {
        let body = build_create_body("sender@acme.com", "inbound@acme.com", None, "SG.secret");
        assert_eq!(body["apiKey"], "SG.secret");
        assert_eq!(body["fromEmail"], "sender@acme.com");
        assert_eq!(body["inboundEmail"], "inbound@acme.com");
        assert!(body.get("fromName").is_none());
    }

    #[test]
    fn create_body_includes_from_name_when_set() {
        let body = build_create_body("s@acme.com", "in@acme.com", Some("Acme Support"), "SG.k");
        assert_eq!(body["fromName"], "Acme Support");
    }

    #[test]
    fn test_body_carries_recipient() {
        assert_eq!(build_test_body("dev@smoo.ai"), json!({ "to": "dev@smoo.ai" }));
    }

    #[test]
    fn resolve_api_key_prefers_env() {
        assert_eq!(resolve_api_key(Some("SG.fromenv".to_string())).unwrap(), "SG.fromenv");
    }

    #[test]
    fn resolve_api_key_trims_env() {
        assert_eq!(resolve_api_key(Some("  SG.padded \n".to_string())).unwrap(), "SG.padded");
    }

    #[test]
    fn resolve_api_key_errors_without_tty_when_blank() {
        // In `cargo test` stdin is not a TTY, so a blank/absent env value
        // must error rather than block on a prompt.
        assert!(resolve_api_key(None).is_err());
        assert!(resolve_api_key(Some("   ".to_string())).is_err());
    }
}

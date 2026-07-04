//! `th api crm contacts …` — CRM contacts via the platform API,
//! authenticated as the logged-in *user* (`smooai-user.json`), so
//! writes are attributed to a real person (e.g. tara@offsetwell.com)
//! rather than an M2M client. SMOODEV-1735.
//!
//! `import` is an idempotent bulk upsert from a JSON array file. The
//! dedup key is the lowercased email, falling back to the last 10
//! digits of the phone. Re-running adds zero new rows. `--dry-run`
//! resolves the org, parses the file, fetches the existing contacts,
//! and prints what it WOULD do without writing.

use std::collections::HashMap;

use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::{print_json, read_body};
use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// Contact records (list / get / create / update / import).
    Contacts {
        #[command(subcommand)]
        cmd: ContactsCmd,
    },
    /// Company / account records (list / show / upsert).
    Companies {
        #[command(subcommand)]
        cmd: CompaniesCmd,
    },
    /// Deals — your sales pipeline (list / show / create / move).
    Deals {
        #[command(subcommand)]
        cmd: DealsCmd,
    },
}

#[derive(Subcommand)]
pub enum CompaniesCmd {
    /// List companies for the org.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Filter to companies matching this search term.
        #[arg(long)]
        search: Option<String>,
        /// Maximum number of companies to return.
        #[arg(long, default_value = "50")]
        limit: u32,
        /// Print raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Show a single company by id.
    Show {
        /// The company id from `th api crm companies list`.
        company_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create or update a company, matched by name (case-insensitive) or
    /// domain. Safe to re-run — a second call patches the same row.
    Upsert {
        /// Company name (the match key).
        name: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Website domain (e.g. rpmpizza.com) — also a match key.
        #[arg(long)]
        domain: Option<String>,
        /// Industry (free text).
        #[arg(long)]
        industry: Option<String>,
        /// Website URL.
        #[arg(long)]
        website: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum DealsCmd {
    /// List deals as a pipeline view (totals + table).
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Filter to a single stage.
        #[arg(long)]
        stage: Option<String>,
        /// Maximum number of deals to return.
        #[arg(long, default_value = "50")]
        limit: u32,
        /// Print raw JSON instead of the pipeline view.
        #[arg(long)]
        json: bool,
    },
    /// Show a single deal by id.
    Show {
        /// The deal id from `th api crm deals list`.
        deal_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a deal, matched by title (case-insensitive). Safe to
    /// re-run — an existing deal with the same title is left untouched
    /// (use `move` to change its stage).
    Create {
        /// Deal title (the match key).
        title: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Deal value in dollars (e.g. 5500).
        #[arg(long)]
        value: Option<f64>,
        /// Pipeline stage (free text, e.g. "closed_won", "discovery").
        #[arg(long)]
        stage: Option<String>,
        /// Link to a company id.
        #[arg(long)]
        company: Option<String>,
        /// Link to a contact id.
        #[arg(long)]
        contact: Option<String>,
        /// Close date (ISO-8601, e.g. 2026-07-02).
        #[arg(long = "close-date")]
        close_date: Option<String>,
    },
    /// Move a deal to a new stage.
    Move {
        /// The deal id from `th api crm deals list`.
        deal_id: String,
        /// New stage (free text).
        stage: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ContactsCmd {
    /// List contacts for the org.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Filter to contacts matching this search term.
        #[arg(long)]
        search: Option<String>,
        /// Maximum number of contacts to return.
        #[arg(long, default_value = "50")]
        limit: u32,
    },
    /// Get a single contact by id.
    Get {
        /// The contact id from `th api crm contacts list`.
        contact_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a contact from a JSON body (file path, or `-` for stdin).
    Create {
        /// JSON body (file path, or `-` for stdin) with the new contact's fields.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update a contact from a JSON body (file path, or `-` for stdin).
    Update {
        /// The contact id from `th api crm contacts list`.
        contact_id: String,
        /// JSON body (file path, or `-` for stdin) with the fields to patch.
        body: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Idempotent bulk upsert from a JSON array file. Dedup key:
    /// lowercased email, else last-10 phone digits. `--dry-run` to
    /// preview without writing.
    Import {
        /// Path to a JSON file containing an array of contact objects.
        file: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Preview what would be created/updated without writing.
        #[arg(long)]
        dry_run: bool,
        /// Minimum delay between API writes, in ms. The contacts API rate
        /// limits at 100 requests / 60s per auth token, so the default
        /// (700ms ≈ 85/min) stays safely under it. On a rate-limit error the
        /// import also waits 61s and retries.
        #[arg(long, default_value = "700")]
        rate_ms: u64,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Contacts { cmd } => contacts(cmd).await,
        Cmd::Companies { cmd } => companies(cmd).await,
        Cmd::Deals { cmd } => deals(cmd).await,
    }
}

/// Resolve the org id for user-authenticated calls. The user session
/// doesn't persist an active org, so this is `--org` flag → `SMOOAI_ORG_ID`.
fn resolve_org(override_org: Option<String>) -> Result<String> {
    if let Some(o) = override_org.filter(|s| !s.trim().is_empty()) {
        return Ok(o);
    }
    if let Ok(o) = std::env::var("SMOOAI_ORG_ID") {
        if !o.trim().is_empty() {
            return Ok(o);
        }
    }
    anyhow::bail!("no org specified — pass `--org <id>` or set SMOOAI_ORG_ID")
}

async fn contacts(cmd: ContactsCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        ContactsCmd::List { org, search, limit } => {
            let org = resolve_org(org)?;
            let mut path = format!("/organizations/{org}/crm/contacts?limit={limit}");
            if let Some(s) = search.filter(|s| !s.trim().is_empty()) {
                path.push_str(&format!("&search={}", urlencoding::encode(&s)));
            }
            print_json(&client.get(&path).await.context("GET contacts")?);
        }
        ContactsCmd::Get { contact_id, org } => {
            let org = resolve_org(org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{org}/crm/contacts/{contact_id}"))
                    .await
                    .context("GET contact")?,
            );
        }
        ContactsCmd::Create { body, org } => {
            let org = resolve_org(org)?;
            let body = read_body(&body)?;
            print_json(
                &client
                    .post(&format!("/organizations/{org}/crm/contacts"), &body)
                    .await
                    .context("POST contact")?,
            );
        }
        ContactsCmd::Update { contact_id, body, org } => {
            let org = resolve_org(org)?;
            let body = read_body(&body)?;
            print_json(
                &client
                    .patch(&format!("/organizations/{org}/crm/contacts/{contact_id}"), &body)
                    .await
                    .context("PATCH contact")?,
            );
        }
        ContactsCmd::Import { file, org, dry_run, rate_ms } => {
            let org = resolve_org(org)?;
            import(&client, &org, &file, dry_run, rate_ms).await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Companies
// ---------------------------------------------------------------------------

async fn companies(cmd: CompaniesCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        CompaniesCmd::List { org, search, limit, json } => {
            let org = resolve_org(org)?;
            let mut path = format!("/organizations/{org}/crm/companies?limit={limit}");
            if let Some(s) = search.filter(|s| !s.trim().is_empty()) {
                path.push_str(&format!("&search={}", urlencoding::encode(&s)));
            }
            let body = client.get(&path).await.context("GET companies")?;
            if json {
                print_json(&body);
            } else {
                render_companies(&body);
            }
        }
        CompaniesCmd::Show { company_id, org } => {
            let org = resolve_org(org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{org}/crm/companies/{company_id}"))
                    .await
                    .context("GET company")?,
            );
        }
        CompaniesCmd::Upsert {
            name,
            org,
            domain,
            industry,
            website,
        } => {
            let org = resolve_org(org)?;
            upsert_company(&client, &org, &name, domain, industry, website).await?;
        }
    }
    Ok(())
}

/// Find an existing company by case-insensitive name, or by domain.
async fn find_company(client: &UserClient, org: &str, name: &str, domain: Option<&str>) -> Result<Option<Value>> {
    let list = client
        .get(&format!("/organizations/{org}/crm/companies?limit=200"))
        .await
        .context("GET companies for match")?;
    let name_l = name.trim().to_lowercase();
    let dom_l = domain.map(|d| d.trim().to_lowercase()).filter(|d| !d.is_empty());
    Ok(list.as_array().and_then(|arr| {
        arr.iter()
            .find(|c| {
                let cn = c.get("name").and_then(Value::as_str).unwrap_or_default().trim().to_lowercase();
                let cd = c.get("domain").and_then(Value::as_str).map(|s| s.trim().to_lowercase());
                cn == name_l || (dom_l.is_some() && cd == dom_l)
            })
            .cloned()
    }))
}

async fn upsert_company(client: &UserClient, org: &str, name: &str, domain: Option<String>, industry: Option<String>, website: Option<String>) -> Result<()> {
    let mut body = json!({ "name": name });
    if let Some(d) = domain.filter(|s| !s.trim().is_empty()) {
        body["domain"] = json!(d);
    }
    if let Some(i) = industry.filter(|s| !s.trim().is_empty()) {
        body["industry"] = json!(i);
    }
    if let Some(w) = website.filter(|s| !s.trim().is_empty()) {
        body["website"] = json!(w);
    }

    let existing = find_company(client, org, name, body.get("domain").and_then(Value::as_str)).await?;
    if let Some(c) = existing {
        let id = c.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        client
            .patch(&format!("/organizations/{org}/crm/companies/{id}"), &body)
            .await
            .context("PATCH company")?;
        println!("  {} updated company {} {}", "↻".yellow(), id.dimmed(), name.bold());
    } else {
        let r = client
            .post(&format!("/organizations/{org}/crm/companies"), &body)
            .await
            .context("POST company")?;
        let id = r.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
        println!("  {} created company {} {}", "✚".green(), id.dimmed(), name.bold());
    }
    Ok(())
}

fn render_companies(body: &Value) {
    let cos = body.as_array().cloned().unwrap_or_default();
    let count = format!("({})", cos.len());
    println!();
    println!("  {} {}", "Companies".bold(), count.dimmed());
    if cos.is_empty() {
        println!("\n  {}\n", "none".dimmed());
        return;
    }
    let (h_name, h_dom, h_ind) = (format!("{:<28}", "NAME"), format!("{:<26}", "DOMAIN"), format!("{:<20}", "INDUSTRY"));
    println!();
    println!("  {}  {}  {}", h_name.dimmed(), h_dom.dimmed(), h_ind.dimmed());
    for c in &cos {
        println!(
            "  {:<28}  {:<26}  {:<20}",
            truncate(c.get("name").and_then(Value::as_str).unwrap_or("—"), 28),
            truncate(c.get("domain").and_then(Value::as_str).unwrap_or("—"), 26),
            truncate(c.get("industry").and_then(Value::as_str).unwrap_or("—"), 20),
        );
    }
    println!();
}

// ---------------------------------------------------------------------------
// Deals (pipeline)
// ---------------------------------------------------------------------------

async fn deals(cmd: DealsCmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        DealsCmd::List { org, stage, limit, json } => {
            let org = resolve_org(org)?;
            let mut path = format!("/organizations/{org}/crm/deals?limit={limit}");
            if let Some(s) = stage.filter(|s| !s.trim().is_empty()) {
                path.push_str(&format!("&stage={}", urlencoding::encode(&s)));
            }
            let body = client.get(&path).await.context("GET deals")?;
            if json {
                print_json(&body);
            } else {
                render_deals(&body);
            }
        }
        DealsCmd::Show { deal_id, org } => {
            let org = resolve_org(org)?;
            show_deal(&client, &org, &deal_id).await?;
        }
        DealsCmd::Create {
            title,
            org,
            value,
            stage,
            company,
            contact,
            close_date,
        } => {
            let org = resolve_org(org)?;
            create_deal(&client, &org, &title, value, stage, company, contact, close_date).await?;
        }
        DealsCmd::Move { deal_id, stage, org } => {
            let org = resolve_org(org)?;
            client
                .patch(&format!("/organizations/{org}/crm/deals/{deal_id}"), &json!({ "stage": stage }))
                .await
                .context("PATCH deal stage")?;
            println!("  {} moved deal {} → {}", "↻".yellow(), deal_id.dimmed(), stage_color(&stage));
        }
    }
    Ok(())
}

async fn find_deal(client: &UserClient, org: &str, title: &str) -> Result<Option<Value>> {
    let list = client
        .get(&format!("/organizations/{org}/crm/deals?limit=200"))
        .await
        .context("GET deals for match")?;
    let t = title.trim().to_lowercase();
    Ok(list.as_array().and_then(|arr| {
        arr.iter()
            .find(|d| d.get("title").and_then(Value::as_str).unwrap_or_default().trim().to_lowercase() == t)
            .cloned()
    }))
}

#[allow(clippy::too_many_arguments)]
async fn create_deal(
    client: &UserClient,
    org: &str,
    title: &str,
    value: Option<f64>,
    stage: Option<String>,
    company: Option<String>,
    contact: Option<String>,
    close_date: Option<String>,
) -> Result<()> {
    if let Some(existing) = find_deal(client, org, title).await? {
        let id = existing.get("id").and_then(Value::as_str).unwrap_or("?");
        println!(
            "  {} deal {} already exists {} — left as-is (use `deals move` to change stage)",
            "•".cyan(),
            id.dimmed(),
            title.bold()
        );
        return Ok(());
    }
    let stage = stage.filter(|s| !s.trim().is_empty());
    let mut body = json!({ "title": title });
    if let Some(v) = value {
        body["value"] = json!(v);
    }
    if let Some(s) = &stage {
        body["stage"] = json!(s);
    }
    if let Some(c) = company.filter(|s| !s.trim().is_empty()) {
        body["companyId"] = json!(resolve_company_id(client, org, &c).await?);
    }
    if let Some(c) = contact.filter(|s| !s.trim().is_empty()) {
        body["contactId"] = json!(resolve_contact_id(client, org, &c).await?);
    }
    if let Some(d) = close_date.filter(|s| !s.trim().is_empty()) {
        body["closeDate"] = json!(d);
    }
    let r = client.post(&format!("/organizations/{org}/crm/deals"), &body).await.context("POST deal")?;
    let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
    println!(
        "  {} created deal {} {}  {}  {}",
        "✚".green(),
        id.dimmed(),
        title.bold(),
        stage_color(stage.as_deref().unwrap_or("")),
        fmt_money(value),
    );
    Ok(())
}

/// Resolve a company id from a uuid (used as-is) or a name (looked up).
async fn resolve_company_id(client: &UserClient, org: &str, s: &str) -> Result<String> {
    if looks_like_uuid(s) {
        return Ok(s.to_string());
    }
    match find_company(client, org, s, None).await? {
        Some(c) => c.get("id").and_then(Value::as_str).map(str::to_string).context("matched company has no id"),
        None => anyhow::bail!("no company matches '{s}' — create it first: `th api crm companies upsert \"{s}\"`"),
    }
}

/// Resolve a contact id from a uuid (used as-is) or an email/name (looked up).
async fn resolve_contact_id(client: &UserClient, org: &str, s: &str) -> Result<String> {
    if looks_like_uuid(s) {
        return Ok(s.to_string());
    }
    // Match locally against every contact — the `?search=` param doesn't
    // reliably hit a full email address, so reuse the same paged fetch as
    // `import` and compare exact email / "first last" here.
    let contacts = fetch_all(client, org, std::time::Duration::from_millis(0)).await?;
    let sl = s.trim().to_lowercase();
    let found = contacts.iter().find(|c| {
        let email = c.get("email").and_then(Value::as_str).unwrap_or_default().to_lowercase();
        let name = format!(
            "{} {}",
            c.get("firstName").and_then(Value::as_str).unwrap_or(""),
            c.get("lastName").and_then(Value::as_str).unwrap_or("")
        )
        .trim()
        .to_lowercase();
        email == sl || name == sl
    });
    match found {
        Some(c) => c.get("id").and_then(Value::as_str).map(str::to_string).context("matched contact has no id"),
        None => anyhow::bail!("no contact matches '{s}' — create it first: `th api crm contacts create`"),
    }
}

async fn show_deal(client: &UserClient, org: &str, deal_id: &str) -> Result<()> {
    let d = client.get(&format!("/organizations/{org}/crm/deals/{deal_id}")).await.context("GET deal")?;
    println!();
    println!("  {}", d.get("title").and_then(Value::as_str).unwrap_or("—").bold());
    println!("  {} {}", "Stage  ".dimmed(), stage_color(d.get("stage").and_then(Value::as_str).unwrap_or("")));
    println!("  {} {}", "Value  ".dimmed(), fmt_money(as_money(d.get("value"))).bold());
    println!("  {} {}", "Close  ".dimmed(), short_date(d.get("closeDate")));
    if let Some(cid) = d.get("companyId").and_then(Value::as_str) {
        let name = client
            .get(&format!("/organizations/{org}/crm/companies/{cid}"))
            .await
            .ok()
            .and_then(|c| c.get("name").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| cid.to_string());
        println!("  {} {}", "Company".dimmed(), name);
    }
    if let Some(cid) = d.get("contactId").and_then(Value::as_str) {
        let label = client
            .get(&format!("/organizations/{org}/crm/contacts/{cid}"))
            .await
            .ok()
            .map(|c| contact_label(&c))
            .unwrap_or_else(|| cid.to_string());
        println!("  {} {}", "Contact".dimmed(), label);
    }
    println!("  {} {}", "id     ".dimmed(), deal_id.dimmed());
    println!();
    Ok(())
}

fn contact_label(c: &Value) -> String {
    let name = format!(
        "{} {}",
        c.get("firstName").and_then(Value::as_str).unwrap_or(""),
        c.get("lastName").and_then(Value::as_str).unwrap_or("")
    )
    .trim()
    .to_string();
    let email = c.get("email").and_then(Value::as_str).unwrap_or("");
    match (name.is_empty(), email.is_empty()) {
        (false, false) => format!("{name} <{email}>"),
        (false, true) => name,
        _ => email.to_string(),
    }
}

fn render_deals(body: &Value) {
    let deals = body.as_array().cloned().unwrap_or_default();
    let pipeline: f64 = deals.iter().filter_map(|d| as_money(d.get("value"))).sum();
    println!();
    println!("  {}", "Deals".bold());
    println!(
        "  {} {}     {} {}",
        "Total".dimmed(),
        deals.len().to_string().bold(),
        "Pipeline".dimmed(),
        fmt_money(Some(pipeline)).bold()
    );
    if deals.is_empty() {
        println!("\n  {}\n", "no deals yet".dimmed());
        return;
    }
    let (h_title, h_stage, h_value, h_close) = (
        format!("{:<38}", "TITLE"),
        format!("{:<14}", "STAGE"),
        format!("{:>12}", "VALUE"),
        format!("{:<10}", "CLOSE"),
    );
    println!();
    println!("  {}  {}  {}  {}", h_title.dimmed(), h_stage.dimmed(), h_value.dimmed(), h_close.dimmed());
    for d in &deals {
        let title = truncate(d.get("title").and_then(Value::as_str).unwrap_or("—"), 38);
        let value = format!("{:>12}", fmt_money(as_money(d.get("value"))));
        println!(
            "  {:<38}  {}  {}  {}",
            title,
            stage_cell(d.get("stage").and_then(Value::as_str).unwrap_or(""), 14),
            value,
            short_date(d.get("closeDate")),
        );
    }
    println!();
}

// --- small formatting helpers ------------------------------------------------

/// A JSON number OR numeric-string (drizzle serializes numeric(15,2) as a
/// string) as an f64.
fn as_money(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
}

fn fmt_money(v: Option<f64>) -> String {
    match v {
        Some(n) => format!("${}", group_thousands(n)),
        None => "—".to_string(),
    }
}

/// `1234567.5` → `1,234,567.50`. Comma-grouped, always 2 decimals.
fn group_thousands(n: f64) -> String {
    let neg = n < 0.0;
    let cents = (n.abs() * 100.0).round() as u64;
    let (dollars, frac) = (cents / 100, cents % 100);
    let ds = dollars.to_string();
    let bytes = ds.as_bytes();
    let len = bytes.len();
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    format!("{}{}.{:02}", if neg { "-" } else { "" }, out, frac)
}

fn short_date(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .map(|s| s.chars().take(10).collect::<String>())
        .unwrap_or_else(|| "—".into())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// Color a stage label (green=won, red=lost, cyan=otherwise). No padding.
fn stage_color(stage: &str) -> String {
    let s = stage.trim();
    if s.is_empty() {
        return "—".dimmed().to_string();
    }
    let low = s.to_lowercase();
    if low.contains("won") {
        s.green().to_string()
    } else if low.contains("lost") {
        s.red().to_string()
    } else {
        s.cyan().to_string()
    }
}

/// Left-pad a stage label to `width` THEN color it, so table columns stay
/// aligned (padding a pre-colored string would count the ANSI escapes).
fn stage_cell(stage: &str, width: usize) -> String {
    let plain = truncate(stage.trim(), width);
    let padded = format!("{plain:<width$}");
    let low = stage.to_lowercase();
    if plain.is_empty() {
        format!("{:<width$}", "—").dimmed().to_string()
    } else if low.contains("won") {
        padded.green().to_string()
    } else if low.contains("lost") {
        padded.red().to_string()
    } else {
        padded.cyan().to_string()
    }
}

/// True for a canonical 8-4-4-4-12 hex uuid, so `--company`/`--contact`
/// accept either an id or a human name.
fn looks_like_uuid(s: &str) -> bool {
    let s = s.trim();
    s.len() == 36
        && s.chars()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// Lowercased, trimmed email if it looks like an email (`x@y`).
fn norm_email(v: &Value) -> Option<String> {
    let s = v.get("email")?.as_str()?.trim().to_lowercase();
    if s.contains('@') && !s.contains(';') && !s.contains(' ') {
        Some(s)
    } else {
        None
    }
}

/// Last 10 digits of the phone, if there are at least 10.
fn norm_phone(v: &Value) -> Option<String> {
    let raw = v.get("phone")?.as_str()?;
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    if digits.len() >= 10 {
        Some(digits[digits.len() - 10..].to_string())
    } else {
        None
    }
}

/// Fetch every contact in the org, paging in blocks of 200. Paces between
/// pages so the existing-contacts scan doesn't itself trip the rate limit.
async fn fetch_all(client: &UserClient, org: &str, rate: std::time::Duration) -> Result<Vec<Value>> {
    let mut all = Vec::new();
    let mut offset = 0u32;
    loop {
        let path = format!("/organizations/{org}/crm/contacts?limit=200&offset={offset}");
        let body = client.get(&path).await.with_context(|| format!("GET contacts (offset {offset})"))?;
        let page = body.as_array().cloned().unwrap_or_default();
        let n = page.len();
        all.extend(page);
        if n < 200 {
            break;
        }
        offset += 200;
        tokio::time::sleep(rate).await;
    }
    Ok(all)
}

/// A single write to perform.
#[derive(Clone, Copy)]
enum Op<'a> {
    Create,
    Update(&'a str),
}

/// Execute one write, pacing first and retrying once-per-minute on the
/// contacts API's "100 requests / 60s" rate-limit error (HTTP 400 whose body
/// mentions "rate limit"). Up to 6 retries (~6 min of backoff) before giving up.
async fn exec(client: &UserClient, org: &str, op: Op<'_>, body: &Value, rate: std::time::Duration) -> Result<Value> {
    let mut attempt = 0u32;
    loop {
        tokio::time::sleep(rate).await;
        let res = match op {
            Op::Create => client.post(&format!("/organizations/{org}/crm/contacts"), body).await,
            Op::Update(id) => client.patch(&format!("/organizations/{org}/crm/contacts/{id}"), body).await,
        };
        match res {
            Ok(v) => return Ok(v),
            Err(e) if e.to_string().contains("rate limit") && attempt < 6 => {
                attempt += 1;
                eprintln!("    rate limited — waiting 61s then retrying (attempt {attempt})");
                tokio::time::sleep(std::time::Duration::from_secs(61)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn import(client: &UserClient, org: &str, file: &str, dry_run: bool, rate_ms: u64) -> Result<()> {
    let rate = std::time::Duration::from_millis(rate_ms);
    let parsed = read_body(file)?;
    let items = parsed.as_array().context("import file must contain a JSON array of contact objects")?;

    if let Some(u) = UserClient::user_label() {
        println!();
        println!(
            "  {} importing {} contacts into org {} as {}{}",
            "●".cyan(),
            items.len().to_string().bold(),
            org.cyan(),
            u.bold(),
            if dry_run { "  [dry-run]".yellow().to_string() } else { String::new() }
        );
    }

    // Build lookup maps from the existing contacts.
    let existing = fetch_all(client, org, rate).await?;
    let mut email_to_id: HashMap<String, String> = HashMap::new();
    let mut phone_to_id: HashMap<String, String> = HashMap::new();
    for c in &existing {
        let Some(id) = c.get("id").and_then(|v| v.as_str()) else { continue };
        if let Some(e) = norm_email(c) {
            email_to_id.entry(e).or_insert_with(|| id.to_string());
        }
        if let Some(p) = norm_phone(c) {
            phone_to_id.entry(p).or_insert_with(|| id.to_string());
        }
    }

    let mut created = 0u32;
    let mut updated = 0u32;
    let mut skipped: HashMap<String, u32> = HashMap::new();

    for item in items {
        if !item.is_object() {
            *skipped.entry("not an object".into()).or_insert(0) += 1;
            continue;
        }
        let email = norm_email(item);
        let phone = norm_phone(item);
        let (key_kind, key) = match (&email, &phone) {
            (Some(e), _) => ("email", e.clone()),
            (None, Some(p)) => ("phone", p.clone()),
            (None, None) => {
                *skipped.entry("no email or phone".into()).or_insert(0) += 1;
                continue;
            }
        };
        let label = email.clone().unwrap_or_else(|| format!("phone:{key}"));

        let existing_id = if key_kind == "email" {
            email_to_id.get(&key).cloned()
        } else {
            phone_to_id.get(&key).cloned()
        };

        if let Some(id) = existing_id {
            if dry_run {
                println!("  {} would update {} {}", "↻".yellow(), id.dimmed(), label.dimmed());
            } else {
                exec(client, org, Op::Update(&id), item, rate)
                    .await
                    .with_context(|| format!("update {label}"))?;
                println!("  {} updated {} {}", "↻".yellow(), id.dimmed(), label.dimmed());
            }
            updated += 1;
        } else if dry_run {
            println!("  {} would create {}", "✚".green(), label.bold());
            created += 1;
            // Reserve the key so a duplicate later in the file dedups in dry-run too.
            remember(&mut email_to_id, &mut phone_to_id, &email, &phone, "(dry-run)");
        } else {
            let resp = exec(client, org, Op::Create, item, rate).await.with_context(|| format!("create {label}"))?;
            let new_id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            println!("  {} created {} {}", "✚".green(), new_id.dimmed(), label.bold());
            created += 1;
            remember(&mut email_to_id, &mut phone_to_id, &email, &phone, &new_id);
        }
    }

    println!();
    println!(
        "  {} {} created, {} updated, {} skipped",
        "✓".green().bold(),
        created.to_string().green().bold(),
        updated.to_string().yellow().bold(),
        skipped.values().sum::<u32>().to_string().bold()
    );
    let mut reasons: Vec<(&String, &u32)> = skipped.iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(a.1));
    for (reason, n) in reasons {
        println!("      {} {} {}", "⊘".dimmed(), n.to_string().dimmed(), reason.dimmed());
    }
    println!();
    Ok(())
}

/// Record a key → id mapping so within-file duplicates upsert rather
/// than double-insert.
fn remember(email_to_id: &mut HashMap<String, String>, phone_to_id: &mut HashMap<String, String>, email: &Option<String>, phone: &Option<String>, id: &str) {
    if let Some(e) = email {
        email_to_id.insert(e.clone(), id.to_string());
    }
    if let Some(p) = phone {
        phone_to_id.insert(p.clone(), id.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{group_thousands, looks_like_uuid, norm_email, norm_phone};
    use serde_json::json;

    #[test]
    fn money_is_comma_grouped_with_two_decimals() {
        assert_eq!(group_thousands(5500.0), "5,500.00");
        assert_eq!(group_thousands(1234567.5), "1,234,567.50");
        assert_eq!(group_thousands(0.0), "0.00");
        assert_eq!(group_thousands(299.0), "299.00");
        // Rounds cents, doesn't truncate.
        assert_eq!(group_thousands(99.995), "100.00");
    }

    #[test]
    fn uuid_detection_gates_name_vs_id() {
        assert!(looks_like_uuid("660e8400-e29b-41d4-a716-446655440000"));
        assert!(!looks_like_uuid("RPM Pizza"));
        assert!(!looks_like_uuid("tim.fikes@rpmpizza.com"));
        // right length, wrong dash positions
        assert!(!looks_like_uuid("660e8400e29b-41d4-a716-4466554400001"));
    }

    #[test]
    fn email_is_lowercased_and_trimmed() {
        assert_eq!(norm_email(&json!({ "email": "  Stephen@DasBBQ.com " })), Some("stephen@dasbbq.com".into()));
    }

    #[test]
    fn email_missing_or_malformed_is_none() {
        assert_eq!(norm_email(&json!({})), None);
        assert_eq!(norm_email(&json!({ "email": "" })), None);
        assert_eq!(norm_email(&json!({ "email": "not-an-email" })), None);
        // Two emails jammed in one field (Tara's row) → rejected, not guessed.
        assert_eq!(norm_email(&json!({ "email": "a@b.com; c@d.com" })), None);
    }

    #[test]
    fn phone_keys_on_last_10_digits() {
        assert_eq!(norm_phone(&json!({ "phone": "(404) 281-4855" })), Some("4042814855".into()));
        // Leading country code → same last-10 key, so +1 and bare forms dedup.
        assert_eq!(norm_phone(&json!({ "phone": "+1 404 281 4855" })), Some("4042814855".into()));
    }

    #[test]
    fn phone_too_short_is_none() {
        assert_eq!(norm_phone(&json!({ "phone": "12345" })), None);
        assert_eq!(norm_phone(&json!({})), None);
    }
}

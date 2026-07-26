//! `th referrals …` — the org's partner / advocate referral program.
//!
//! One program per org (Phase 1 quota) owns the commission economics;
//! partners under it each get a `partnerCode` that becomes a shareable
//! `https://api.smoo.ai/r/<code>` link. Referred signups are cookied and
//! attributed, and Stripe invoices generate commission rows.
//!
//! Backed by `/organizations/{org_id}/referral-programs…` and
//! `/organizations/{org_id}/referrals/commissions`. SMOODEV-2768.

use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::{print_json, require_active_org, require_authed};

/// Public host that serves the `/r/<code>` tracking redirect.
///
/// This is `api.smoo.ai`, not `smoo.ai` — the handler is an api-prime
/// route, and the marketing site has no `/r/` path (it 404s). The
/// feature doc's prettier `smoo.ai/r/<code>` form is aspirational;
/// point partners at what actually resolves.
const REFERRAL_LINK_BASE: &str = "https://api.smoo.ai/r";

#[derive(Subcommand)]
pub enum Cmd {
    /// Show the org's referral program (economics + status).
    Show {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create the org's referral program.
    Create {
        /// Human-readable name, e.g. "Smoo AI Partner Program".
        name: String,
        /// URL slug scoped to the org.
        #[arg(long, default_value = "partners")]
        slug: String,
        /// Commission percentage of attributed MRR (e.g. 20 for 20%).
        #[arg(long = "rate", default_value_t = 20.0)]
        rate_percent: f64,
        /// Months commissions keep paying per customer. Omit for lifetime.
        #[arg(long = "duration-months")]
        duration_months: Option<i64>,
        /// Days a referral cookie survives.
        #[arg(long = "cookie-days")]
        cookie_days: Option<i64>,
        /// Days post-signup before a commission unlocks (refund guard).
        #[arg(long = "lockout-days")]
        lockout_days: Option<i64>,
        /// How partners get paid.
        #[arg(long = "payout-method", default_value = "manual_csv")]
        payout_method: String,
        /// draft | active | paused | archived.
        #[arg(long, default_value = "active")]
        status: String,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update the org's referral program.
    Update {
        /// New name.
        #[arg(long)]
        name: Option<String>,
        /// New commission percentage (e.g. 25 for 25%).
        #[arg(long = "rate")]
        rate_percent: Option<f64>,
        /// draft | active | paused | archived.
        #[arg(long)]
        status: Option<String>,
        /// manual_csv | stripe_connect | wise.
        #[arg(long = "payout-method")]
        payout_method: Option<String>,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Partners under the program — the people who get paid.
    #[command(visible_alias = "partner")]
    Partners {
        #[command(subcommand)]
        cmd: PartnersCmd,
    },
    /// Print a partner's shareable referral link.
    Link {
        /// Partner email, display name, or code.
        partner: String,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Attributed signups (who came in through a partner link).
    Attributions {
        /// Max rows to return.
        #[arg(long)]
        limit: Option<i64>,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Tracked link visits.
    Visits {
        /// Max rows to return.
        #[arg(long)]
        limit: Option<i64>,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Commissions owed / paid across the org.
    Commissions {
        /// Filter by status, e.g. `held`, `payable`, `paid`.
        #[arg(long)]
        status: Option<String>,
        /// Max rows to return.
        #[arg(long)]
        limit: Option<i64>,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PartnersCmd {
    /// List partners with their codes, links and earnings.
    List {
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Add a partner. They are `active` and paid at `--payout-email` (defaults to their email).
    Add {
        /// Partner display name.
        name: String,
        /// Partner email (their identity in the program).
        #[arg(long)]
        email: String,
        /// Where commissions get sent (PayPal / Wise). Defaults to `--email`.
        #[arg(long = "payout-email")]
        payout_email: Option<String>,
        /// Force a specific referral code instead of a generated one.
        #[arg(long = "code")]
        partner_code: Option<String>,
        /// invited | active | paused | banned.
        #[arg(long, default_value = "active")]
        status: String,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update a partner (matched by email, name, or code).
    Update {
        /// Partner email, display name, or code.
        partner: String,
        /// New payout email.
        #[arg(long = "payout-email")]
        payout_email: Option<String>,
        /// invited | active | paused | banned.
        #[arg(long)]
        status: Option<String>,
        /// pending | w9_on_file | 1099_issued.
        #[arg(long = "tax-form-status")]
        tax_form_status: Option<String>,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove a partner from the program.
    #[command(visible_alias = "rm")]
    Remove {
        /// Partner email, display name, or code.
        partner: String,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Show { org } => {
            let o = require_active_org(&client, org)?;
            let program = load_program(&client, &o).await?;
            render_program(&program);
        }
        Cmd::Create {
            name,
            slug,
            rate_percent,
            duration_months,
            cookie_days,
            lockout_days,
            payout_method,
            status,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let body = build_program_body(&ProgramSpec {
                name: &name,
                slug: &slug,
                rate_percent,
                duration_months,
                cookie_days,
                lockout_days,
                payout_method: &payout_method,
                status: &status,
            });
            let program = client
                .post(&format!("/organizations/{o}/referral-programs"), Some(&body))
                .await
                .context("POST referral program")?;
            render_program(&program);
        }
        Cmd::Update {
            name,
            rate_percent,
            status,
            payout_method,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let program = load_program(&client, &o).await?;
            let id = program_id(&program)?;
            let body = build_program_patch(name.as_deref(), rate_percent, status.as_deref(), payout_method.as_deref());
            let updated = client
                .patch(&format!("/organizations/{o}/referral-programs/{id}"), &body)
                .await
                .context("PATCH referral program")?;
            render_program(&updated);
        }
        Cmd::Partners { cmd } => partners(cmd).await?,
        Cmd::Link { partner, org } => {
            let o = require_active_org(&client, org)?;
            let id = program_id(&load_program(&client, &o).await?)?;
            let found = find_partner(&client, &o, &id, &partner).await?;
            let code = found.get("partnerCode").and_then(Value::as_str).unwrap_or("?");
            let name = found.get("displayName").and_then(Value::as_str).unwrap_or("");
            println!();
            println!("  {}  {}", name.bold(), code.dimmed());
            println!("  {}", referral_link(code).cyan());
            println!();
        }
        Cmd::Attributions { limit, org } => {
            let o = require_active_org(&client, org)?;
            let id = program_id(&load_program(&client, &o).await?)?;
            let path = with_limit(&format!("/organizations/{o}/referral-programs/{id}/attributions"), limit);
            print_json(&client.get(&path).await.context("GET attributions")?);
        }
        Cmd::Visits { limit, org } => {
            let o = require_active_org(&client, org)?;
            let id = program_id(&load_program(&client, &o).await?)?;
            let path = with_limit(&format!("/organizations/{o}/referral-programs/{id}/visits"), limit);
            print_json(&client.get(&path).await.context("GET visits")?);
        }
        Cmd::Commissions { status, limit, org } => {
            let o = require_active_org(&client, org)?;
            let mut path = with_limit(&format!("/organizations/{o}/referrals/commissions"), limit);
            if let Some(s) = status {
                let sep = if path.contains('?') { '&' } else { '?' };
                path = format!("{path}{sep}status={s}");
            }
            print_json(&client.get(&path).await.context("GET commissions")?);
        }
    }
    Ok(())
}

async fn partners(cmd: PartnersCmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        PartnersCmd::List { org } => {
            let o = require_active_org(&client, org)?;
            let id = program_id(&load_program(&client, &o).await?)?;
            let list = client
                .get(&format!("/organizations/{o}/referral-programs/{id}/partners"))
                .await
                .context("GET partners")?;
            render_partners(&list);
        }
        PartnersCmd::Add {
            name,
            email,
            payout_email,
            partner_code,
            status,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let id = program_id(&load_program(&client, &o).await?)?;
            let body = build_partner_body(&name, &email, payout_email.as_deref(), partner_code.as_deref(), &status);
            let partner = client
                .post(&format!("/organizations/{o}/referral-programs/{id}/partners"), Some(&body))
                .await
                .context("POST partner")?;
            render_partners(&json!([partner]));
        }
        PartnersCmd::Update {
            partner,
            payout_email,
            status,
            tax_form_status,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let id = program_id(&load_program(&client, &o).await?)?;
            let found = find_partner(&client, &o, &id, &partner).await?;
            let partner_id = found.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            let body = build_partner_patch(payout_email.as_deref(), status.as_deref(), tax_form_status.as_deref());
            let updated = client
                .patch(&format!("/organizations/{o}/referral-programs/{id}/partners/{partner_id}"), &body)
                .await
                .context("PATCH partner")?;
            render_partners(&json!([updated]));
        }
        PartnersCmd::Remove { partner, org } => {
            let o = require_active_org(&client, org)?;
            let id = program_id(&load_program(&client, &o).await?)?;
            let found = find_partner(&client, &o, &id, &partner).await?;
            let partner_id = found.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            client
                .delete(&format!("/organizations/{o}/referral-programs/{id}/partners/{partner_id}"))
                .await
                .context("DELETE partner")?;
            println!();
            println!("  {} removed {}", "✓".green(), partner.bold());
            println!();
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

/// Load the org's single referral program, erroring with the create
/// hint when none exists yet.
async fn load_program(client: &smooth_api_client::SmoothApiClient, org: &str) -> Result<Value> {
    let list = client
        .get(&format!("/organizations/{org}/referral-programs"))
        .await
        .context("GET referral programs")?;
    first_program(&list).ok_or_else(|| anyhow::anyhow!("no referral program for this org — create one with `th referrals create \"Partner Program\"`"))
}

fn first_program(list: &Value) -> Option<Value> {
    list.get("data")
        .and_then(Value::as_array)
        .or_else(|| list.as_array())
        .and_then(|a| a.first())
        .cloned()
}

fn program_id(program: &Value) -> Result<String> {
    program
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("referral program response has no id"))
}

/// Resolve a partner by email, display name, or code (case-insensitive).
async fn find_partner(client: &smooth_api_client::SmoothApiClient, org: &str, program: &str, needle: &str) -> Result<Value> {
    let list = client
        .get(&format!("/organizations/{org}/referral-programs/{program}/partners"))
        .await
        .context("GET partners")?;
    match_partner(&list, needle).ok_or_else(|| anyhow::anyhow!("no partner matching '{needle}' in this program"))
}

fn match_partner(list: &Value, needle: &str) -> Option<Value> {
    let items = list.get("data").and_then(Value::as_array).or_else(|| list.as_array())?;
    let needle = needle.trim().to_lowercase();
    items
        .iter()
        .find(|p| {
            ["email", "displayName", "partnerCode", "id"]
                .iter()
                .filter_map(|k| p.get(*k).and_then(Value::as_str))
                .any(|v| v.to_lowercase() == needle)
        })
        .cloned()
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

/// The program economics as given on the command line.
pub struct ProgramSpec<'a> {
    pub name: &'a str,
    pub slug: &'a str,
    pub rate_percent: f64,
    pub duration_months: Option<i64>,
    pub cookie_days: Option<i64>,
    pub lockout_days: Option<i64>,
    pub payout_method: &'a str,
    pub status: &'a str,
}

fn build_program_body(spec: &ProgramSpec) -> Value {
    let mut body = json!({
        "name": spec.name,
        "slug": spec.slug,
        "status": spec.status,
        "commissionRateBps": percent_to_bps(spec.rate_percent),
        "payoutMethod": spec.payout_method,
    });
    // Absent `--duration-months` means lifetime commissions, which the
    // API models as an explicit null (a missing key defaults to 12).
    body["commissionDurationMonths"] = match spec.duration_months {
        Some(m) => json!(m),
        None => Value::Null,
    };
    if let Some(d) = spec.cookie_days {
        body["cookieDays"] = json!(d);
    }
    if let Some(d) = spec.lockout_days {
        body["lockoutDays"] = json!(d);
    }
    body
}

fn build_program_patch(name: Option<&str>, rate_percent: Option<f64>, status: Option<&str>, payout_method: Option<&str>) -> Value {
    let mut body = json!({});
    if let Some(n) = name {
        body["name"] = json!(n);
    }
    if let Some(r) = rate_percent {
        body["commissionRateBps"] = json!(percent_to_bps(r));
    }
    if let Some(s) = status {
        body["status"] = json!(s);
    }
    if let Some(p) = payout_method {
        body["payoutMethod"] = json!(p);
    }
    body
}

fn build_partner_body(name: &str, email: &str, payout_email: Option<&str>, partner_code: Option<&str>, status: &str) -> Value {
    let mut body = json!({
        "displayName": name,
        "email": email,
        "payoutEmail": payout_email.unwrap_or(email),
        "status": status,
    });
    if let Some(code) = partner_code {
        body["partnerCode"] = json!(code);
    }
    body
}

fn build_partner_patch(payout_email: Option<&str>, status: Option<&str>, tax_form_status: Option<&str>) -> Value {
    let mut body = json!({});
    if let Some(e) = payout_email {
        body["payoutEmail"] = json!(e);
    }
    if let Some(s) = status {
        body["status"] = json!(s);
    }
    if let Some(t) = tax_form_status {
        body["taxFormStatus"] = json!(t);
    }
    body
}

fn percent_to_bps(percent: f64) -> i64 {
    (percent * 100.0).round() as i64
}

fn referral_link(code: &str) -> String {
    format!("{REFERRAL_LINK_BASE}/{code}")
}

fn with_limit(path: &str, limit: Option<i64>) -> String {
    match limit {
        Some(n) => format!("{path}?limit={n}"),
        None => path.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_program(program: &Value) {
    let name = program.get("name").and_then(Value::as_str).unwrap_or("—");
    let status = program.get("status").and_then(Value::as_str).unwrap_or("—");
    let bps = program.get("commissionRateBps").and_then(Value::as_i64).unwrap_or(0);
    let duration = program
        .get("commissionDurationMonths")
        .and_then(Value::as_i64)
        .map(|m| format!("{m} months"))
        .unwrap_or_else(|| "lifetime".to_string());
    println!();
    println!("  {}  {}", name.bold(), status.cyan());
    println!("  {} {}% of MRR · {}", "Commission".dimmed(), bps as f64 / 100.0, duration);
    println!(
        "  {} {}d cookie · {}d lockout",
        "Windows   ".dimmed(),
        program.get("cookieDays").and_then(Value::as_i64).unwrap_or(0),
        program.get("lockoutDays").and_then(Value::as_i64).unwrap_or(0)
    );
    println!(
        "  {} {}",
        "Payout    ".dimmed(),
        program.get("payoutMethod").and_then(Value::as_str).unwrap_or("—")
    );
    if let Some(id) = program.get("id").and_then(Value::as_str) {
        println!("  {} {}", "id        ".dimmed(), id.dimmed());
    }
    println!();
}

fn render_partners(list: &Value) {
    let partners = list
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| list.as_array())
        .cloned()
        .unwrap_or_default();
    println!();
    println!("  {} {}", "Partners".bold(), format!("({})", partners.len()).dimmed());
    if partners.is_empty() {
        println!("\n  {}\n", "none — add one with `th referrals partners add`".dimmed());
        return;
    }
    println!();
    println!(
        "  {}  {}  {}  {}  {}",
        format!("{:<24}", "NAME").dimmed(),
        format!("{:<28}", "EMAIL").dimmed(),
        format!("{:<10}", "STATUS").dimmed(),
        format!("{:>9}", "EARNED").dimmed(),
        "LINK".dimmed(),
    );
    for p in &partners {
        let name = p.get("displayName").and_then(Value::as_str).unwrap_or("—");
        let email = p.get("email").and_then(Value::as_str).unwrap_or("—");
        let status = p.get("status").and_then(Value::as_str).unwrap_or("—");
        let code = p.get("partnerCode").and_then(Value::as_str).unwrap_or("?");
        let earned = p.get("totalEarnedCents").and_then(Value::as_i64).unwrap_or(0);
        println!(
            "  {:<24}  {:<28}  {}  {:>9}  {}",
            truncate(name, 24),
            truncate(email, 28),
            format!("{:<10}", status).cyan(),
            format!("${:.2}", earned as f64 / 100.0),
            referral_link(code).dimmed(),
        );
    }
    println!();
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Terse spec builder so the tests read as "these economics".
    fn spec<'a>(duration: Option<i64>, cookie: Option<i64>, lockout: Option<i64>, payout: &'a str) -> ProgramSpec<'a> {
        ProgramSpec {
            name: "Partner Program",
            slug: "partners",
            rate_percent: 20.0,
            duration_months: duration,
            cookie_days: cookie,
            lockout_days: lockout,
            payout_method: payout,
            status: "active",
        }
    }

    #[test]
    fn percent_converts_to_basis_points() {
        assert_eq!(percent_to_bps(20.0), 2000);
        assert_eq!(percent_to_bps(2.5), 250);
        assert_eq!(percent_to_bps(100.0), 10_000);
    }

    #[test]
    fn absent_duration_means_lifetime_not_the_twelve_month_default() {
        let body = build_program_body(&spec(None, None, None, "manual_csv"));
        assert!(body["commissionDurationMonths"].is_null());
        let body = build_program_body(&spec(Some(12), None, None, "manual_csv"));
        assert_eq!(body["commissionDurationMonths"], 12);
    }

    #[test]
    fn program_body_carries_economics() {
        let body = build_program_body(&spec(Some(12), Some(90), Some(45), "wise"));
        assert_eq!(body["name"], "Partner Program");
        assert_eq!(body["slug"], "partners");
        assert_eq!(body["commissionRateBps"], 2000);
        assert_eq!(body["cookieDays"], 90);
        assert_eq!(body["lockoutDays"], 45);
        assert_eq!(body["payoutMethod"], "wise");
        assert_eq!(body["status"], "active");
    }

    #[test]
    fn program_body_omits_unset_windows() {
        let body = build_program_body(&spec(Some(12), None, None, "manual_csv"));
        assert!(body.get("cookieDays").is_none());
        assert!(body.get("lockoutDays").is_none());
    }

    #[test]
    fn program_patch_only_sends_changed_fields() {
        let body = build_program_patch(None, Some(25.0), None, None);
        assert_eq!(body["commissionRateBps"], 2500);
        assert!(body.get("name").is_none());
        assert!(body.get("status").is_none());
        assert!(body.get("payoutMethod").is_none());
    }

    #[test]
    fn partner_body_defaults_payout_email_to_email() {
        let body = build_partner_body("Wes Leigh", "wes@example.com", None, None, "active");
        assert_eq!(body["payoutEmail"], "wes@example.com");
        assert_eq!(body["displayName"], "Wes Leigh");
        assert!(body.get("partnerCode").is_none());
    }

    #[test]
    fn partner_body_honors_explicit_payout_and_code() {
        let body = build_partner_body("Wes", "wes@x.com", Some("pay@x.com"), Some("WESLEIGH"), "invited");
        assert_eq!(body["payoutEmail"], "pay@x.com");
        assert_eq!(body["partnerCode"], "WESLEIGH");
        assert_eq!(body["status"], "invited");
    }

    #[test]
    fn partner_patch_only_sends_changed_fields() {
        let body = build_partner_patch(None, Some("paused"), None);
        assert_eq!(body["status"], "paused");
        assert!(body.get("payoutEmail").is_none());
        assert!(body.get("taxFormStatus").is_none());
    }

    #[test]
    fn partner_matches_on_email_name_or_code_case_insensitively() {
        let list = json!([
            { "id": "1", "email": "brett@x.com", "displayName": "Brett Dunkerly", "partnerCode": "BRETT7" },
            { "id": "2", "email": "wes@x.com", "displayName": "Wes Leigh", "partnerCode": "WES42" },
        ]);
        assert_eq!(match_partner(&list, "WES@X.COM").unwrap()["id"], "2");
        assert_eq!(match_partner(&list, "brett dunkerly").unwrap()["id"], "1");
        assert_eq!(match_partner(&list, "wes42").unwrap()["id"], "2");
        assert!(match_partner(&list, "nobody").is_none());
    }

    #[test]
    fn first_program_reads_bare_array_and_envelope() {
        let bare = json!([{ "id": "p1" }]);
        assert_eq!(first_program(&bare).unwrap()["id"], "p1");
        let enveloped = json!({ "data": [{ "id": "p2" }] });
        assert_eq!(first_program(&enveloped).unwrap()["id"], "p2");
        assert!(first_program(&json!([])).is_none());
    }

    /// The redirect handler lives on api.smoo.ai; smoo.ai/r/<code> 404s.
    #[test]
    fn referral_link_points_at_the_host_that_actually_serves_it() {
        assert_eq!(referral_link("ABC123"), "https://api.smoo.ai/r/ABC123");
    }

    #[test]
    fn limit_is_appended_only_when_set() {
        assert_eq!(with_limit("/x/visits", Some(50)), "/x/visits?limit=50");
        assert_eq!(with_limit("/x/visits", None), "/x/visits");
    }

    #[test]
    fn truncate_adds_ellipsis_past_the_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("aaaaaaaaaa", 5), "aaaa…");
    }
}

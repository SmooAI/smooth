//! `th smoo calling numbers available|attach|buy` — getting a number onto the
//! org (SMOODEV-4052), the CLI twin of the hosted MCP's
//! `phone_numbers_available` / `phone_number_attach` / `phone_number_purchase`
//! and the copilot's `phone_numbers.*`.
//!
//! Each verb wraps the route the dashboard's Phone Numbers page calls
//! (`/organizations/{org}/integrations/twilio/…`). Attaching and buying are
//! separate requests there: `intent: "attach"` never buys (a number the org's
//! Twilio account does not own is refused), `intent: "purchase"` refuses a
//! number the account already owns, needs `billing.manage` on top of
//! `calling.admin`, and must acknowledge Twilio's current monthly price —
//! without it the route answers 409 with the quote and buys nothing.
//!
//! The quote is for THE number: the route takes the country from the number
//! (a `--country` that disagrees is refused) and prices the type Twilio sells
//! it as (local, toll-free, mobile). `buy` therefore asks the route itself for
//! the quote — a purchase request with no acknowledgement, which buys nothing
//! and answers 409 with the quote — prints it, and refuses unless
//! `--accept-price` is at least that quote; then it asks for confirmation (a
//! TTY prompt, or `--yes` in scripts). `attach` asks too: it re-points the
//! number's calls and texts at Smoo. A 403 reads as "admin only".

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::{print_json, require_active_org, require_authed};
use crate::destructive::{Confirm, Severity, Target};

#[derive(Subcommand)]
pub enum ProvisionCmd {
    /// Numbers to attach (already owned, no charge) and to buy, with the monthly price.
    #[command(visible_alias = "search")]
    Available {
        /// Optional 3-digit area code, e.g. 415.
        #[arg(long = "area-code")]
        area_code: Option<String>,
        /// Two-letter ISO country code. Default US.
        #[arg(long)]
        country: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Attach a number the org's Twilio account ALREADY owns (needs calling admin). Never buys.
    Attach {
        /// E.164, e.g. +14155551234.
        phone_number: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
    /// BUY a new number from Twilio (needs calling admin + billing manage). Spends money every month.
    #[command(visible_alias = "purchase")]
    Buy {
        /// E.164, from `calling numbers available`.
        phone_number: String,
        /// The monthly price you agree to pay, e.g. 1.15. Required: run without it to see the quote.
        #[arg(long = "accept-price", value_name = "AMOUNT")]
        accept_price: Option<f64>,
        /// Two-letter ISO country of the number, as a cross-check. Taken from the number when omitted.
        #[arg(long)]
        country: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
}

/// E.164, the route's rule: `+`, 2-15 digits, first non-zero.
pub fn e164(raw: &str) -> Result<String> {
    let n = raw.trim();
    let digits = n.strip_prefix('+').unwrap_or("");
    if (2..=15).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit()) && !digits.starts_with('0') {
        Ok(n.to_string())
    } else {
        bail!("phone number `{n}` must be E.164, e.g. +14155551234")
    }
}

/// Two-letter ISO country for the search, upper-cased; `None` → US.
pub fn country(raw: Option<&str>) -> Result<String> {
    Ok(country_opt(raw)?.unwrap_or_else(|| "US".into()))
}

/// Optional two-letter ISO country, upper-cased, with no default — a purchase
/// never invents one (the route derives it from the number).
pub fn country_opt(raw: Option<&str>) -> Result<Option<String>> {
    match raw.map(str::trim).filter(|c| !c.is_empty()) {
        None => Ok(None),
        Some(c) if c.len() == 2 && c.chars().all(|ch| ch.is_ascii_alphabetic()) => Ok(Some(c.to_ascii_uppercase())),
        Some(c) => bail!("country `{c}` must be a two-letter ISO code, e.g. US"),
    }
}

/// The `available-numbers` query.
pub fn available_query(area: Option<&str>, country_code: Option<&str>) -> Result<String> {
    let mut q = format!("country={}", country(country_code)?);
    if let Some(a) = area.map(str::trim).filter(|a| !a.is_empty()) {
        if a.len() != 3 || !a.chars().all(|c| c.is_ascii_digit()) {
            bail!("area code `{a}` must be three digits, e.g. 415");
        }
        q.push_str(&format!("&areaCode={a}"));
    }
    Ok(q)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// "$1.15/month" or "1.00 EUR/month".
#[must_use]
pub fn price_label(price: &str, unit: &str) -> String {
    if unit.is_empty() || unit.eq_ignore_ascii_case("USD") {
        format!("${price}/month")
    } else {
        format!("{price} {unit}/month")
    }
}

/// The quote out of an `available-numbers` response: (price, unit), `None` when Twilio gave none.
#[must_use]
pub fn quote(available: &Value) -> Option<(f64, String, String)> {
    let raw = s(available, "monthlyPrice");
    let price = raw.parse::<f64>().ok().filter(|p| p.is_finite())?;
    Some((price, raw.to_string(), s(available, "priceUnit").to_string()))
}

/// The route's quote for one number: what buying it costs and why.
#[derive(Debug, PartialEq)]
pub struct NumberQuote {
    pub price: f64,
    pub raw: String,
    pub unit: String,
    /// `local`, `toll free` or `mobile`, as Twilio sells this number.
    pub number_type: String,
    pub country: String,
}

/// The quote out of the route's `409 price_acknowledgement_required` (the
/// client's error reads "POST … returned HTTP 409 Conflict: {json}"). `None`
/// for any other refusal.
#[must_use]
pub fn quote_from_refusal(err: &str) -> Option<NumberQuote> {
    let at = err.find("returned HTTP 409")?;
    let body: Value = serde_json::from_str(&err[at + err[at..].find('{')?..]).ok()?;
    if s(&body, "error") != "price_acknowledgement_required" {
        return None;
    }
    let q = body.get("quote")?;
    let raw = s(q, "monthlyPrice");
    let price = raw.parse::<f64>().ok().filter(|p| p.is_finite())?;
    Some(NumberQuote {
        price,
        raw: raw.to_string(),
        unit: s(q, "priceUnit").to_string(),
        number_type: s(q, "numberType").to_string(),
        country: s(q, "country").to_string(),
    })
}

/// Is the acknowledged price enough? Compared in cents, as the route does.
#[must_use]
pub fn price_ok(accepted: f64, quoted: f64) -> bool {
    (accepted * 100.0).round() >= (quoted * 100.0).round()
}

/// The `POST /phone-numbers` body for a purchase; `accepted: None` asks for the
/// quote (the route buys nothing without an acknowledgement).
#[must_use]
pub fn purchase_body(phone_number: &str, country_code: Option<&str>, accepted: Option<f64>) -> Value {
    let mut body = json!({ "phoneNumber": phone_number, "intent": "purchase" });
    if let Some(c) = country_code {
        body["country"] = json!(c);
    }
    if let Some(p) = accepted {
        body["acknowledgedMonthlyPrice"] = json!(p);
    }
    body
}

/// A 403 from the route reads as "admin only", not an outage.
fn admin_only(err: anyhow::Error, buying: bool) -> anyhow::Error {
    if err.to_string().contains("HTTP 403") {
        let need = if buying {
            "admin only: buying a number needs both calling admin (calling.admin) and billing management (billing.manage) — nothing was bought"
        } else {
            "admin only: attaching or searching numbers needs the calling admin permission (calling.admin) — nothing was changed"
        };
        return err.context(need);
    }
    err
}

fn render_available(owned: Option<&Value>, available: &Value) {
    println!();
    if let Some(rows) = owned.and_then(|o| o.get("ownedNumbers")).and_then(Value::as_array) {
        println!("  {}", "already owned — attach with `calling numbers attach <number>` (no charge)".bold());
        if rows.is_empty() {
            println!("  {} {}", "●".dimmed(), "none waiting".dimmed());
        }
        for r in rows {
            println!("  {} {} {}", "○".dimmed(), s(r, "phoneNumber").cyan(), s(r, "friendlyName").dimmed());
        }
        println!();
    }
    let rows = available.get("availableNumbers").and_then(Value::as_array).cloned().unwrap_or_default();
    match quote(available) {
        Some((_, raw, unit)) => println!(
            "  {} {}",
            "for sale — buy with `calling numbers buy <number> --accept-price".bold(),
            format!("{raw}` — costs {} until released", price_label(&raw, &unit)).bold()
        ),
        None => println!(
            "  {}",
            "for sale — Twilio gave no price quote right now, so these cannot be bought yet".yellow()
        ),
    }
    if rows.is_empty() {
        println!("  {} {}", "●".dimmed(), "no numbers matched".dimmed());
    }
    for r in &rows {
        println!(
            "  {} {} {} {}",
            "○".dimmed(),
            s(r, "phoneNumber").cyan(),
            s(r, "friendlyName").dimmed(),
            [s(r, "locality"), s(r, "region")]
                .iter()
                .filter(|p| !p.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
                .dimmed()
        );
    }
    println!();
}

pub async fn run(cmd: ProvisionCmd) -> Result<()> {
    match cmd {
        ProvisionCmd::Available {
            area_code,
            country: c,
            org,
            json,
        } => {
            let q = available_query(area_code.as_deref(), c.as_deref())?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let base = format!("/organizations/{o}/integrations/twilio");
            // The owned list is a convenience: failing there must not hide what is for sale.
            let owned = client.get(&format!("{base}/owned-numbers")).await.ok();
            let available = client
                .get(&format!("{base}/available-numbers?{q}"))
                .await
                .map_err(|e| admin_only(e, false))
                .context("GET integrations/twilio/available-numbers")?;
            if json {
                print_json(&json!({ "owned": owned, "available": available }));
            } else {
                render_available(owned.as_ref(), &available);
            }
        }
        ProvisionCmd::Attach { phone_number, org, confirm } => {
            let number = e164(&phone_number)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let body = json!({ "phoneNumber": number, "intent": "attach" });
            println!("\n  {} attaching never buys; the number's calls and texts move to Smoo", "●".dimmed());
            if confirm.dry_run {
                print_json(&body);
            }
            let proceed = crate::destructive::gate_with(
                &Target {
                    verb: "attach",
                    noun: "phone number",
                    id: &number,
                    org: &o,
                    severity: Severity::Standard,
                },
                confirm,
            )?;
            if proceed {
                let out = client
                    .post(&format!("/organizations/{o}/integrations/twilio/phone-numbers"), Some(&body))
                    .await
                    .map_err(|e| admin_only(e, false))
                    .context("POST integrations/twilio/phone-numbers (attach)")?;
                println!("\n  {} attached {}", "✓".green(), number.cyan());
                print_json(&out);
            }
        }
        ProvisionCmd::Buy {
            phone_number,
            accept_price,
            country: c,
            org,
            confirm,
        } => {
            let number = e164(&phone_number)?;
            let cc = country_opt(c.as_deref())?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let base = format!("/organizations/{o}/integrations/twilio");
            // The quote for THIS number, from the route that will enforce it: a purchase
            // request with no acknowledgement buys nothing and answers 409 with the quote.
            let probe = client
                .post(&format!("{base}/phone-numbers"), Some(&purchase_body(&number, cc.as_deref(), None)))
                .await;
            let q = match probe {
                Ok(_) => bail!("the route accepted a purchase with no price acknowledgement — refusing to go on; report this"),
                Err(e) => match quote_from_refusal(&e.to_string()) {
                    Some(q) => q,
                    None => return Err(admin_only(e, true)).context("POST integrations/twilio/phone-numbers (price quote)"),
                },
            };
            let label = price_label(&q.raw, &q.unit);
            let kind = if q.number_type == "toll free" { "toll-free" } else { q.number_type.as_str() };
            println!(
                "\n  {} buying {} (a {} {} number) costs {} from Twilio, every month until released",
                "$".yellow().bold(),
                number.cyan(),
                q.country,
                kind,
                label.yellow().bold()
            );
            let Some(accepted) = accept_price else {
                bail!("nothing was bought: re-run with `--accept-price {}` to agree to {label}", q.raw);
            };
            if !accepted.is_finite() || accepted < 0.0 || !price_ok(accepted, q.price) {
                bail!(
                    "nothing was bought: --accept-price {accepted} is below Twilio's current {label}; re-run with `--accept-price {}`",
                    q.raw
                );
            }
            let body = purchase_body(&number, cc.as_deref(), Some(accepted));
            if confirm.dry_run {
                print_json(&body);
            }
            let noun = format!("phone number for {label}");
            let proceed = crate::destructive::gate_with(
                &Target {
                    verb: "buy",
                    noun: &noun,
                    id: &number,
                    org: &o,
                    severity: Severity::Standard,
                },
                confirm,
            )?;
            if proceed {
                let out = client
                    .post(&format!("{base}/phone-numbers"), Some(&body))
                    .await
                    .map_err(|e| admin_only(e, true))
                    .context("POST integrations/twilio/phone-numbers (purchase)")?;
                println!("\n  {} bought and attached {} — {}", "✓".green(), number.cyan(), label);
                print_json(&out);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_mirrors_the_route() {
        assert_eq!(e164(" +14155550100 ").unwrap(), "+14155550100");
        for bad in ["4155550100", "+0123", "+1", "+1-415"] {
            assert!(e164(bad).is_err(), "{bad}");
        }
        assert_eq!(country(None).unwrap(), "US");
        assert_eq!(country(Some("gb")).unwrap(), "GB");
        assert!(country(Some("USA")).is_err());
        assert_eq!(available_query(Some("415"), None).unwrap(), "country=US&areaCode=415");
        assert!(available_query(Some("41"), None).is_err());
    }

    #[test]
    fn the_price_must_be_acknowledged_at_or_above_the_quote() {
        let q = quote(&json!({ "monthlyPrice": "1.15", "priceUnit": "USD" })).unwrap();
        assert!((q.0 - 1.15).abs() < f64::EPSILON);
        assert!(price_ok(1.15, q.0));
        assert!(price_ok(2.0, q.0));
        assert!(!price_ok(1.14, q.0));
        assert!(quote(&json!({ "monthlyPrice": null })).is_none());
        assert_eq!(price_label("1.15", "USD"), "$1.15/month");
        assert_eq!(price_label("1.00", "EUR"), "1.00 EUR/month");
    }

    #[test]
    fn purchase_body_states_the_intent_and_price_and_never_invents_a_country() {
        assert_eq!(
            purchase_body("+14155550100", Some("US"), Some(1.15)),
            json!({ "phoneNumber": "+14155550100", "intent": "purchase", "country": "US", "acknowledgedMonthlyPrice": 1.15 })
        );
        // The quote request: no acknowledgement, so the route buys nothing.
        assert_eq!(
            purchase_body("+18335550100", None, None),
            json!({ "phoneNumber": "+18335550100", "intent": "purchase" })
        );
        assert_eq!(country_opt(None).unwrap(), None);
        assert_eq!(country_opt(Some("au")).unwrap().as_deref(), Some("AU"));
        assert!(country_opt(Some("AUS")).is_err());
    }

    #[test]
    fn the_quote_comes_from_the_routes_refusal_for_this_number() {
        let err = r#"POST /organizations/o/integrations/twilio/phone-numbers returned HTTP 409 Conflict: {"error":"price_acknowledgement_required","message":"Buying +18335550100 (a US toll-free number) costs $2.15/month","quote":{"country":"US","numberType":"toll free","monthlyPrice":"2.15","priceUnit":"USD"}}"#;
        let q = quote_from_refusal(err).unwrap();
        assert_eq!(q.raw, "2.15");
        assert_eq!(q.number_type, "toll free");
        assert_eq!(q.country, "US");
        assert!(!price_ok(1.15, q.price));
        // Any other 409 (not for sale, already owned) is not a quote.
        assert!(quote_from_refusal("POST /x returned HTTP 409 Conflict: +1 is not for sale").is_none());
        assert!(quote_from_refusal("POST /x returned HTTP 403 Forbidden: {}").is_none());
    }

    #[test]
    fn a_403_reads_as_admin_only() {
        let e = admin_only(anyhow::anyhow!("POST /x returned HTTP 403 Forbidden: {{}}"), true);
        assert!(format!("{e:#}").contains("billing.manage"));
        let e = admin_only(anyhow::anyhow!("POST /x returned HTTP 403 Forbidden: {{}}"), false);
        assert!(format!("{e:#}").contains("calling.admin"));
        let e = admin_only(anyhow::anyhow!("POST /x returned HTTP 409 Conflict: not owned"), false);
        assert!(!format!("{e:#}").contains("admin only"));
    }
}

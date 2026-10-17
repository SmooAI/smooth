//! `smoo credits …` — prepaid AI credits for the active org (smooai
//! SMOODEV-3966 / ADR-155; this verb is SMOODEV-3969).
//!
//! A credit pack raises the org's LLM gateway ceiling on top of its monthly
//! allowance; auto-reload buys one off-session when the balance drops below a
//! threshold. Three existing routes on the LLM gateway app back it:
//!
//! - `GET  /organizations/{org}/llm-gateway/credits` — balance, auto-reload, eligibility (`billing.read`)
//! - `POST /organizations/{org}/llm-gateway/credits/checkout` `{ packCents }` → `{ url }` (`billing.manage`)
//! - `PUT  /organizations/{org}/llm-gateway/credits/auto-reload` `{ enabled, thresholdCents, packCents }` (`billing.manage`)
//!
//! They authenticate as the **user** (Supabase JWT), so this uses [`UserClient`].
//! `buy` only opens a Stripe Checkout: nothing is charged until the user pays
//! there. Turning auto-reload ON authorizes future off-session charges, so it
//! previews unless `--confirm` (CLI-Spec §3).

use anstream::{eprintln, println};
use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};
use smooth_cast::ai_credits::{self, dollars, PACKS_CENTS};

use super::print_json;
use crate::smooai::user_client::UserClient;

/// "When my balance drops below $X" — smooai `AI_CREDIT_AUTO_RELOAD_THRESHOLDS_CENTS`.
pub const THRESHOLDS_CENTS: [u32; 3] = [100, 200, 500];

#[derive(Subcommand)]
pub enum Cmd {
    /// Show the org's AI credit balance, auto-reload and eligibility.
    ///
    /// The balance is credit left after this month's included allowance is
    /// used up. Needs `billing.read` in the org.
    #[command(visible_alias = "balance")]
    Show {
        /// Override the active org (`smoo org switch`).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw response JSON.
        #[arg(long)]
        json: bool,
    },
    /// Buy a credit pack — opens a Stripe Checkout for it.
    ///
    /// `smoo credits buy 5` or `smoo credits buy 10`. Nothing is charged
    /// until you pay on the Checkout page; the credit lands within seconds of
    /// paying. Needs `billing.manage` in the org (admins have it).
    #[command(visible_alias = "top-up")]
    Buy {
        /// The pack, in dollars: 5 or 10.
        #[arg(value_name = "DOLLARS")]
        pack: String,
        /// Override the active org (`smoo org switch`).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Print the Checkout link without opening a browser.
        #[arg(long)]
        no_open: bool,
        /// Emit the raw response JSON (`{ url }`).
        #[arg(long)]
        json: bool,
    },
    /// Auto-reload: buy a pack automatically when the balance runs low.
    ///
    /// Bare `smoo credits auto-reload` shows the current setting.
    #[command(name = "auto-reload", visible_alias = "autoreload")]
    AutoReload {
        #[command(subcommand)]
        cmd: Option<AutoReloadCmd>,
    },
}

#[derive(Subcommand)]
pub enum AutoReloadCmd {
    /// Show whether auto-reload is on, and its threshold and pack.
    Status {
        /// Override the active org (`smoo org switch`).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw auto-reload JSON.
        #[arg(long)]
        json: bool,
    },
    /// Turn auto-reload on — charges the saved card off-session.
    ///
    /// Previews unless `--confirm`. Unset flags keep the current values
    /// (default: below $1, add $10). Needs a card on file and `billing.manage`.
    On {
        /// Reload when the balance drops below this many dollars: 1, 2 or 5.
        #[arg(long, value_name = "DOLLARS")]
        below: Option<String>,
        /// The pack to buy each time, in dollars: 5 or 10.
        #[arg(long, value_name = "DOLLARS")]
        add: Option<String>,
        /// Actually turn it on. Without it, prints what would change.
        #[arg(long)]
        confirm: bool,
        /// Override the active org (`smoo org switch`).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw response JSON.
        #[arg(long)]
        json: bool,
    },
    /// Turn auto-reload off. Nothing more is charged automatically.
    Off {
        /// Override the active org (`smoo org switch`).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw response JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Bare `smoo credits` is the balance.
#[must_use]
pub const fn default_cmd() -> Cmd {
    Cmd::Show { org_id: None, json: false }
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        Cmd::Show { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = get_credits(&client, &org).await?;
            if json {
                print_json(&resp);
            } else {
                print_show(&resp, &org);
            }
        }
        Cmd::Buy { pack, org_id, no_open, json } => {
            let pack_cents = parse_pack(&pack)?;
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(
                    &format!("/organizations/{org}/llm-gateway/credits/checkout"),
                    &json!({ "packCents": pack_cents }),
                )
                .await
                .map_err(explain)?;
            if json {
                print_json(&resp);
                return Ok(());
            }
            let url = resp
                .get("url")
                .and_then(Value::as_str)
                .filter(|u| u.starts_with("https://"))
                .ok_or_else(|| anyhow!("the checkout response had no link: {resp}"))?;
            println!();
            println!(
                "  {} {} AI credit pack — pay on the Stripe Checkout page:",
                "●".bold(),
                dollars(pack_cents).bold()
            );
            println!("    {}", url.cyan());
            if !no_open {
                if let Err(e) = open::that(url) {
                    eprintln!("    (couldn't open a browser: {e} — open the link above)");
                }
            }
            println!(
                "  {} the credit lands within seconds of paying; check with {}",
                "○".dimmed(),
                "smoo credits".bold()
            );
            println!();
        }
        Cmd::AutoReload { cmd } => auto_reload(cmd.unwrap_or(AutoReloadCmd::Status { org_id: None, json: false }), &client).await?,
    }
    Ok(())
}

async fn get_credits(client: &UserClient, org: &str) -> Result<Value> {
    client.get(&format!("/organizations/{org}/llm-gateway/credits")).await.map_err(explain)
}

async fn auto_reload(cmd: AutoReloadCmd, client: &UserClient) -> Result<()> {
    match cmd {
        AutoReloadCmd::Status { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = get_credits(client, &org).await?;
            let settings = resp.get("autoReload").cloned().unwrap_or(Value::Null);
            if json {
                print_json(&settings);
            } else {
                println!();
                println!("  {}", auto_reload_line(&settings));
                if let Some(why) = disabled_reason(&settings) {
                    println!("    {} it switched itself off: {why}", "◐".yellow());
                }
                println!();
            }
        }
        AutoReloadCmd::On {
            below,
            add,
            confirm,
            org_id,
            json,
        } => {
            let org = crate::active_org::resolve(org_id)?;
            let current = get_credits(client, &org).await?;
            let body = on_body(current.get("autoReload").unwrap_or(&Value::Null), below.as_deref(), add.as_deref())?;
            let summary = reload_summary(&body);
            if !confirm {
                println!();
                println!("  {} would turn auto-reload ON for org {org}: {summary}", "○".dimmed());
                println!(
                    "    the saved card is charged off-session each time — rerun with {} to turn it on",
                    "--confirm".bold()
                );
                println!();
                return Ok(());
            }
            let resp = put_auto_reload(client, &org, &body).await?;
            if json {
                print_json(&resp);
            } else {
                println!();
                println!("  {} auto-reload on: {summary}", "●".green().bold());
                println!();
            }
        }
        AutoReloadCmd::Off { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let current = get_credits(client, &org).await?;
            let body = off_body(current.get("autoReload").unwrap_or(&Value::Null));
            let resp = put_auto_reload(client, &org, &body).await?;
            if json {
                print_json(&resp);
            } else {
                println!();
                println!("  {} auto-reload off — nothing more is charged automatically", "○".bold());
                println!();
            }
        }
    }
    Ok(())
}

async fn put_auto_reload(client: &UserClient, org: &str, body: &Value) -> Result<Value> {
    client
        .put(&format!("/organizations/{org}/llm-gateway/credits/auto-reload"), body)
        .await
        .map_err(explain)
}

// ── pure helpers ─────────────────────────────────────────────────────────────

/// "5", "$5", "5.00" → 500 cents. Whole cents only.
fn parse_dollars(raw: &str) -> Option<u32> {
    let s = raw.trim().trim_start_matches('$');
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) || frac.len() > 2 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let cents = format!("{frac:0<2}").parse::<u32>().ok()?;
    whole.parse::<u32>().ok()?.checked_mul(100)?.checked_add(cents)
}

fn choices(cents: &[u32]) -> String {
    cents.iter().map(|c| dollars(*c)).collect::<Vec<_>>().join(", ")
}

/// A pack the server sells, in cents.
///
/// # Errors
/// When `raw` is not one of [`PACKS_CENTS`].
pub fn parse_pack(raw: &str) -> Result<u32> {
    match parse_dollars(raw) {
        Some(c) if PACKS_CENTS.contains(&c) => Ok(c),
        _ => bail!("`{raw}` is not an AI credit pack\n  → choose one of {}", choices(&PACKS_CENTS)),
    }
}

/// An auto-reload threshold the server accepts, in cents.
///
/// # Errors
/// When `raw` is not one of [`THRESHOLDS_CENTS`].
pub fn parse_threshold(raw: &str) -> Result<u32> {
    match parse_dollars(raw) {
        Some(c) if THRESHOLDS_CENTS.contains(&c) => Ok(c),
        _ => bail!("`{raw}` is not an auto-reload threshold\n  → choose one of {}", choices(&THRESHOLDS_CENTS)),
    }
}

fn cents_field(settings: &Value, key: &str) -> Option<u32> {
    settings.get(key).and_then(Value::as_u64).and_then(|c| u32::try_from(c).ok())
}

/// The PUT body for `on`: flags win, else the org's current values, else the server's defaults.
///
/// # Errors
/// When a flag is not an accepted amount.
pub fn on_body(current: &Value, below: Option<&str>, add: Option<&str>) -> Result<Value> {
    let threshold = match below {
        Some(raw) => parse_threshold(raw)?,
        None => cents_field(current, "thresholdCents").unwrap_or(THRESHOLDS_CENTS[0]),
    };
    let pack = match add {
        Some(raw) => parse_pack(raw)?,
        None => cents_field(current, "packCents").unwrap_or(PACKS_CENTS[PACKS_CENTS.len() - 1]),
    };
    Ok(json!({ "enabled": true, "thresholdCents": threshold, "packCents": pack }))
}

/// The PUT body for `off`: the route needs threshold + pack, so the current ones ride along unchanged.
#[must_use]
pub fn off_body(current: &Value) -> Value {
    json!({
        "enabled": false,
        "thresholdCents": cents_field(current, "thresholdCents").unwrap_or(THRESHOLDS_CENTS[0]),
        "packCents": cents_field(current, "packCents").unwrap_or(PACKS_CENTS[PACKS_CENTS.len() - 1]),
    })
}

/// "when the balance drops below $1, add $10".
fn reload_summary(settings: &Value) -> String {
    let below = cents_field(settings, "thresholdCents").map_or_else(|| "?".into(), dollars);
    let add = cents_field(settings, "packCents").map_or_else(|| "?".into(), dollars);
    format!("when the balance drops below {below}, add {add}")
}

fn disabled_reason(settings: &Value) -> Option<&str> {
    settings.get("disabledReason").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

/// One line for the auto-reload setting; the glyph carries the state without color.
#[must_use]
pub fn auto_reload_line(settings: &Value) -> String {
    if settings.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
        format!("● auto-reload on — {}", reload_summary(settings))
    } else {
        "○ auto-reload off — turn it on with `smoo credits auto-reload on --confirm`".to_string()
    }
}

/// `$4.20` from a USD float.
fn usd(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_f64).map(|d| format!("${d:.2}"))
}

/// The human view of `GET …/credits`, as lines (no color, so tests read it).
#[must_use]
pub fn show_lines(resp: &Value, org: &str) -> Vec<String> {
    let mut out = vec![format!("AI credits · org {org}")];
    if resp.get("acceptsCredits").and_then(Value::as_bool) == Some(false) {
        out.push("○ this org does not use AI credits".into());
        return out;
    }
    let balance = usd(resp, "balanceUsd").unwrap_or_else(|| "$0.00".into());
    out.push(format!("● balance {balance}"));
    let used = usd(resp, "consumedThisMonthUsd").unwrap_or_else(|| "$0.00".into());
    match usd(resp, "allowanceUsd") {
        Some(allowance) => out.push(format!("  {used} of credit used this month, after the {allowance} included allowance")),
        None => out.push(format!("  {used} of credit used this month")),
    }
    let settings = resp.get("autoReload").cloned().unwrap_or(Value::Null);
    out.push(auto_reload_line(&settings));
    if let Some(why) = disabled_reason(&settings) {
        out.push(format!("◐ auto-reload switched itself off: {why}"));
    }
    let eligibility = resp.get("eligibility").cloned().unwrap_or(Value::Null);
    if eligibility.get("eligible").and_then(Value::as_bool) == Some(false) {
        let why = eligibility
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("this org can't buy AI credits right now");
        out.push(format!("✗ {why}"));
    } else {
        out.push(format!("→ {}: `smoo credits buy {}`", ai_credits::top_up_line(""), pack_choices_arg()));
    }
    out
}

/// "5|10" — the `buy` argument choices.
fn pack_choices_arg() -> String {
    PACKS_CENTS
        .iter()
        .map(|c| dollars(*c).trim_start_matches('$').to_string())
        .collect::<Vec<_>>()
        .join("|")
}

fn print_show(resp: &Value, org: &str) {
    println!();
    for (i, line) in show_lines(resp, org).iter().enumerate() {
        if i == 0 {
            println!("  {}", line.bold());
        } else {
            println!("  {line}");
        }
    }
    println!();
}

/// The server's own message out of an `HTTP <status>: <body>` error, when it sent one.
fn server_message(text: &str) -> Option<String> {
    let body = &text[text.find('{')?..];
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("message")
        .or_else(|| v.pointer("/error/message"))
        .or_else(|| v.get("error"))
        .and_then(Value::as_str)
        .filter(|m| !m.trim().is_empty())
        .map(str::to_string)
}

/// CLI-Spec §4 errors: what failed (the server's words), then what to do.
#[must_use]
pub fn explain(e: anyhow::Error) -> anyhow::Error {
    let text = e.to_string();
    let said = server_message(&text);
    let next = if text.contains("HTTP 403") {
        "buying credit or changing auto-reload needs `billing.manage` in this org (reading the balance needs `billing.read`) — ask an org admin, or check `--org`"
    } else if text.contains("HTTP 409") {
        "fix the billing issue above on the dashboard, then retry"
    } else if text.contains("HTTP 429") {
        "that's the daily or monthly pack limit — try again later"
    } else if text.contains("HTTP 503") {
        "AI credit packs aren't on sale yet — try again later"
    } else {
        return e;
    };
    let first = said.unwrap_or_else(|| text.clone());
    anyhow!("{first}\n  → {next}: {}", ai_credits::TOP_UP_URL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dollar_spellings() {
        assert_eq!(parse_dollars("5"), Some(500));
        assert_eq!(parse_dollars("$10"), Some(1000));
        assert_eq!(parse_dollars("5.00"), Some(500));
        assert_eq!(parse_dollars("2.5"), Some(250));
        assert_eq!(parse_dollars(" 1 "), Some(100));
        for bad in ["", "$", "five", "5.001", "-5", ".5", "1e3", "5.x"] {
            assert!(parse_dollars(bad).is_none(), "{bad}");
        }
        assert!(parse_dollars("99999999999").is_none(), "overflow is refused, not wrapped");
    }

    #[test]
    fn packs_are_only_what_the_server_sells() {
        assert_eq!(parse_pack("5").unwrap(), 500);
        assert_eq!(parse_pack("$10").unwrap(), 1000);
        let err = parse_pack("3").unwrap_err().to_string();
        assert!(err.contains("not an AI credit pack"), "{err}");
        assert!(err.contains("$5, $10"), "the error names the choices: {err}");
    }

    #[test]
    fn thresholds_are_only_what_the_server_accepts() {
        assert_eq!(parse_threshold("1").unwrap(), 100);
        assert_eq!(parse_threshold("$5").unwrap(), 500);
        assert!(parse_threshold("3").is_err());
    }

    #[test]
    fn on_body_keeps_current_values_for_unset_flags() {
        let current = json!({ "enabled": false, "thresholdCents": 200, "packCents": 500 });
        assert_eq!(
            on_body(&current, None, None).unwrap(),
            json!({ "enabled": true, "thresholdCents": 200, "packCents": 500 })
        );
        assert_eq!(
            on_body(&current, Some("5"), Some("10")).unwrap(),
            json!({ "enabled": true, "thresholdCents": 500, "packCents": 1000 })
        );
        assert!(on_body(&current, Some("4"), None).is_err());
        assert!(on_body(&current, None, Some("3")).is_err());
    }

    #[test]
    fn on_body_defaults_match_the_server_defaults() {
        // smooai AI_CREDIT_AUTO_RELOAD_DEFAULTS: below $1, add $10.
        assert_eq!(
            on_body(&Value::Null, None, None).unwrap(),
            json!({ "enabled": true, "thresholdCents": 100, "packCents": 1000 })
        );
    }

    #[test]
    fn off_body_carries_the_current_values() {
        let current = json!({ "enabled": true, "thresholdCents": 500, "packCents": 500 });
        assert_eq!(off_body(&current), json!({ "enabled": false, "thresholdCents": 500, "packCents": 500 }));
        assert_eq!(off_body(&Value::Null)["enabled"], false);
    }

    #[test]
    fn auto_reload_line_reads_without_color() {
        let on = json!({ "enabled": true, "thresholdCents": 100, "packCents": 1000 });
        assert_eq!(auto_reload_line(&on), "● auto-reload on — when the balance drops below $1, add $10");
        assert!(auto_reload_line(&json!({ "enabled": false })).starts_with("○ auto-reload off"));
    }

    fn credits(eligible: bool) -> Value {
        json!({
            "balanceUsd": 4.2,
            "consumedThisMonthUsd": 0.8,
            "allowanceUsd": 2.3,
            "acceptsCredits": true,
            "autoReload": { "enabled": false, "thresholdCents": 100, "packCents": 1000, "disabledReason": "card_declined", "disabledAt": null },
            "eligibility": if eligible {
                json!({ "eligible": true, "reason": null, "message": null })
            } else {
                json!({ "eligible": false, "reason": "no_subscription", "message": "AI credits need an active Smoo AI subscription with a card on file." })
            },
            "packsCents": [500, 1000],
            "recentCharges": [],
        })
    }

    #[test]
    fn show_renders_balance_usage_reload_and_next_step() {
        let lines = show_lines(&credits(true), "org-1");
        assert_eq!(lines[0], "AI credits · org org-1");
        assert_eq!(lines[1], "● balance $4.20");
        assert!(lines[2].contains("$0.80 of credit used this month") && lines[2].contains("$2.30"), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("○ auto-reload off")), "{lines:?}");
        assert!(lines.iter().any(|l| l == "◐ auto-reload switched itself off: card_declined"), "{lines:?}");
        let last = lines.last().unwrap();
        assert!(last.contains("Top up $5 or $10 anytime") && last.contains("smoo credits buy 5|10"), "{last}");
    }

    #[test]
    fn show_says_why_an_ineligible_org_cannot_buy() {
        let lines = show_lines(&credits(false), "org-1");
        assert_eq!(lines.last().unwrap(), "✗ AI credits need an active Smoo AI subscription with a card on file.");
    }

    #[test]
    fn show_for_an_org_without_credits_is_an_answer() {
        let lines = show_lines(&json!({ "acceptsCredits": false }), "org-1");
        assert_eq!(
            lines,
            vec!["AI credits · org org-1".to_string(), "○ this org does not use AI credits".to_string()]
        );
    }

    #[test]
    fn explain_gives_two_lines_with_the_server_message() {
        let e = explain(anyhow!(
            r#"PUT https://api.smoo.ai/x returned HTTP 409 Conflict: {"message":"Add a card to your billing details to turn on auto-reload."}"#
        ))
        .to_string();
        let mut lines = e.lines();
        assert_eq!(lines.next(), Some("Add a card to your billing details to turn on auto-reload."));
        let next = lines.next().unwrap();
        assert!(next.starts_with("  → ") && next.contains(ai_credits::TOP_UP_URL), "{next}");

        let e = explain(anyhow!("GET https://api.smoo.ai/x returned HTTP 403 Forbidden: {{}}")).to_string();
        assert!(e.contains("billing.manage"), "{e}");
    }

    #[test]
    fn explain_passes_other_errors_through() {
        let e = explain(anyhow!("GET https://api.smoo.ai/x returned 401 — run `th auth login`")).to_string();
        assert_eq!(e, "GET https://api.smoo.ai/x returned 401 — run `th auth login`");
    }

    #[test]
    fn pack_choices_arg_matches_the_packs() {
        assert_eq!(pack_choices_arg(), "5|10");
    }
}

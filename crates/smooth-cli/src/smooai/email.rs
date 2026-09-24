//! `smoo email …` — email deliverability and signatures for the active org
//! (SMOODEV-3272).
//!
//! Every read calls the same api-prime / backend route the web app's Email
//! Diagnostics and Workforce → Signatures screens call, as the signed-in user.
//! No logic is re-implemented here: the checks, the DMARC aggregation and the
//! sender identification all live server-side.
//!
//! The `render_*` functions are pure and return plain text so the CLI and the
//! `th mcp serve` tools (`email_check_domain`, …) print exactly the same
//! thing. They share one rule with the observability tools: an EMPTY answer is
//! said in words and never reads like an all-clear — "no DMARC reports
//! received" is a setup problem, not a clean bill of health.

use std::fmt::Write as _;

use anstream::println;
use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::Value;
use smooth_api_client::SmoothApiClient;

use super::{print_json, require_active_org, require_authed};

/// Default reporting window, matching the web screens.
pub const DEFAULT_DAYS: u32 = 30;

#[derive(Subcommand)]
pub enum Cmd {
    /// Grade a domain's MX, SPF, DKIM, DMARC, blocklist, MTA-STS and BIMI
    /// records — each with what to change. Works on any domain.
    Check {
        /// The domain to check, e.g. `smoo.ai`.
        domain: String,
        /// Print raw JSON instead of the report.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// DMARC pass rate and per-source results from the aggregate reports
    /// mailbox providers sent about your domain.
    Dmarc {
        /// Limit to one monitored domain. All of the org's domains if omitted.
        #[arg(long)]
        domain: Option<String>,
        /// Reporting window in days.
        #[arg(long, default_value_t = DEFAULT_DAYS)]
        days: u32,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Every service sending mail as your domain — named, counted, and
    /// flagged when unidentified, failing, new, or a forwarder.
    Sources {
        /// Limit to one monitored domain. All of the org's domains if omitted.
        #[arg(long)]
        domain: Option<String>,
        /// Reporting window in days.
        #[arg(long, default_value_t = DEFAULT_DAYS)]
        days: u32,
        /// How many sources to list (the counts cover all of them).
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Print raw JSON instead of the listing.
        #[arg(long)]
        json: bool,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// SMTP TLS reports (RFC 8460): could mail addressed to you be delivered
    /// over trusted TLS, and if not, why.
    Tls {
        /// Limit to one monitored domain. All of the org's domains if omitted.
        #[arg(long)]
        domain: Option<String>,
        /// Reporting window in days.
        #[arg(long, default_value_t = DEFAULT_DAYS)]
        days: u32,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Managed email signatures: which domains are signing, and who is signed.
    Signatures {
        /// Print raw JSON instead of the listing.
        #[arg(long)]
        json: bool,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    let (body, text) = match cmd {
        Cmd::Check { domain, json, org } => {
            let o = require_active_org(&client, org)?;
            let body = check_domain(&client, &o, &domain).await?;
            (json.then(|| body.clone()), render_check(&body))
        }
        Cmd::Dmarc { domain, days, json, org } => {
            let o = require_active_org(&client, org)?;
            let body = dmarc_summary(&client, &o, domain.as_deref(), days).await?;
            (json.then(|| body.clone()), render_dmarc(&body))
        }
        Cmd::Sources {
            domain,
            days,
            limit,
            json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let body = sending_sources(&client, &o, domain.as_deref(), days).await?;
            (json.then(|| body.clone()), render_sources(&body, limit))
        }
        Cmd::Tls { domain, days, json, org } => {
            let o = require_active_org(&client, org)?;
            let body = tls_summary(&client, &o, domain.as_deref(), days).await?;
            (json.then(|| body.clone()), render_tls(&body))
        }
        Cmd::Signatures { json, org } => {
            let o = require_active_org(&client, org)?;
            let body = signature_status(&client, &o).await?;
            (json.then(|| body.clone()), render_signatures(&body))
        }
    };
    match body {
        Some(b) => print_json(&b),
        None => println!("{text}"),
    }
    Ok(())
}

// ── HTTP ────────────────────────────────────────────────────────────────────

/// `?k=v&…` with `None` values skipped and every value URL-encoded.
fn qs(pairs: &[(&str, Option<String>)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={}", urlencoding::encode(v))))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// The `domain` + `days` query the three report routes share.
fn window_query(domain: Option<&str>, days: u32) -> String {
    qs(&[("domain", domain.map(str::to_string)), ("days", Some(days.to_string()))])
}

/// `GET /organizations/:org/email-diagnostics/checks?domain=`
///
/// # Errors
/// Non-2xx from the route.
pub async fn check_domain(client: &SmoothApiClient, org: &str, domain: &str) -> Result<Value> {
    let query = qs(&[("domain", Some(domain.trim().to_string()))]);
    client
        .get(&format!("/organizations/{org}/email-diagnostics/checks{query}"))
        .await
        .context("GET email-diagnostics/checks")
}

/// `GET /organizations/:org/dmarc/reports/summary?domain=&days=`
///
/// # Errors
/// Non-2xx from the route.
pub async fn dmarc_summary(client: &SmoothApiClient, org: &str, domain: Option<&str>, days: u32) -> Result<Value> {
    client
        .get(&format!("/organizations/{org}/dmarc/reports/summary{}", window_query(domain, days)))
        .await
        .context("GET dmarc/reports/summary")
}

/// `GET /organizations/:org/dmarc/sources?domain=&days=`
///
/// # Errors
/// Non-2xx from the route.
pub async fn sending_sources(client: &SmoothApiClient, org: &str, domain: Option<&str>, days: u32) -> Result<Value> {
    client
        .get(&format!("/organizations/{org}/dmarc/sources{}", window_query(domain, days)))
        .await
        .context("GET dmarc/sources")
}

/// `GET /organizations/:org/tlsrpt/summary?domain=&days=`
///
/// # Errors
/// Non-2xx from the route.
pub async fn tls_summary(client: &SmoothApiClient, org: &str, domain: Option<&str>, days: u32) -> Result<Value> {
    client
        .get(&format!("/organizations/{org}/tlsrpt/summary{}", window_query(domain, days)))
        .await
        .context("GET tlsrpt/summary")
}

/// Every signing domain plus who is signed on each, merged into
/// `{ configs: [{ …config, renders: [{ senderEmail, renderedAt }] }] }`.
///
/// The renders route returns the full signature HTML per sender; that is
/// dropped here — it is the signature itself, not status, and it would flood
/// a model's context.
///
/// # Errors
/// Non-2xx from either route.
pub async fn signature_status(client: &SmoothApiClient, org: &str) -> Result<Value> {
    let list = client
        .get(&format!("/organizations/{org}/workforce/email-signatures"))
        .await
        .context("GET workforce/email-signatures")?;
    let mut configs = Vec::new();
    for cfg in rows(&list, "data") {
        let mut cfg = cfg.clone();
        if let Some(id) = cfg.get("id").and_then(Value::as_str) {
            let renders = client
                .get(&format!("/organizations/{org}/workforce/email-signatures/{}/renders", urlencoding::encode(id)))
                .await
                .context("GET workforce/email-signatures/:id/renders")?;
            let slim: Vec<Value> = rows(&renders, "data")
                .iter()
                .map(|r| serde_json::json!({ "senderEmail": r.get("senderEmail"), "renderedAt": r.get("renderedAt") }))
                .collect();
            cfg["renders"] = Value::Array(slim);
        }
        configs.push(cfg);
    }
    Ok(serde_json::json!({ "configs": configs }))
}

// ── Rendering (pure) ────────────────────────────────────────────────────────

/// Rows under `key`, accepting the enveloped shape or a bare array.
fn rows<'a>(body: &'a Value, key: &str) -> &'a [Value] {
    body.as_array().or_else(|| body.get(key).and_then(Value::as_array)).map_or(&[], Vec::as_slice)
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn n(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn status_mark(status: &str) -> &'static str {
    match status {
        "pass" => "PASS",
        "warn" => "WARN",
        "fail" => "FAIL",
        "info" => "INFO",
        _ => "????",
    }
}

fn percent(fraction: f64) -> String {
    format!("{:.1}%", fraction * 100.0)
}

fn scope(body: &Value) -> String {
    let domain = body.get("domain").and_then(Value::as_str).unwrap_or("all monitored domains");
    format!("{domain}, last {} days", n(body, "windowDays"))
}

/// The graded domain report. Each check leads with its verdict and the
/// server's explanation — the explanation is the actionable part.
#[must_use]
pub fn render_check(body: &Value) -> String {
    let mut out = String::new();
    let summary = body.get("summary").cloned().unwrap_or(Value::Null);
    let _ = writeln!(
        out,
        "{} — overall {} ({} pass, {} warn, {} fail, {} info)",
        s(body, "domain"),
        status_mark(s(&summary, "status")),
        n(&summary, "pass"),
        n(&summary, "warn"),
        n(&summary, "fail"),
        n(&summary, "info"),
    );
    let Some(checks) = body.get("checks").and_then(Value::as_object) else {
        out.push_str("No checks in the response.");
        return out;
    };
    // Fixed order — the most load-bearing records first.
    for (key, label) in [
        ("spf", "SPF"),
        ("dkim", "DKIM"),
        ("dmarc", "DMARC"),
        ("mx", "MX"),
        ("blocklist", "Blocklists"),
        ("mtaSts", "MTA-STS"),
        ("bimi", "BIMI"),
    ] {
        let Some(c) = checks.get(key) else { continue };
        let _ = writeln!(out, "\n[{}] {label}: {}", status_mark(s(c, "status")), s(c, "explanation"));
        if let Some(record) = c.get("record").and_then(Value::as_str) {
            let _ = writeln!(out, "    record: {record}");
        }
        if key == "spf" {
            let _ = writeln!(out, "    DNS lookups: {} of {}", n(c, "lookups"), n(c, "lookupLimit"));
        }
    }
    out.trim_end().to_string()
}

/// DMARC pass rate + sources. Zero messages is reported as a setup question,
/// never as a clean domain.
#[must_use]
pub fn render_dmarc(body: &Value) -> String {
    let mut out = String::new();
    let total = n(body, "totalMessages");
    let _ = writeln!(out, "DMARC — {}", scope(body));
    if total == 0 {
        out.push_str(
            "No DMARC reports in this window. That is NOT the same as \"no problems\": mailbox providers send nothing when the \
             reporting address or its DNS record is wrong. Check the domain's `rua=` tag points at the address on the Destinations screen.",
        );
        return out;
    }
    let rate = body.get("passRate").and_then(Value::as_f64).unwrap_or(0.0);
    let _ = writeln!(
        out,
        "Pass rate {} — {total} messages, {} passed, {} failed",
        percent(rate),
        n(body, "passMessages"),
        n(body, "failMessages"),
    );
    let sources = rows(body, "sources");
    let failing: Vec<&Value> = sources
        .iter()
        .filter(|src| !src.get("aligned").and_then(Value::as_bool).unwrap_or(false))
        .collect();
    if failing.is_empty() {
        let _ = writeln!(out, "All {} sending sources are aligned.", sources.len());
    } else {
        let _ = writeln!(out, "\nNot aligned ({} of {} sources):", failing.len(), sources.len());
        for src in failing.iter().take(15) {
            let _ = writeln!(
                out,
                "  {}  {} msgs  DKIM {}/{}  SPF {}/{}  disposition: {}",
                s(src, "sourceIp"),
                n(src, "messages"),
                n(src, "dkimPass"),
                n(src, "messages"),
                n(src, "spfPass"),
                n(src, "messages"),
                src.get("dispositions")
                    .and_then(Value::as_array)
                    .map(|d| d.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default(),
            );
        }
    }
    let reporters: Vec<&str> = rows(body, "reporters").iter().map(|r| s(r, "orgName")).filter(|x| !x.is_empty()).collect();
    if !reporters.is_empty() {
        let _ = writeln!(out, "\nReported by: {}", reporters.join(", "));
    }
    out.trim_end().to_string()
}

/// The sending-source inventory: counts first, then the sources worth a look
/// (unidentified / failing / new) ahead of the healthy ones.
#[must_use]
pub fn render_sources(body: &Value, limit: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Sending sources — {}", scope(body));
    let sources = rows(body, "sources");
    if sources.is_empty() {
        out.push_str(
            "No sending sources in this window. Sources come from DMARC aggregate reports, so none usually means no reports are \
             arriving yet — not that nothing sends as this domain.",
        );
        return out;
    }
    let _ = writeln!(
        out,
        "{} systems · {} unidentified · {} not authenticated · {} new (first seen in the last 14 days)",
        n(body, "totalSources"),
        n(body, "unidentified"),
        n(body, "failing"),
        n(body, "newSources"),
    );
    let attention = |src: &&Value| -> u8 {
        match (s(src, "verdict"), src.get("vendor").is_some_and(Value::is_null)) {
            ("failing", _) => 0,
            (_, true) => 1,
            ("mixed", _) => 2,
            _ => 3,
        }
    };
    let mut ordered: Vec<&Value> = sources.iter().collect();
    ordered.sort_by_key(|src| (attention(src), std::cmp::Reverse(n(src, "messages"))));
    out.push('\n');
    for src in ordered.iter().take(limit) {
        let vendor = src.get("vendor").and_then(Value::as_str).unwrap_or("UNIDENTIFIED");
        let mut flags = vec![s(src, "verdict").to_string()];
        if src.get("likelyForwarder").and_then(Value::as_bool).unwrap_or(false) {
            flags.push("forwarder".into());
        }
        if src.get("isNew").and_then(Value::as_bool).unwrap_or(false) {
            flags.push("new".into());
        }
        let _ = writeln!(
            out,
            "  {vendor:<22} {:<40} {:>6} msgs  DMARC pass {}  [{}]",
            s(src, "sourceIp"),
            n(src, "messages"),
            n(src, "passMessages"),
            flags.join(", "),
        );
    }
    if sources.len() > limit {
        let _ = writeln!(out, "  … {} more (raise the limit to see them)", sources.len() - limit);
    }
    out.trim_end().to_string()
}

/// TLS-RPT summary. No reports is called out the same way DMARC's is.
#[must_use]
pub fn render_tls(body: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "SMTP TLS reports — {}", scope(body));
    let ok = n(body, "successfulSessions");
    let failed = n(body, "failedSessions");
    if ok + failed == 0 {
        out.push_str(
            "No TLS reports in this window. Senders only report once `_smtp._tls.<domain>` publishes a `v=TLSRPTv1; rua=` \
             address, and the first reports take about a day to arrive.",
        );
        return out;
    }
    let rate = body.get("successRate").and_then(Value::as_f64).unwrap_or(0.0);
    let _ = writeln!(out, "{} of sessions used trusted TLS — {ok} succeeded, {failed} failed", percent(rate));
    if let Some(last) = body.get("lastReportAt").and_then(Value::as_str) {
        let _ = writeln!(out, "Last report: {last}");
    }
    let failures = rows(body, "failures");
    if !failures.is_empty() {
        out.push_str("\nFailures:\n");
        for f in failures {
            let hosts = f
                .get("hosts")
                .and_then(Value::as_array)
                .map(|h| h.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  {} — {} session(s): {} [{}]",
                s(f, "resultType"),
                n(f, "sessions"),
                s(f, "meaning"),
                hosts
            );
        }
    }
    out.trim_end().to_string()
}

/// Signing domains and who is signed on each.
#[must_use]
pub fn render_signatures(body: &Value) -> String {
    let configs = rows(body, "configs");
    if configs.is_empty() {
        return "No signing domains configured. Add one under Workforce → Signatures.".to_string();
    }
    let mut out = String::new();
    for cfg in configs {
        let renders = rows(cfg, "renders");
        let rendered = cfg.get("lastRenderedAt").and_then(Value::as_str).unwrap_or("never");
        let _ = writeln!(
            out,
            "{} — {} · last rendered {rendered} · {} signed sender(s)",
            s(cfg, "domain"),
            s(cfg, "status"),
            renders.len()
        );
        for r in renders {
            let _ = writeln!(out, "  {}", s(r, "senderEmail"));
        }
        if s(cfg, "status") == "active" && renders.is_empty() {
            out.push_str("  (active but nobody is signed — re-render to fill it)\n");
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::json;

    use super::*;

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[test]
    fn check_requires_a_domain() {
        assert!(Wrap::try_parse_from(["t", "check"]).is_err());
        let w = Wrap::try_parse_from(["t", "check", "smoo.ai"]).expect("domain parses");
        assert!(matches!(w.cmd, Cmd::Check { ref domain, json: false, org: None } if domain == "smoo.ai"));
    }

    #[test]
    fn report_commands_default_to_thirty_days_and_all_domains() {
        for sub in ["dmarc", "tls"] {
            let w = Wrap::try_parse_from(["t", sub]).expect("parses bare");
            match w.cmd {
                Cmd::Dmarc { domain, days, .. } | Cmd::Tls { domain, days, .. } => {
                    assert_eq!(days, DEFAULT_DAYS);
                    assert!(domain.is_none());
                }
                _ => panic!("wrong variant for {sub}"),
            }
        }
        let w = Wrap::try_parse_from(["t", "sources", "--domain", "smoo.ai", "--days", "90", "--limit", "5"]).expect("flags parse");
        assert!(matches!(w.cmd, Cmd::Sources { domain: Some(ref d), days: 90, limit: 5, .. } if d == "smoo.ai"));
    }

    #[test]
    fn window_query_encodes_and_skips_absent_domain() {
        assert_eq!(window_query(None, 30), "?days=30");
        assert_eq!(window_query(Some("a b.com"), 7), "?domain=a%20b.com&days=7");
    }

    #[test]
    fn check_leads_each_record_with_its_verdict_and_counts_spf_lookups() {
        let body = json!({
            "domain": "smoo.ai",
            "summary": { "status": "warn", "pass": 4, "warn": 1, "fail": 0, "info": 2 },
            "checks": {
                "spf": { "status": "pass", "explanation": "One record.", "record": "v=spf1 -all", "lookups": 3, "lookupLimit": 10 },
                "dmarc": { "status": "warn", "explanation": "Monitoring only.", "record": "v=DMARC1; p=none" },
            }
        });
        let out = render_check(&body);
        assert!(out.starts_with("smoo.ai — overall WARN (4 pass, 1 warn, 0 fail, 2 info)"));
        assert!(out.contains("[PASS] SPF: One record."));
        assert!(out.contains("DNS lookups: 3 of 10"));
        assert!(out.contains("[WARN] DMARC: Monitoring only."));
        // SPF is printed before DMARC regardless of JSON key order.
        assert!(out.find("SPF").unwrap() < out.find("DMARC").unwrap());
    }

    #[test]
    fn dmarc_with_no_messages_is_a_setup_question_not_an_all_clear() {
        let out = render_dmarc(&json!({ "domain": "x.com", "windowDays": 30, "totalMessages": 0, "sources": [] }));
        assert!(out.contains("NOT the same as \"no problems\""));
        assert!(!out.contains("aligned"));
    }

    #[test]
    fn dmarc_lists_only_unaligned_sources() {
        let body = json!({
            "domain": "smoo.ai", "windowDays": 30, "totalMessages": 100, "passMessages": 90, "failMessages": 10, "passRate": 0.9,
            "sources": [
                { "sourceIp": "1.1.1.1", "messages": 90, "dkimPass": 90, "spfPass": 90, "dispositions": ["none"], "aligned": true },
                { "sourceIp": "2.2.2.2", "messages": 10, "dkimPass": 0, "spfPass": 3, "dispositions": ["reject"], "aligned": false }
            ],
            "reporters": [{ "orgName": "google.com" }]
        });
        let out = render_dmarc(&body);
        assert!(out.contains("Pass rate 90.0% — 100 messages, 90 passed, 10 failed"));
        assert!(out.contains("Not aligned (1 of 2 sources)"));
        assert!(out.contains("2.2.2.2"));
        assert!(!out.contains("1.1.1.1"));
        assert!(out.contains("Reported by: google.com"));
    }

    #[test]
    fn sources_put_failing_and_unidentified_first() {
        let body = json!({
            "domain": "smoo.ai", "windowDays": 30, "totalSources": 3, "unidentified": 1, "failing": 1, "newSources": 1,
            "sources": [
                { "sourceIp": "9.9.9.9", "vendor": "Google", "messages": 500, "passMessages": 500, "verdict": "aligned", "likelyForwarder": false, "isNew": false },
                { "sourceIp": "8.8.8.8", "vendor": null, "messages": 5, "passMessages": 5, "verdict": "aligned", "likelyForwarder": false, "isNew": true },
                { "sourceIp": "7.7.7.7", "vendor": "SendGrid", "messages": 20, "passMessages": 0, "verdict": "failing", "likelyForwarder": false, "isNew": false }
            ]
        });
        let out = render_sources(&body, 20);
        assert!(out.contains("3 systems · 1 unidentified · 1 not authenticated · 1 new"));
        let (failing, unidentified, healthy) = (out.find("7.7.7.7").unwrap(), out.find("8.8.8.8").unwrap(), out.find("9.9.9.9").unwrap());
        assert!(failing < unidentified && unidentified < healthy, "attention order wrong:\n{out}");
        assert!(out.contains("UNIDENTIFIED"));
        assert!(out.contains("new"));
    }

    #[test]
    fn sources_truncation_is_announced() {
        let src = json!({ "sourceIp": "1.1.1.1", "vendor": "Google", "messages": 1, "passMessages": 1, "verdict": "aligned" });
        let body = json!({ "windowDays": 30, "totalSources": 3, "sources": [src.clone(), src.clone(), src] });
        assert!(render_sources(&body, 2).contains("… 1 more"));
    }

    #[test]
    fn empty_sources_and_tls_explain_themselves() {
        assert!(render_sources(&json!({ "windowDays": 30, "sources": [] }), 20).contains("no reports are arriving yet"));
        let tls = render_tls(&json!({ "windowDays": 30, "successfulSessions": 0, "failedSessions": 0 }));
        assert!(tls.contains("No TLS reports"));
        assert!(tls.contains("_smtp._tls"));
    }

    #[test]
    fn tls_reports_rate_and_failures() {
        let body = json!({
            "domain": "smoo.ai", "windowDays": 30, "successfulSessions": 99, "failedSessions": 1, "successRate": 0.99,
            "lastReportAt": "2026-09-25T00:00:00Z",
            "failures": [{ "resultType": "certificate-expired", "meaning": "The MX certificate has expired.", "sessions": 1, "hosts": ["mx.smoo.ai"] }]
        });
        let out = render_tls(&body);
        assert!(out.contains("99.0% of sessions used trusted TLS"));
        assert!(out.contains("certificate-expired — 1 session(s): The MX certificate has expired. [mx.smoo.ai]"));
    }

    #[test]
    fn signatures_list_domains_and_flag_an_empty_active_one() {
        let body = json!({ "configs": [
            { "domain": "smoo.ai", "status": "active", "lastRenderedAt": "2026-09-22T21:47:56Z", "renders": [{ "senderEmail": "brent@smoo.ai" }] },
            { "domain": "example.com", "status": "active", "renders": [] },
        ]});
        let out = render_signatures(&body);
        assert!(out.contains("smoo.ai — active · last rendered 2026-09-22T21:47:56Z · 1 signed sender(s)"));
        assert!(out.contains("  brent@smoo.ai"));
        assert!(out.contains("active but nobody is signed"));
        assert!(render_signatures(&json!({ "configs": [] })).contains("No signing domains"));
    }

    #[test]
    fn rows_accepts_envelope_and_bare_array() {
        assert_eq!(rows(&json!({ "data": [1, 2] }), "data").len(), 2);
        assert_eq!(rows(&json!([1]), "data").len(), 1);
        assert!(rows(&json!({}), "data").is_empty());
    }
}

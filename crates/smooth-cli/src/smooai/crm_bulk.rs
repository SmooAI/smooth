//! CRM writes at scale — the CLI twins of the copilot's `crm.capture`,
//! `crm.import_apply`, `contacts.import_preview` and `contacts.import`
//! (SMOODEV-3611). Wired into `smoo crm capture` and
//! `smoo crm contacts import-table` / `import-address-book` in `crm.rs`.
//!
//! The routes respond `{ok, summary, data[, dryRun]}` (capture / import-table)
//! or the address-book shapes (`{contacts, totalPeople}` preview, `{total,
//! created, updated, skipped}` sync). Imports PREVIEW by default; nothing is
//! written without `--apply`, mirroring the MCP tools' preview-first posture.
//!
//! The pure flag → body mappings live here so they are unit-tested without a
//! server.

use std::io::Read as _;

use anstream::println;
use anyhow::{bail, Context, Result};
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};

use super::print_json;

/// CRM fields a `--map field=Header` may name (`crm.import_apply`'s mapping).
pub(crate) const IMPORT_FIELDS: &[&str] = &["email", "first_name", "last_name", "name", "phone", "title", "company", "company_domain"];

/// Flags for `smoo crm capture`, borrowed.
pub(crate) struct CaptureArgs<'a> {
    pub company: Option<&'a str>,
    pub domain: Option<&'a str>,
    pub contacts: &'a [String],
    pub deal: Option<&'a str>,
    pub amount: Option<f64>,
    pub stage: Option<&'a str>,
    pub close_date: Option<&'a str>,
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// One `--contact` spec as a capture contact object.
///
/// Accepts `"First Last <a@b.com>"`, a bare email, a phone number, or
/// `"First Last <+13175550142>"`. A name with no email/phone is refused — the
/// route skips a contact with no identity, and silently dropping it here would
/// read as success.
pub(crate) fn parse_contact_spec(spec: &str) -> Result<Value> {
    let spec = spec.trim();
    let (name, ident) = match (spec.find('<'), spec.rfind('>')) {
        (Some(l), Some(r)) if r > l => (spec[..l].trim(), spec[l + 1..r].trim()),
        _ => ("", spec),
    };
    let mut out = Map::new();
    if ident.contains('@') {
        out.insert("email".into(), json!(ident));
    } else if ident.chars().filter(char::is_ascii_digit).count() >= 7 && ident.chars().all(|c| c.is_ascii_digit() || " +-().".contains(c)) {
        out.insert("phone".into(), json!(ident));
    } else {
        bail!("contact {spec:?} needs an email or phone — use \"First Last <a@b.com>\", a bare email, or a phone number");
    }
    let mut parts = name.split_whitespace();
    if let Some(first) = parts.next() {
        out.insert("firstName".into(), json!(first));
        let rest: Vec<&str> = parts.collect();
        if !rest.is_empty() {
            out.insert("lastName".into(), json!(rest.join(" ")));
        }
    }
    Ok(Value::Object(out))
}

/// `POST /crm/capture` body.
pub(crate) fn capture_body(a: &CaptureArgs<'_>) -> Result<Value> {
    if a.contacts.is_empty() {
        bail!("pass at least one --contact");
    }
    let contacts = a.contacts.iter().map(|c| parse_contact_spec(c)).collect::<Result<Vec<_>>>()?;
    let mut body = json!({ "contacts": contacts });
    match non_empty(a.company) {
        Some(name) => {
            let mut co = json!({ "name": name });
            if let Some(d) = non_empty(a.domain) {
                co["domain"] = json!(d);
            }
            body["company"] = co;
        }
        None if non_empty(a.domain).is_some() => bail!("--domain needs --company"),
        None => {}
    }
    match non_empty(a.deal) {
        Some(title) => {
            let mut deal = json!({ "title": title });
            if let Some(v) = a.amount {
                deal["amount"] = json!(v);
            }
            if let Some(s) = non_empty(a.stage) {
                deal["stage"] = json!(s);
            }
            if let Some(d) = non_empty(a.close_date) {
                if chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_err() {
                    bail!("--close-date {d:?} must be YYYY-MM-DD");
                }
                deal["closeDate"] = json!(d);
            }
            body["deal"] = deal;
        }
        None if a.amount.is_some() || non_empty(a.stage).is_some() || non_empty(a.close_date).is_some() => {
            bail!("--amount / --stage / --close-date need --deal")
        }
        None => {}
    }
    Ok(body)
}

/// `--map field=Header` flags as the `mapping` object. `email` is required and
/// a field may be mapped once.
pub(crate) fn parse_mapping(maps: &[String]) -> Result<Value> {
    let mut out = Map::new();
    for m in maps {
        let Some((field, header)) = m.split_once('=') else {
            bail!("--map {m:?} must be field=Header (e.g. email=Email)");
        };
        let field = field.trim().to_ascii_lowercase().replace('-', "_");
        let header = header.trim();
        if !IMPORT_FIELDS.contains(&field.as_str()) {
            bail!("unknown --map field {field:?} — one of {}", IMPORT_FIELDS.join(", "));
        }
        if header.is_empty() {
            bail!("--map {field}= needs a header name");
        }
        if out.insert(field.clone(), json!(header)).is_some() {
            bail!("--map {field} given twice");
        }
    }
    if !out.contains_key("email") {
        bail!("--map email=<Header> is required — rows are upserted by email");
    }
    Ok(Value::Object(out))
}

/// `--delimiter` normalised to what the route takes (`,` or `\t`).
fn delimiter_value(d: &str) -> Result<&'static str> {
    match d.trim() {
        "," | "comma" => Ok(","),
        "\t" | "\\t" | "tab" | "tsv" => Ok("\t"),
        other => bail!("--delimiter {other:?} must be `,` or `tab`"),
    }
}

/// `POST /crm/import-table` body. `dry_run` is the default (no `--apply`).
pub(crate) fn import_table_body(table: &str, maps: &[String], delimiter: Option<&str>, skip_rows: &[u64], dry_run: bool) -> Result<Value> {
    if table.trim().is_empty() {
        bail!("the table is empty — pass a CSV/TSV file with a header row");
    }
    let mut body = json!({ "table": table, "mapping": parse_mapping(maps)?, "dryRun": dry_run });
    if let Some(d) = delimiter {
        body["delimiter"] = json!(delimiter_value(d)?);
    }
    if !skip_rows.is_empty() {
        body["skip_rows"] = json!(skip_rows);
    }
    Ok(body)
}

/// `google` / `microsoft` (a few spellings), refused otherwise.
pub(crate) fn address_book_provider(p: &str) -> Result<&'static str> {
    match p.trim().to_ascii_lowercase().as_str() {
        "google" | "gmail" | "gsuite" => Ok("google"),
        "microsoft" | "outlook" | "office365" | "m365" | "ms" => Ok("microsoft"),
        other => bail!("--provider {other:?} must be google or microsoft"),
    }
}

/// A file path or `-` (stdin) as text.
pub(crate) fn read_text(path: &str) -> Result<String> {
    if path == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("read stdin")?;
        Ok(s)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {path}"))
    }
}

fn summary(r: &Value) -> &str {
    r.get("summary").and_then(Value::as_str).unwrap_or("")
}

/// `{ok, summary, data}` → the summary line (or the raw JSON with `--json`).
pub(crate) fn print_outcome(r: &Value, json: bool) {
    if json || summary(r).is_empty() {
        print_json(r);
    } else {
        println!("  {} {}", "✓".green(), summary(r));
    }
}

fn num(v: &Value, ptr: &str) -> Option<u64> {
    v.pointer(ptr).and_then(Value::as_u64)
}

/// The counts line for an import-table response.
pub(crate) fn import_table_counts(r: &Value, applied: bool) -> String {
    let total = num(r, "/data/preview/total").unwrap_or(0);
    let invalid = num(r, "/data/preview/skippedInvalid").unwrap_or(0);
    let contacts = num(r, "/data/createdContacts").unwrap_or(0);
    let companies = num(r, "/data/createdCompanies").unwrap_or(0);
    let verb = if applied { "upserted" } else { "would upsert" };
    format!(
        "{total} row(s); {verb} {contacts} contact(s) and {companies} compan{}; {invalid} skipped (no valid email)",
        if companies == 1 { "y" } else { "ies" }
    )
}

pub(crate) fn print_import_table(r: &Value, json: bool, applied: bool) {
    if json {
        print_json(r);
        return;
    }
    if !summary(r).is_empty() {
        println!("  {}", summary(r));
    }
    println!("  {}", import_table_counts(r, applied));
    if !applied {
        println!("  {} dry run — nothing written. Re-run with --apply to import.", "ℹ".cyan());
    }
}

pub(crate) fn print_address_book_preview(r: &Value, json: bool, provider: &str) {
    if json {
        print_json(r);
        return;
    }
    let rows = r.get("contacts").and_then(Value::as_array).cloned().unwrap_or_default();
    match r.get("totalPeople").and_then(Value::as_u64) {
        Some(t) => println!("  {t} contact(s) in your {provider} address book. Sample:"),
        None => println!("  Sample of your {provider} address book:"),
    }
    for c in &rows {
        let name = c.get("displayName").and_then(Value::as_str).unwrap_or("(no name)");
        let email = c.get("primaryEmail").and_then(Value::as_str).unwrap_or("— no email (skipped on import)");
        println!("    {} {}", name.bold(), email.dimmed());
    }
    println!(
        "  {} preview only — re-run with --apply to import (upserts by email; re-running is safe).",
        "ℹ".cyan()
    );
}

pub(crate) fn print_address_book_sync(r: &Value, json: bool) {
    if json {
        print_json(r);
        return;
    }
    let n = |k: &str| r.get(k).and_then(Value::as_u64).unwrap_or(0);
    println!(
        "  {} imported {} contact(s): {} created, {} updated, {} skipped",
        "✓".green(),
        n("total"),
        n("created"),
        n("updated"),
        n("skipped")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_spec_parses_name_and_email_or_phone() {
        assert_eq!(
            parse_contact_spec("Jane Q Doe <jane@acme.com>").unwrap(),
            json!({ "email": "jane@acme.com", "firstName": "Jane", "lastName": "Q Doe" })
        );
        assert_eq!(parse_contact_spec("bob@acme.com").unwrap(), json!({ "email": "bob@acme.com" }));
        assert_eq!(parse_contact_spec("+1 (317) 555-0142").unwrap(), json!({ "phone": "+1 (317) 555-0142" }));
        assert!(parse_contact_spec("Just A Name").is_err());
    }

    fn args<'a>(contacts: &'a [String]) -> CaptureArgs<'a> {
        CaptureArgs {
            company: None,
            domain: None,
            contacts,
            deal: None,
            amount: None,
            stage: None,
            close_date: None,
        }
    }

    #[test]
    fn capture_body_builds_company_contacts_and_deal() {
        let contacts = vec!["Jane <jane@acme.com>".to_string()];
        let mut a = args(&contacts);
        a.company = Some("Acme");
        a.domain = Some("acme.com");
        a.deal = Some("Acme renewal");
        a.amount = Some(1200.0);
        a.close_date = Some("2026-12-01");
        let b = capture_body(&a).unwrap();
        assert_eq!(b["company"], json!({ "name": "Acme", "domain": "acme.com" }));
        assert_eq!(b["contacts"][0]["email"], "jane@acme.com");
        assert_eq!(b["deal"], json!({ "title": "Acme renewal", "amount": 1200.0, "closeDate": "2026-12-01" }));
    }

    #[test]
    fn capture_body_refuses_orphan_flags() {
        let contacts = vec!["a@b.com".to_string()];
        let mut a = args(&contacts);
        a.amount = Some(5.0);
        assert!(capture_body(&a).is_err(), "--amount without --deal");
        let mut a = args(&contacts);
        a.domain = Some("b.com");
        assert!(capture_body(&a).is_err(), "--domain without --company");
        let mut a = args(&contacts);
        a.deal = Some("x");
        a.close_date = Some("next week");
        assert!(capture_body(&a).is_err(), "bad close date");
        assert!(capture_body(&args(&[])).is_err(), "no contacts");
        let b = capture_body(&args(&contacts)).unwrap();
        assert!(b.get("company").is_none() && b.get("deal").is_none());
    }

    #[test]
    fn mapping_requires_email_and_known_unique_fields() {
        let m = |v: &[&str]| parse_mapping(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(
            m(&["email=E-mail", "first-name=First"]).unwrap(),
            json!({ "email": "E-mail", "first_name": "First" })
        );
        assert!(m(&["first_name=First"]).is_err(), "email required");
        assert!(m(&["email=A", "email=B"]).is_err(), "duplicate");
        assert!(m(&["email=A", "nickname=N"]).is_err(), "unknown field");
        assert!(m(&["email"]).is_err(), "no =");
    }

    #[test]
    fn import_table_body_defaults_to_dry_run() {
        let maps = vec!["email=Email".to_string()];
        let b = import_table_body("Email\na@b.com\n", &maps, Some("tab"), &[2], true).unwrap();
        assert_eq!(b["dryRun"], true);
        assert_eq!(b["delimiter"], "\t");
        assert_eq!(b["skip_rows"], json!([2]));
        let b = import_table_body("Email\na@b.com\n", &maps, None, &[], false).unwrap();
        assert_eq!(b["dryRun"], false);
        assert!(b.get("delimiter").is_none() && b.get("skip_rows").is_none());
        assert!(import_table_body("  ", &maps, None, &[], true).is_err());
        assert!(import_table_body("x", &maps, Some(";"), &[], true).is_err());
    }

    #[test]
    fn provider_aliases() {
        assert_eq!(address_book_provider("Google").unwrap(), "google");
        assert_eq!(address_book_provider("outlook").unwrap(), "microsoft");
        assert!(address_book_provider("yahoo").is_err());
    }

    #[test]
    fn import_counts_read_the_response() {
        let r = json!({ "data": { "preview": { "total": 10, "skippedInvalid": 2 }, "createdContacts": 8, "createdCompanies": 1 } });
        assert_eq!(
            import_table_counts(&r, false),
            "10 row(s); would upsert 8 contact(s) and 1 company; 2 skipped (no valid email)"
        );
        assert!(import_table_counts(&r, true).contains("upserted 8"));
    }
}

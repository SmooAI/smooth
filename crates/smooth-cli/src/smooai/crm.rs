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
use serde_json::Value;

use super::{print_json, read_body};
use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// Contact records (list / get / create / update / import).
    Contacts {
        #[command(subcommand)]
        cmd: ContactsCmd,
    },
}

#[derive(Subcommand)]
pub enum ContactsCmd {
    /// List contacts for the org.
    List {
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        search: Option<String>,
        #[arg(long, default_value = "50")]
        limit: u32,
    },
    /// Get a single contact by id.
    Get {
        contact_id: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Create a contact from a JSON body (file path, or `-` for stdin).
    Create {
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Update a contact from a JSON body (file path, or `-` for stdin).
    Update {
        contact_id: String,
        body: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Idempotent bulk upsert from a JSON array file. Dedup key:
    /// lowercased email, else last-10 phone digits. `--dry-run` to
    /// preview without writing.
    Import {
        /// Path to a JSON file containing an array of contact objects.
        file: String,
        #[arg(long)]
        org: Option<String>,
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
    let client = UserClient::from_user_session()?;
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
            print_json(&client.get(&format!("/organizations/{org}/crm/contacts/{contact_id}")).await.context("GET contact")?);
        }
        ContactsCmd::Create { body, org } => {
            let org = resolve_org(org)?;
            let body = read_body(&body)?;
            print_json(&client.post(&format!("/organizations/{org}/crm/contacts"), &body).await.context("POST contact")?);
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
                exec(client, org, Op::Update(&id), item, rate).await.with_context(|| format!("update {label}"))?;
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
    use super::{norm_email, norm_phone};
    use serde_json::json;

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

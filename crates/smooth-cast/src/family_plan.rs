//! The Family plan path of Big Smooth onboarding (SMOODEV-3918): mint this
//! machine's Smoo AI gateway key, or learn that the plan is needed first.
//!
//! Server contract (smooai SMOODEV-3916): `POST /organizations/{org}/llm-gateway/keys`
//! with `{"name": "big-smooth-<host>"}` mints a named LiteLLM key and returns
//! `{ key, … }` once. A `big-smooth*` name without an active Family plan
//! returns **402** `{ "error": "family_plan_required", "message", "upgradeUrl" }`.
//! A name that already exists returns 409 — the value is shown only at mint
//! time, so we rotate it (`…/keys/{name}/rotate`): the key is this machine's
//! own, nothing else uses it. Before the 402 ships, the mint simply succeeds.
//!
//! Shared by `th`'s first-run picker and the daemon's `POST
//! /api/llm/provider/family` (the web SPA's first-run card).

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/// Where to buy the plan when a 402 carries no `upgradeUrl`.
pub const FAMILY_PLAN_URL: &str = "https://smoo.ai/apps/start/product?plan=Family%20plan&term=monthly";

/// The one-line pitch every surface shows.
pub const FAMILY_PLAN_PITCH: &str = "Family plan — $9/month, 14-day free trial, hosted models via the Smoo AI gateway";

/// SMOODEV-3969: the trial's AI allowance is small on purpose (bots), so every
/// Family plan surface also says how to keep going past it: "Top up $5 or $10
/// anytime during the trial; auto-reload optional — `smoo credits buy`".
#[must_use]
pub fn top_up_note() -> String {
    format!("{} — `smoo credits buy`", crate::ai_credits::top_up_line(" during the trial"))
}

/// The gateway's coding default (concrete: the legacy `smooth-*` aliases are
/// gone at the gateway, SMOODEV-1793). Tracks the coding slot so it can't drift.
pub const SMOO_DEFAULT_MODEL: &str = smooth_policy::smooth_alias::SmoothSlot::Coding.concrete_default();

/// What one mint attempt produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintOutcome {
    /// The key value (shown once).
    Minted(String),
    /// No active Family plan: send the user to `upgrade_url`.
    PlanRequired { upgrade_url: String, message: String },
}

/// `big-smooth-<host>`: lowercase `[a-z0-9-]`, ≤ 64 chars (the server's key-name rule).
#[must_use]
pub fn key_name(host: &str) -> String {
    let mut slug = String::new();
    for c in host.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "this-machine" } else { slug };
    let mut name = format!("big-smooth-{slug}");
    name.truncate(64);
    name.trim_end_matches('-').to_string()
}

/// This machine's short host name (`hostname`, then `HOSTNAME`/`COMPUTERNAME`).
#[must_use]
pub fn this_host() -> String {
    let from_cmd = std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    from_cmd
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|h| h.split('.').next().unwrap_or_default().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this-machine".into())
}

/// Classify a 402 body. `Some` for any 402 — a body we can't parse still has to
/// send the user somewhere.
#[must_use]
pub fn plan_required_from(status: u16, body: &str) -> Option<MintOutcome> {
    if status != 402 {
        return None;
    }
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let upgrade_url = v
        .get("upgradeUrl")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("https://"))
        .unwrap_or(FAMILY_PLAN_URL)
        .to_string();
    let message = v
        .get("message")
        .and_then(Value::as_str)
        .filter(|m| !m.trim().is_empty())
        .unwrap_or("Big Smooth's hosted models need the Family plan.")
        .to_string();
    Some(MintOutcome::PlanRequired { upgrade_url, message })
}

fn key_from(body: &str, what: &str) -> Result<String> {
    let v: Value = serde_json::from_str(body).with_context(|| format!("parse the {what} response"))?;
    v.get("key")
        .and_then(Value::as_str)
        .filter(|k| !k.trim().is_empty())
        .map(|k| k.trim().to_string())
        .with_context(|| format!("{what} returned no key"))
}

async fn post(http: &reqwest::Client, url: &str, bearer: &str, body: &Value) -> Result<(u16, String)> {
    let resp = http
        .post(url)
        .bearer_auth(bearer)
        .json(body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status().as_u16();
    Ok((status, resp.text().await.unwrap_or_default()))
}

/// Mint (or, when the name exists, rotate) this machine's gateway key.
///
/// # Errors
/// Network failure, 401 (sign in again), or any other non-2xx/402 answer.
pub async fn mint_or_rotate(http: &reqwest::Client, api_base: &str, bearer: &str, org: &str, name: &str) -> Result<MintOutcome> {
    let base = api_base.trim_end_matches('/');
    let url = format!("{base}/organizations/{org}/llm-gateway/keys");
    let (status, body) = post(http, &url, bearer, &json!({ "name": name })).await?;
    if let Some(p) = plan_required_from(status, &body) {
        return Ok(p);
    }
    match status {
        200..=299 => return Ok(MintOutcome::Minted(key_from(&body, "the key mint")?)),
        409 => {}
        401 => bail!("your Smoo AI session expired — sign in again (smoo auth login)"),
        403 => bail!("only an org admin can mint gateway keys for this org (HTTP 403): {body}"),
        _ => bail!("minting the gateway key failed (HTTP {status}): {body}"),
    }
    let url = format!("{base}/organizations/{org}/llm-gateway/keys/{name}/rotate");
    let (status, body) = post(http, &url, bearer, &json!({})).await?;
    if let Some(p) = plan_required_from(status, &body) {
        return Ok(p);
    }
    match status {
        200..=299 => Ok(MintOutcome::Minted(key_from(&body, "the key rotation")?)),
        401 => bail!("your Smoo AI session expired — sign in again (smoo auth login)"),
        _ => bail!("rotating the existing `{name}` key failed (HTTP {status}): {body}"),
    }
}

/// Wait before the `attempt`-th retry (0-based) while the checkout is open:
/// every 5 s for the first minute, then every 15 s (the entitlement usually
/// lands seconds after checkout; no need to hammer the API after that).
#[must_use]
pub const fn plan_poll_delay(attempt: u32) -> std::time::Duration {
    if attempt < 12 {
        std::time::Duration::from_secs(5)
    } else {
        std::time::Duration::from_secs(15)
    }
}

/// Retries in ~10 minutes of [`plan_poll_delay`]: 12 × 5 s + 36 × 15 s.
pub const PLAN_POLL_MAX: u32 = 48;

/// The first org id in a `GET /organizations` answer (`{data: [...]}` or a bare array).
#[must_use]
pub fn first_org_id(body: &Value) -> Option<String> {
    body.get("data")
        .and_then(Value::as_array)
        .or_else(|| body.as_array())?
        .iter()
        .find_map(|o| o.get("id").and_then(Value::as_str).filter(|id| !id.trim().is_empty()).map(str::to_string))
}

/// The user's first org, or `None` when they have none yet. A new user gets a
/// personal org when they start the Family plan checkout, so the org must be
/// re-resolved after checkout rather than once up front.
///
/// # Errors
/// Network failure or a non-2xx answer.
pub async fn discover_org(http: &reqwest::Client, api_base: &str, bearer: &str) -> Result<Option<String>> {
    let url = format!("{}/organizations", api_base.trim_end_matches('/'));
    let resp = http.get(&url).bearer_auth(bearer).send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("listing your orgs failed (HTTP {status}): {text}");
    }
    Ok(first_org_id(&serde_json::from_str(&text).unwrap_or(Value::Null)))
}

/// The answer for a signed-in user with no org yet: the checkout creates one.
#[must_use]
pub fn no_org_yet() -> MintOutcome {
    MintOutcome::PlanRequired {
        upgrade_url: FAMILY_PLAN_URL.to_string(),
        message: "Start your Family plan — checkout sets up your Smoo account for Big Smooth.".to_string(),
    }
}

/// Register the Smoo gateway provider in `providers.json` without clobbering
/// other providers; it becomes the default when nothing else works (the same
/// rule `th model login` applies) and the `coding` slot points at it.
///
/// # Errors
/// An empty key, or a write failure.
pub fn save_gateway_key(path: &Path, key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        bail!("empty gateway key");
    }
    let mut registry = if path.exists() {
        crate::provider_migration::load_providers_with_migration(path).unwrap_or_default()
    } else {
        smooth_operator::providers::ProviderRegistry::default()
    };
    let mut cfg = smooth_operator::providers::ProviderConfig::smooai_gateway(key);
    cfg.default_model = SMOO_DEFAULT_MODEL.to_string();
    registry.register_provider(cfg);
    if registry.default_llm_config().is_err() || registry.list_providers().len() == 1 {
        registry.set_default_provider("smooai-gateway");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    registry.save_to_file(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn top_up_note_names_the_packs_and_the_verb() {
        let note = top_up_note();
        assert_eq!(note, "Top up $5 or $10 anytime during the trial; auto-reload optional — `smoo credits buy`");
    }

    /// A one-file mock of api.smoo.ai: answers each request with the next
    /// scripted `(status, body)` and records the request line + body.
    async fn mock(script: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        tokio::spawn(async move {
            for (status, body) in script {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = vec![0u8; 16 * 1024];
                let mut req = String::new();
                // Read until the body (Content-Length) is complete.
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    req.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if let Some((head, rest)) = req.split_once("\r\n\r\n") {
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if rest.len() >= len {
                            break;
                        }
                    }
                }
                let line = req.lines().next().unwrap_or_default().to_string();
                let body_in = req.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
                let auth = req
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .unwrap_or_default()
                    .to_string();
                seen2.lock().unwrap().push(format!("{line} | {auth} | {body_in}"));
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (base, seen)
    }

    #[test]
    fn key_name_is_a_valid_server_name() {
        assert_eq!(key_name("Brents-MacBook-Pro"), "big-smooth-brents-macbook-pro");
        assert_eq!(key_name("smoo hub.local"), "big-smooth-smoo-hub-local");
        assert_eq!(key_name("!!!"), "big-smooth-this-machine");
        let long = key_name(&"x".repeat(200));
        assert!(long.len() <= 64 && long.starts_with("big-smooth-"));
        assert!(!this_host().is_empty());
    }

    #[test]
    fn plan_required_reads_the_upgrade_url_or_falls_back() {
        let body = r#"{"error":"family_plan_required","message":"Start your trial","upgradeUrl":"https://smoo.ai/apps/start/product?plan=Family%20plan&term=monthly"}"#;
        assert_eq!(
            plan_required_from(402, body),
            Some(MintOutcome::PlanRequired {
                upgrade_url: FAMILY_PLAN_URL.into(),
                message: "Start your trial".into()
            })
        );
        // Unparseable body or a non-https URL still lands on the plan page.
        assert!(matches!(plan_required_from(402, "nope"), Some(MintOutcome::PlanRequired { upgrade_url, .. }) if upgrade_url == FAMILY_PLAN_URL));
        assert!(
            matches!(plan_required_from(402, r#"{"upgradeUrl":"javascript:alert(1)"}"#), Some(MintOutcome::PlanRequired { upgrade_url, .. }) if upgrade_url == FAMILY_PLAN_URL)
        );
        assert_eq!(plan_required_from(200, body), None);
        assert_eq!(plan_required_from(409, body), None);
    }

    #[test]
    fn poll_backs_off_after_a_minute_and_caps_near_ten_minutes() {
        assert_eq!(plan_poll_delay(0).as_secs(), 5);
        assert_eq!(plan_poll_delay(11).as_secs(), 5);
        assert_eq!(plan_poll_delay(12).as_secs(), 15);
        let total: u64 = (1..PLAN_POLL_MAX).map(|i| plan_poll_delay(i - 1).as_secs()).sum();
        assert!((540..=660).contains(&total), "{total}");
    }

    #[test]
    fn first_org_reads_both_shapes() {
        assert_eq!(first_org_id(&json!({"data": [{"id": "o1"}, {"id": "o2"}]})).as_deref(), Some("o1"));
        assert_eq!(first_org_id(&json!([{"name": "x"}, {"id": "o3"}])).as_deref(), Some("o3"));
        assert_eq!(first_org_id(&json!({"data": []})), None);
        assert_eq!(first_org_id(&Value::Null), None);
        assert!(matches!(no_org_yet(), MintOutcome::PlanRequired { upgrade_url, .. } if upgrade_url == FAMILY_PLAN_URL));
    }

    #[tokio::test]
    async fn discover_org_lists_the_users_orgs() {
        let (base, seen) = mock(vec![(200, r#"{"data":[{"id":"personal-1"}]}"#)]).await;
        assert_eq!(
            discover_org(&reqwest::Client::new(), &base, "jwt").await.unwrap().as_deref(),
            Some("personal-1")
        );
        assert!(seen.lock().unwrap()[0].starts_with("GET /organizations HTTP"));
        let (base, _) = mock(vec![(200, r#"{"data":[]}"#)]).await;
        assert_eq!(discover_org(&reqwest::Client::new(), &base, "jwt").await.unwrap(), None);
    }

    #[tokio::test]
    async fn mint_returns_the_key_and_sends_the_name_and_bearer() {
        let (base, seen) = mock(vec![(201, r#"{"id":"1","name":"big-smooth-x","key":"sk-new"}"#)]).await;
        let out = mint_or_rotate(&reqwest::Client::new(), &base, "jwt", "org-1", "big-smooth-x").await.unwrap();
        assert_eq!(out, MintOutcome::Minted("sk-new".into()));
        let seen = seen.lock().unwrap();
        assert!(seen[0].starts_with("POST /organizations/org-1/llm-gateway/keys HTTP"), "{}", seen[0]);
        assert!(seen[0].contains("Bearer jwt"));
        assert!(seen[0].contains(r#""name":"big-smooth-x""#));
    }

    #[tokio::test]
    async fn mint_402_is_the_upgrade_path_not_an_error() {
        let (base, _) = mock(vec![(
            402,
            r#"{"error":"family_plan_required","message":"Family plan required","upgradeUrl":"https://smoo.ai/apps/start/product?plan=Family%20plan&term=monthly&org=o"}"#,
        )])
        .await;
        let out = mint_or_rotate(&reqwest::Client::new(), &base, "jwt", "o", "big-smooth-x").await.unwrap();
        assert_eq!(
            out,
            MintOutcome::PlanRequired {
                upgrade_url: "https://smoo.ai/apps/start/product?plan=Family%20plan&term=monthly&org=o".into(),
                message: "Family plan required".into()
            }
        );
    }

    #[tokio::test]
    async fn existing_key_is_rotated_and_rotation_can_402_too() {
        let (base, seen) = mock(vec![(409, r#"{"message":"exists"}"#), (200, r#"{"key":"sk-rot"}"#)]).await;
        let out = mint_or_rotate(&reqwest::Client::new(), &base, "jwt", "o", "big-smooth-x").await.unwrap();
        assert_eq!(out, MintOutcome::Minted("sk-rot".into()));
        assert!(seen.lock().unwrap()[1].starts_with("POST /organizations/o/llm-gateway/keys/big-smooth-x/rotate HTTP"));

        let (base, _) = mock(vec![(409, "{}"), (402, r#"{"error":"family_plan_required"}"#)]).await;
        let out = mint_or_rotate(&reqwest::Client::new(), &base, "jwt", "o", "big-smooth-x").await.unwrap();
        assert!(matches!(out, MintOutcome::PlanRequired { .. }));
    }

    #[tokio::test]
    async fn other_failures_are_errors_with_a_next_step() {
        let (base, _) = mock(vec![(401, "{}")]).await;
        let e = mint_or_rotate(&reqwest::Client::new(), &base, "jwt", "o", "n").await.unwrap_err().to_string();
        assert!(e.contains("smoo auth login"), "{e}");
        let (base, _) = mock(vec![(403, r#"{"message":"admin only"}"#)]).await;
        let e = mint_or_rotate(&reqwest::Client::new(), &base, "jwt", "o", "n").await.unwrap_err().to_string();
        assert!(e.contains("org admin"), "{e}");
        let (base, _) = mock(vec![(201, r#"{"key":""}"#)]).await;
        assert!(
            mint_or_rotate(&reqwest::Client::new(), &base, "jwt", "o", "n").await.is_err(),
            "an empty key is not a key"
        );
    }

    #[test]
    fn save_gateway_key_registers_without_clobbering_and_routes_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        save_gateway_key(&p, "sk-1").unwrap();
        let reg = smooth_operator::providers::ProviderRegistry::load_from_file(&p).unwrap();
        assert_eq!(reg.list_providers(), vec!["smooai-gateway"]);
        let cfg = reg.default_llm_config().unwrap();
        assert!(cfg.api_url.contains("llm.smoo.ai"), "{}", cfg.api_url);
        assert_eq!(cfg.api_key, "sk-1");
        assert_eq!(cfg.model, SMOO_DEFAULT_MODEL);
        assert!(crate::providers::route_at(&p, "coding").is_some(), "the daemon sees it as configured");
        let mut reg = reg;
        reg.register_provider(smooth_operator::providers::ProviderConfig::ollama());
        reg.save_to_file(&p).unwrap();
        save_gateway_key(&p, " sk-2 ").unwrap();
        let reg = smooth_operator::providers::ProviderRegistry::load_from_file(&p).unwrap();
        let mut ids = reg.list_providers();
        ids.sort_unstable();
        assert_eq!(ids, vec!["ollama", "smooai-gateway"]);
        assert_eq!(reg.get_provider("smooai-gateway").unwrap().api_key, "sk-2");
        assert!(save_gateway_key(&p, "  ").is_err());
    }
}

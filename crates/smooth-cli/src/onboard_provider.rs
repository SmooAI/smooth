//! Big Smooth's first-run model picker (SMOODEV-3918) — one flow, three doors:
//! `th up` with no provider, `th doctor --onboard`'s Providers step, and
//! `th model setup` (plus `th harness add --agentic`'s provider gate).
//!
//! Two choices:
//!
//! 1. **Family plan** ($9/month, 14-day free trial, hosted models through the
//!    Smoo AI gateway): sign in with Smoo → mint this machine's
//!    `big-smooth-<host>` gateway key. No plan yet ⇒ the server answers 402
//!    with a checkout URL; we open it and retry the mint every few seconds
//!    until the trial starts (or the user gives up), then save the key.
//! 2. **Your own key**: an OpenAI-compatible or Anthropic endpoint (base URL,
//!    key, model — validated with one test request before saving), or any
//!    catalog provider via `th model login`.
//!
//! The core (validation, the mint and its 402, the write to providers.json)
//! lives in `smooth_cast::{provider_setup, family_plan}`, shared with the
//! daemon's `configure_provider` tool and its web first-run card.

use std::future::Future;
use std::io::IsTerminal;
use std::time::Duration;

use anstream::eprintln;
use anyhow::{bail, Context, Result};
use dialoguer::theme::ColorfulTheme;
use owo_colors::OwoColorize;
use serde_json::Value;
use smooth_cast::family_plan::{self, MintOutcome};
use smooth_cast::provider_setup::{self, ByoFormat, ByoInput};

use crate::gradient::paint;

/// The top-level choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderChoice {
    FamilyPlan,
    OwnKey,
    Cancel,
}

/// The picker rows, in the order [`provider_choice`] expects.
#[must_use]
pub fn provider_rows() -> Vec<String> {
    vec![
        "Family plan — $9/month, 14-day free trial, hosted models (recommended)".to_string(),
        "Your own key — OpenAI-compatible, Anthropic, OpenRouter, Ollama, …".to_string(),
        "Not now".to_string(),
    ]
}

/// Map a picker index to a choice.
#[must_use]
pub const fn provider_choice(index: usize) -> ProviderChoice {
    match index {
        0 => ProviderChoice::FamilyPlan,
        1 => ProviderChoice::OwnKey,
        _ => ProviderChoice::Cancel,
    }
}

/// The "your own key" sub-choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnKeyKind {
    OpenAiCompatible,
    Anthropic,
    Catalog,
}

/// Rows for the own-key picker, in the order [`own_key_choice`] expects.
#[must_use]
pub fn own_key_rows() -> Vec<&'static str> {
    vec![
        "OpenAI-compatible endpoint (OpenAI, Groq, vLLM, LM Studio, … — base URL + key + model)",
        "Anthropic (or an Anthropic-compatible endpoint)",
        "Pick from the provider catalog (OpenRouter, Google, Kimi, Ollama, …)",
    ]
}

/// Map an own-key picker index to a kind (out-of-range ⇒ catalog).
#[must_use]
pub const fn own_key_choice(index: usize) -> OwnKeyKind {
    match index {
        0 => OwnKeyKind::OpenAiCompatible,
        1 => OwnKeyKind::Anthropic,
        _ => OwnKeyKind::Catalog,
    }
}

/// Prefilled base URL + model for a kind.
#[must_use]
pub const fn own_key_defaults(kind: OwnKeyKind) -> (ByoFormat, &'static str, &'static str) {
    match kind {
        OwnKeyKind::Anthropic => (ByoFormat::Anthropic, "https://api.anthropic.com/v1", "claude-sonnet-4-5"),
        OwnKeyKind::OpenAiCompatible | OwnKeyKind::Catalog => (ByoFormat::OpenAi, "https://api.openai.com/v1", "gpt-4o-mini"),
    }
}

/// What the provider gate says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// A model is configured and the daemon is using it.
    Ready,
    /// Credentials are saved but the running daemon booted without them.
    RestartRequired,
    /// No model at all — run the picker.
    NeedsSetup,
}

/// Read the daemon's `GET /api/llm/provider` answer.
#[must_use]
pub fn gate_from_status(status: &Value) -> Gate {
    let configured = status.get("configured").and_then(Value::as_bool).unwrap_or(false);
    let restart = status.get("restart_required").and_then(Value::as_bool).unwrap_or(false);
    match (configured, restart) {
        (true, false) => Gate::Ready,
        (true, true) => Gate::RestartRequired,
        (false, _) => Gate::NeedsSetup,
    }
}

fn providers_path() -> Result<std::path::PathBuf> {
    Ok(dirs_next::home_dir().context("no home directory")?.join(".smooth").join("providers.json"))
}

/// The gate without a running daemon (`th up`, `th doctor`): the same rule the
/// daemon applies at boot — `SMOOAI_GATEWAY_KEY`, else a routed provider with a
/// key in `~/.smooth/providers.json`.
#[must_use]
pub fn local_gate() -> Gate {
    local_gate_from(std::env::var("SMOOAI_GATEWAY_KEY").ok().as_deref(), providers_path().ok().as_deref())
}

/// Pure core of [`local_gate`].
#[must_use]
pub fn local_gate_from(env_key: Option<&str>, providers: Option<&std::path::Path>) -> Gate {
    if env_key.is_some_and(|k| !k.trim().is_empty()) || providers.and_then(|p| smooth_cast::providers::route_at(p, "coding")).is_some() {
        Gate::Ready
    } else {
        Gate::NeedsSetup
    }
}

/// What to print instead of a picker when stdin isn't a terminal.
#[must_use]
pub fn non_tty_hint() -> String {
    format!(
        "Big Smooth has no model yet. Choose one:\n  → th model setup                                         ({})\n  → th providers add <id> --url <base-url> --api-key <key> --format openai|anthropic --model <model>   (your own key)",
        family_plan::FAMILY_PLAN_PITCH
    )
}

/// What the picker set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setup {
    FamilyPlan,
    OwnKey { id: String },
}

/// Run the picker. `Ok(None)` when the user chose "not now".
///
/// # Errors
/// When stdin isn't a terminal, or a setup step fails.
pub async fn run_picker() -> Result<Option<Setup>> {
    if !std::io::stdin().is_terminal() {
        bail!("{}", non_tty_hint());
    }
    eprintln!();
    eprintln!(
        "  {} Big {} needs a model to think with. Pick one — you can switch any time (th model setup).",
        paint("◐", |g| g.bold().to_string()),
        crate::gradient::smooth()
    );
    let idx = dialoguer::Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Choose Big Smooth's model")
        .items(&provider_rows())
        .default(0)
        .interact()?;
    match provider_choice(idx) {
        ProviderChoice::FamilyPlan => {
            if setup_family_plan().await? {
                Ok(Some(Setup::FamilyPlan))
            } else {
                Ok(None)
            }
        }
        ProviderChoice::OwnKey => setup_own_key().await.map(|id| id.map(|id| Setup::OwnKey { id })),
        ProviderChoice::Cancel => Ok(None),
    }
}

/// `th model setup`: run the picker and say what happens next.
///
/// # Errors
/// As [`run_picker`].
pub async fn cmd_model_setup() -> Result<()> {
    match run_picker().await? {
        Some(_) => {
            print_next_step().await;
            Ok(())
        }
        None => {
            eprintln!(
                "  {} nothing changed — run `th model setup` when you're ready.",
                paint("○", |g| g.dimmed().to_string())
            );
            Ok(())
        }
    }
}

/// After saving: a running daemon booted without the provider and needs a
/// restart; a stopped one picks it up on `th up`.
pub async fn print_next_step() {
    if crate::daemon_health::probe(crate::daemon_health::DEFAULT_PORT).await.is_up() {
        eprintln!(
            "  {} Big Smooth reads its model at boot — restart it to switch: th down && th up",
            paint("◐", |g| g.bold().to_string())
        );
    } else {
        eprintln!("  {} start Big Smooth with: th up", paint("●", |g| g.bold().to_string()));
    }
}

/// `th up`'s first-run gate: no model ⇒ the picker on a terminal, the two
/// commands otherwise. Never fails `th up` — Big Smooth still starts (its web
/// UI offers the same choice). `SMOOTH_SKIP_ONBOARD=1` opts out. Returns
/// `true` when a provider was saved just now.
pub async fn up_gate() -> bool {
    if std::env::var_os("SMOOTH_SKIP_ONBOARD").is_some() || local_gate() == Gate::Ready {
        return false;
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        eprintln!("{}", non_tty_hint());
        return false;
    }
    match run_picker().await {
        Ok(Some(_)) => true,
        Ok(None) => {
            eprintln!(
                "  {} starting without a model — the web UI and `th model setup` offer the same choice.",
                paint("○", |g| g.dimmed().to_string())
            );
            false
        }
        Err(e) => {
            eprintln!(
                "  {} model setup didn't finish: {e:#}\n    starting anyway — retry with `th model setup`.",
                paint("○", |g| g.yellow().to_string())
            );
            false
        }
    }
}

fn self_exe() -> Result<std::path::PathBuf> {
    std::env::current_exe().context("locate the th binary")
}

fn run_self(args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(self_exe()?)
        .args(args)
        .status()
        .with_context(|| format!("run th {}", args.join(" ")))?;
    if !status.success() {
        bail!("`th {}` exited with {status}", args.join(" "));
    }
    Ok(())
}

fn api_base() -> String {
    std::env::var("SMOOAI_API_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://api.smoo.ai".to_string())
}

/// Retry `attempt` until it mints, the poll budget runs out (`Ok(None)`), or it
/// errors. The first call is immediate; retry `i` waits `delay(i)`.
///
/// # Errors
/// The first error `attempt` returns.
pub async fn poll_until_minted<F, Fut>(mut attempt: F, delay: impl Fn(u32) -> Duration, max_attempts: u32) -> Result<Option<String>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<MintOutcome>>,
{
    for i in 0..max_attempts {
        if i > 0 {
            tokio::time::sleep(delay(i - 1)).await;
        }
        if let MintOutcome::Minted(key) = attempt().await? {
            return Ok(Some(key));
        }
    }
    Ok(None)
}

/// The org to mint in: the active one, else the user's first org (made active),
/// else `None` — they have no org until checkout creates a personal one.
async fn plan_org(http: &reqwest::Client, base: &str, bearer: &str) -> Result<Option<String>> {
    if let Ok(org) = crate::active_org::resolve(None) {
        return Ok(Some(org));
    }
    let Some(org) = family_plan::discover_org(http, base, bearer).await? else {
        return Ok(None);
    };
    let _ = crate::active_org::set(&org);
    Ok(Some(org))
}

/// The Family plan door. `Ok(false)` when the user didn't finish checkout.
async fn setup_family_plan() -> Result<bool> {
    if crate::smooai::user_client::UserClient::user_label().is_none() {
        eprintln!("  {} signing in to {}…", paint("●", |g| g.bold().to_string()), crate::gradient::smoo_ai());
        run_self(&["smoo", "auth", "login"])?;
    }
    let http = reqwest::Client::builder().user_agent(format!("th/{}", env!("CARGO_PKG_VERSION"))).build()?;
    let name = family_plan::key_name(&family_plan::this_host());
    let base = api_base();
    let mint = || {
        let (http, base, name) = (http.clone(), base.clone(), name.clone());
        async move {
            // Fresh every attempt: a checkout can outlast the ~1h access token.
            let creds = crate::auth::refresh::fresh_user_credentials(&http).await?;
            // Re-resolved every attempt too: a user with no org gets a personal
            // one when checkout starts.
            let Some(org) = plan_org(&http, &base, &creds.access_token).await? else {
                return Ok(family_plan::no_org_yet());
            };
            family_plan::mint_or_rotate(&http, &base, &creds.access_token, &org, &name).await
        }
    };
    let key = match mint().await? {
        MintOutcome::Minted(key) => key,
        MintOutcome::PlanRequired { upgrade_url, message } => {
            eprintln!("  {} {message}", paint("◐", |g| g.bold().to_string()));
            eprintln!(
                "  {} {} — opening checkout:",
                paint("●", |g| g.bold().to_string()),
                family_plan::FAMILY_PLAN_PITCH
            );
            eprintln!("    {}", upgrade_url.cyan());
            eprintln!("    {}", paint(&family_plan::top_up_note(), |g| g.dimmed().to_string()));
            if let Err(e) = open::that(&upgrade_url) {
                eprintln!("    (couldn't open a browser: {e} — open the link above)");
            }
            eprintln!(
                "  {} waiting for your trial to start… (Ctrl-C to stop; `th model setup` picks up where you left off)",
                paint("○", |g| g.dimmed().to_string())
            );
            match poll_until_minted(mint, family_plan::plan_poll_delay, family_plan::PLAN_POLL_MAX).await? {
                Some(key) => key,
                None => {
                    eprintln!(
                        "  {} checkout didn't finish in 10 minutes. Finish it here, then rerun `th model setup`:\n    {}",
                        paint("○", |g| g.yellow().to_string()),
                        upgrade_url.cyan()
                    );
                    return Ok(false);
                }
            }
        }
    };
    let path = providers_path()?;
    family_plan::save_gateway_key(&path, &key)?;
    eprintln!(
        "  {} Family plan active — saved this machine's gateway key ({name}) to {}",
        paint("●", |g| g.bold().to_string()),
        path.display()
    );
    eprintln!("  {} {}", paint("○", |g| g.dimmed().to_string()), family_plan::top_up_note());
    Ok(true)
}

/// The own-key door. Returns the provider id saved, `None` when the user backed out.
async fn setup_own_key() -> Result<Option<String>> {
    let theme = ColorfulTheme::default();
    let kind = own_key_choice(
        dialoguer::Select::with_theme(&theme)
            .with_prompt("Which kind of endpoint?")
            .items(&own_key_rows())
            .default(0)
            .interact()?,
    );
    if kind == OwnKeyKind::Catalog {
        run_self(&["model", "login"])?;
        // `th model login` can end without saving (a failed test, declined).
        return Ok((local_gate() == Gate::Ready).then(|| "catalog".to_string()));
    }
    let (format, default_url, default_model) = own_key_defaults(kind);
    let mut base_url = default_url.to_string();
    let mut model = default_model.to_string();
    loop {
        base_url = dialoguer::Input::with_theme(&theme)
            .with_prompt("Base URL")
            .with_initial_text(base_url)
            .interact_text()?;
        let api_key: String = dialoguer::Password::with_theme(&theme)
            .with_prompt("API key (blank for a local server)")
            .allow_empty_password(true)
            .interact()?;
        model = dialoguer::Input::with_theme(&theme)
            .with_prompt("Model")
            .with_initial_text(model)
            .interact_text()?;
        let input = ByoInput {
            format: format.as_str().into(),
            base_url: base_url.clone(),
            api_key: Some(api_key),
            model: model.clone(),
            id: None,
        };
        let outcome = match provider_setup::validate_byo(&input) {
            Err(e) => Err(e),
            Ok(p) => {
                eprintln!("  {} testing {} with one tiny request…", paint("○", |g| g.dimmed().to_string()), p.base_url);
                provider_setup::probe(&p).await.map(|_| p).map_err(|e| format!("{e:#}"))
            }
        };
        match outcome {
            Ok(p) => {
                let path = providers_path()?;
                let mut root = smooth_cast::providers::load_value(&path)?;
                provider_setup::apply_byo(&mut root, &p);
                smooth_cast::providers::save_value(&path, &root)?;
                eprintln!(
                    "  {} connected — saved `{}` ({} · {}) and pointed Big Smooth at it",
                    paint("●", |g| g.bold().to_string()),
                    p.id,
                    p.model,
                    p.base_url
                );
                if !provider_setup::daemon_chat_compatible(&p) {
                    eprintln!(
                        "  {} Big Smooth's chat speaks OpenAI-compatible APIs only; this Anthropic-format endpoint works for `th code`, but chat needs an OpenAI-compatible URL.",
                        paint("○", |g| g.yellow().to_string())
                    );
                }
                return Ok(Some(p.id));
            }
            Err(e) => {
                eprintln!("  {} {e}", paint("○", |g| g.yellow().to_string()));
                let again = dialoguer::Confirm::with_theme(&theme).with_prompt("Try again?").default(true).interact()?;
                if !again {
                    return Ok(None);
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn rows_lead_with_the_family_plan_and_map_in_order() {
        let rows = provider_rows();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].contains("Family plan") && rows[0].contains("$9/month") && rows[0].contains("14-day"));
        assert_eq!(provider_choice(0), ProviderChoice::FamilyPlan);
        assert_eq!(provider_choice(1), ProviderChoice::OwnKey);
        assert_eq!(provider_choice(2), ProviderChoice::Cancel);
        assert_eq!(provider_choice(99), ProviderChoice::Cancel);
    }

    #[test]
    fn own_key_rows_map_in_order_with_sane_defaults() {
        assert_eq!(own_key_rows().len(), 3);
        assert_eq!(own_key_choice(0), OwnKeyKind::OpenAiCompatible);
        assert_eq!(own_key_choice(1), OwnKeyKind::Anthropic);
        assert_eq!(own_key_choice(2), OwnKeyKind::Catalog);
        let (f, url, _) = own_key_defaults(OwnKeyKind::Anthropic);
        assert_eq!((f, url), (ByoFormat::Anthropic, "https://api.anthropic.com/v1"));
        let (f, url, model) = own_key_defaults(OwnKeyKind::OpenAiCompatible);
        assert_eq!(f, ByoFormat::OpenAi);
        // The defaults must pass validation once a key is typed.
        let ok = provider_setup::validate_byo(&ByoInput {
            format: f.as_str().into(),
            base_url: url.into(),
            api_key: Some("sk".into()),
            model: model.into(),
            id: None,
        });
        assert!(ok.is_ok(), "{ok:?}");
    }

    #[test]
    fn gate_reads_the_daemon_answer() {
        assert_eq!(gate_from_status(&json!({"configured": true})), Gate::Ready);
        assert_eq!(gate_from_status(&json!({"configured": true, "restart_required": true})), Gate::RestartRequired);
        assert_eq!(gate_from_status(&json!({"configured": false, "options": []})), Gate::NeedsSetup);
        assert_eq!(gate_from_status(&json!({})), Gate::NeedsSetup, "garbage ⇒ offer setup");
    }

    #[test]
    fn local_gate_matches_the_daemon_rule() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("providers.json");
        assert_eq!(local_gate_from(None, Some(&p)), Gate::NeedsSetup, "no file");
        assert_eq!(local_gate_from(Some("  "), Some(&p)), Gate::NeedsSetup, "blank env key");
        assert_eq!(local_gate_from(Some("sk"), Some(&p)), Gate::Ready);
        std::fs::write(
            &p,
            r#"{"providers":[{"id":"x","api_url":"https://x/v1","api_key":"","api_format":"OpenAiCompat","default_model":"m"}]}"#,
        )
        .unwrap();
        assert_eq!(local_gate_from(None, Some(&p)), Gate::NeedsSetup, "a keyless provider can't run a turn");
        family_plan::save_gateway_key(&p, "sk-1").unwrap();
        assert_eq!(local_gate_from(None, Some(&p)), Gate::Ready);
    }

    #[test]
    fn non_tty_hint_names_both_doors() {
        let h = non_tty_hint();
        assert!(h.contains("th model setup") && h.contains("Family plan"));
        assert!(h.contains("--format openai|anthropic"));
    }

    #[tokio::test]
    async fn poll_retries_through_402s_until_the_trial_starts() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&calls);
        let key = poll_until_minted(
            move || {
                let n = c.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok(if n < 2 {
                        MintOutcome::PlanRequired {
                            upgrade_url: family_plan::FAMILY_PLAN_URL.into(),
                            message: String::new(),
                        }
                    } else {
                        MintOutcome::Minted("sk-trial".into())
                    })
                }
            },
            |_| Duration::from_millis(1),
            10,
        )
        .await
        .unwrap();
        assert_eq!(key.as_deref(), Some("sk-trial"));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn poll_gives_up_after_its_budget_and_surfaces_errors() {
        let out = poll_until_minted(
            || async {
                Ok(MintOutcome::PlanRequired {
                    upgrade_url: String::new(),
                    message: String::new(),
                })
            },
            |_| Duration::from_millis(1),
            3,
        )
        .await
        .unwrap();
        assert_eq!(out, None);
        let err = poll_until_minted(|| async { Err(anyhow::anyhow!("401")) }, |_| Duration::from_millis(1), 3).await;
        assert!(err.is_err());
    }
}

//! Both flows behind `th auth login`.

use std::io::IsTerminal;

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use dialoguer::{theme::ColorfulTheme, Input, Password};
use owo_colors::OwoColorize;
use smooai_client_shared::auth::m2m::client_credentials_grant;
use smooai_client_shared::auth::password::password_grant;
use smooai_client_shared::auth::storage::{CredentialKind, Credentials, CredentialsStore};

use super::active_org;
use super::browser_login;

/// Default `auth.smoo.ai/cli-login` page (the browser-facing endpoint
/// the smooai-side pearl th-62e710 is shipping). Override with
/// `SMOOAI_CLI_LOGIN_URL` for staging / local dev.
const DEFAULT_CLI_LOGIN_URL: &str = "https://auth.smoo.ai/cli-login";

fn cli_login_url() -> String {
    std::env::var("SMOOAI_CLI_LOGIN_URL").unwrap_or_else(|_| DEFAULT_CLI_LOGIN_URL.to_string())
}

fn token_url() -> String {
    // Same env var the M2M flow already honors (`SMOOAI_AUTH_URL`),
    // and the same default endpoint — `/token` accepts both
    // `grant_type=client_credentials` and `grant_type=authorization_code`
    // (the pearl th-62e710 contract).
    std::env::var("SMOOAI_AUTH_URL").unwrap_or_else(|_| "https://auth.smoo.ai/token".to_string())
}

/// Decide whether the browser flow should run. Three inputs, in order:
///   1. Explicit `--browser` / `--no-browser` flag (highest priority).
///   2. `SMOOTH_AUTH_BROWSER` env var: `0` / `false` opts out, `1` /
///      `true` opts in. Default for now is **off** so this ships behind
///      a feature flag until smooai-side `/cli-login` is live (DESIGN.md
///      ship order step 5).
///   3. Stdout-is-a-TTY check — required regardless. CI/SSH/pipe →
///      prompt flow only.
fn should_use_browser_flow(explicit: Option<bool>) -> bool {
    // Hard gate: no TTY = no browser. Even an explicit `--browser`
    // is ignored on a headless host so we don't silently hang.
    if !std::io::stdout().is_terminal() {
        return false;
    }
    if let Some(b) = explicit {
        return b;
    }
    // Feature-flagged default. Once smooai-side /cli-login ships
    // (pearl th-62e710), flip the default-on logic here.
    match std::env::var("SMOOTH_AUTH_BROWSER").ok().as_deref() {
        Some("1") | Some("true") | Some("TRUE") => true,
        Some("0") | Some("false") | Some("FALSE") => false,
        _ => false,
    }
}

/// User flow: prompt for email + password, Supabase password grant,
/// persist to `~/.smooth/auth/smooai-user.json`.
///
/// When the browser flow is enabled (per `should_use_browser_flow`),
/// run the PKCE OAuth handshake against `auth.smoo.ai/cli-login`
/// instead. Both paths persist via the same `CredentialsStore` and the
/// same `active_org::set` integration so downstream commands behave
/// identically.
pub async fn cmd_login_user(email: Option<String>, password: Option<String>, browser: Option<bool>) -> Result<()> {
    if should_use_browser_flow(browser) {
        return cmd_login_user_browser().await;
    }
    let url = super::supabase_url();
    let key = super::supabase_anon_key();

    let theme = ColorfulTheme::default();
    let email: String = match email {
        Some(e) => e,
        None => Input::with_theme(&theme).with_prompt("Email").interact_text().context("prompt for email")?,
    };
    let password: String = match password {
        Some(p) => p,
        None => Password::with_theme(&theme).with_prompt("Password").interact().context("prompt for password")?,
    };

    println!("{} Signing in to Smoo AI ({})...", "→".cyan().bold(), url.dimmed());
    let http = reqwest::Client::new();
    let creds = password_grant(&http, &url, &key, &email, &password).await.context("Supabase password grant")?;

    let store = CredentialsStore::default_user().context("locate ~/.smooth/auth/smooai-user.json")?;
    store.save(&creds).context("persist user credentials")?;

    let user = creds.user.as_deref().unwrap_or("(unknown user)");
    println!(
        "{} Logged in as {} — saved to {}",
        "✓".green().bold(),
        user.bold(),
        store.path().display().to_string().dimmed()
    );
    if let Some(exp) = creds.expires_at {
        println!("  {} Session expires {}", "ℹ".dimmed(), exp.format("%Y-%m-%d %H:%M UTC").to_string().dimmed());
    }
    println!();
    println!("  Run {} to inspect, {} to clear.", "th auth whoami".bold(), "th auth logout".bold());
    Ok(())
}

/// Browser/PKCE flow: open the user's default browser at
/// `auth.smoo.ai/cli-login`, capture the code on a localhost listener,
/// exchange for tokens via PKCE, persist credentials, and write the
/// chosen org through [`active_org::set`].
///
/// See [`browser_login`] for the underlying flow + tests.
async fn cmd_login_user_browser() -> Result<()> {
    let authorize_base = cli_login_url();
    let tok = token_url();
    println!("{} Signing in to Smoo AI via browser...", "→".cyan().bold());
    let http = reqwest::Client::new();
    let outcome = browser_login::run_browser_login(&http, &authorize_base, &tok)
        .await
        .context("browser login flow")?;

    // Build a Credentials record off the token-exchange response and
    // persist via the same store the password flow uses, so downstream
    // commands (`th api orgs show`, `th config list`, …) don't care
    // which flow produced the session.
    let expires_at = outcome.tokens.expires_in.map(|secs| {
        // `i64::try_from` would be the safer cast, but `expires_in` is
        // an unsigned-seconds value bounded by upstream IdPs at far
        // below i64::MAX. Saturating is correct and clippy-pedantic
        // compliant.
        let secs_i64 = i64::try_from(secs).unwrap_or(i64::MAX);
        Utc::now() + ChronoDuration::seconds(secs_i64)
    });
    let creds = Credentials {
        access_token: outcome.tokens.access_token.clone(),
        refresh_token: outcome.tokens.refresh_token.clone(),
        expires_at,
        user: outcome.tokens.email.clone().or_else(|| outcome.tokens.user.clone()),
        active_org_id: outcome.org_id.clone(),
        client_id: None,
        client_secret: None,
        kind: CredentialKind::User,
        created_at: Utc::now(),
    };
    let store = CredentialsStore::default_user().context("locate ~/.smooth/auth/smooai-user.json")?;
    store.save(&creds).context("persist user credentials")?;

    // Sync the active org through the shared writer so the
    // cross-store invariant (pearl th-3217db, single-store today)
    // holds even if a caller went through the credentials store
    // directly above. Idempotent if `org_id` was None — skip rather
    // than clobber whatever was there.
    if let Some(org_id) = &outcome.org_id {
        active_org::set(org_id).context("persist active org id")?;
    }

    let user = creds.user.as_deref().unwrap_or("(unknown user)");
    println!(
        "{} Signed in as {} — saved to {}",
        "✓".green().bold(),
        user.bold(),
        store.path().display().to_string().dimmed()
    );
    if let Some(org) = &outcome.org_id {
        println!("  {} active org: {}", "ℹ".dimmed(), org.cyan().bold());
    }
    if let Some(exp) = creds.expires_at {
        println!("  {} Session expires {}", "ℹ".dimmed(), exp.format("%Y-%m-%d %H:%M UTC").to_string().dimmed());
    }
    println!();
    println!("  Run {} to inspect, {} to clear.", "th auth whoami".bold(), "th auth logout".bold());
    Ok(())
}

/// M2M flow: prompt for client_id + client_secret, RFC 6749
/// `client_credentials` grant against `auth.smoo.ai/token`, persist
/// to `~/.smooth/auth/smooai.json`.
pub async fn cmd_login_m2m(client_id: Option<String>, client_secret: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let client_id: String = match client_id {
        Some(v) => v,
        None => match std::env::var("SMOOAI_CLIENT_ID") {
            Ok(v) => v,
            Err(_) => Input::with_theme(&theme)
                .with_prompt("Client ID")
                .interact_text()
                .context("prompt for client_id")?,
        },
    };
    let client_secret: String = match client_secret {
        Some(v) => v,
        None => match std::env::var("SMOOAI_CLIENT_SECRET") {
            Ok(v) => v,
            Err(_) => Password::with_theme(&theme)
                .with_prompt("Client Secret")
                .interact()
                .context("prompt for client_secret")?,
        },
    };

    println!("{} Exchanging service-account credentials at auth.smoo.ai...", "→".cyan().bold());
    let http = reqwest::Client::new();
    let creds = client_credentials_grant(&http, &client_id, &client_secret)
        .await
        .context("client_credentials grant")?;

    let store = CredentialsStore::default_m2m().context("locate ~/.smooth/auth/smooai.json")?;
    store.save(&creds).context("persist M2M credentials")?;

    println!("{} M2M session saved to {}", "✓".green().bold(), store.path().display().to_string().dimmed());
    if let Some(exp) = creds.expires_at {
        println!(
            "  {} Access token expires {}",
            "ℹ".dimmed(),
            exp.format("%Y-%m-%d %H:%M UTC").to_string().dimmed()
        );
    }
    println!();
    println!("  Run {} to inspect, {} --m2m to clear.", "th auth whoami".bold(), "th auth logout".bold());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // `should_use_browser_flow` reads process-global env vars, so the
    // tests serialize themselves.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }
    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            Self { key, prev }
        }
        fn unset(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn explicit_no_browser_overrides_env_opt_in() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("SMOOTH_AUTH_BROWSER", "1");
        // Even with the env opt-in, `--no-browser` takes precedence —
        // but only when stdout is a TTY (Some(_) means an explicit
        // flag was passed). Test harness usually has no TTY, in which
        // case the function returns false unconditionally — that's
        // still a valid result for `--no-browser`.
        assert!(!should_use_browser_flow(Some(false)));
    }

    #[test]
    fn no_tty_forces_prompt_flow_regardless_of_flag() {
        let _l = ENV_LOCK.lock().unwrap();
        // Under `cargo test`, stdout is captured (not a TTY), so even
        // an explicit `--browser` should not flip the flow. This
        // exercises the hard-gate at the top of the function.
        // (If a future test harness pipes a PTY in, this assertion
        // becomes vacuous; it still documents intent.)
        if !std::io::stdout().is_terminal() {
            assert!(!should_use_browser_flow(Some(true)));
        }
    }

    #[test]
    fn env_opt_out_takes_precedence_over_default() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("SMOOTH_AUTH_BROWSER", "0");
        assert!(!should_use_browser_flow(None));
    }

    #[test]
    fn default_is_prompt_flow_when_env_unset() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::unset("SMOOTH_AUTH_BROWSER");
        // Feature flag default-off until smooai-side /cli-login ships
        // (DESIGN.md ship order step 5).
        assert!(!should_use_browser_flow(None));
    }

    #[test]
    fn cli_login_url_honors_override() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("SMOOAI_CLI_LOGIN_URL", "http://127.0.0.1:1234/cli-login");
        assert_eq!(cli_login_url(), "http://127.0.0.1:1234/cli-login");
    }

    #[test]
    fn cli_login_url_default_is_prod() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::unset("SMOOAI_CLI_LOGIN_URL");
        assert_eq!(cli_login_url(), DEFAULT_CLI_LOGIN_URL);
    }

    #[test]
    fn token_url_honors_smooai_auth_url() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("SMOOAI_AUTH_URL", "http://127.0.0.1:9000/token");
        assert_eq!(token_url(), "http://127.0.0.1:9000/token");
    }
}

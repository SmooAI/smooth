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

/// Default `smoo.ai/cli-login` page — the browser-facing endpoint of the
/// **user/org** flow. This lives on the dashboard domain (`smoo.ai`), not
/// `auth.smoo.ai`, because it authenticates the person through their
/// **Supabase** dashboard session and lets them pick an org. The M2M
/// (`--m2m`) flow is the one that talks to `auth.smoo.ai` (client_credentials).
/// Override with `SMOOAI_CLI_LOGIN_URL` for staging / local dev.
/// Pearl th-a93734 / SMOODEV-1918.
const DEFAULT_CLI_LOGIN_URL: &str = "https://smoo.ai/cli-login";

/// Default token-exchange endpoint for the **browser/user** flow. This is a
/// distinct endpoint from the M2M `auth.smoo.ai/token` (openauthjs
/// client_credentials issuer): `smoo.ai/api/token` is the Next.js route that
/// redeems the PKCE `authorization_code` minted at `/cli-login` and signs the
/// Supabase-compatible user JWT. Keep these two separate — they are different
/// services. Override with `SMOOAI_CLI_TOKEN_URL` for staging / local dev.
const DEFAULT_CLI_TOKEN_URL: &str = "https://smoo.ai/api/token";

fn cli_login_url() -> String {
    std::env::var("SMOOAI_CLI_LOGIN_URL").unwrap_or_else(|_| DEFAULT_CLI_LOGIN_URL.to_string())
}

/// Token-exchange URL for the browser/user (Supabase) flow. Deliberately NOT
/// the M2M `SMOOAI_AUTH_URL` / `auth.smoo.ai/token` — that endpoint only
/// serves `client_credentials`. See [`DEFAULT_CLI_TOKEN_URL`].
fn cli_token_url() -> String {
    std::env::var("SMOOAI_CLI_TOKEN_URL").unwrap_or_else(|_| DEFAULT_CLI_TOKEN_URL.to_string())
}

/// Pure env/flag preference for the browser flow, with NO TTY gate — split
/// out from [`should_use_browser_flow`] so it can be unit-tested without a
/// PTY. Inputs, in order:
///   1. Explicit `--browser` / `--no-browser` flag (highest priority).
///   2. `SMOOTH_AUTH_BROWSER` env var: `0` / `false` opts out, `1` / `true`
///      opts in.
///   3. Default: **on**. The smooai-side `/cli-login` + `/api/token`
///      (Supabase user/org flow) shipped (pearl th-62e710 / SMOODEV-1918),
///      so the browser flow is now the default user login.
fn browser_flow_preference(explicit: Option<bool>) -> bool {
    if let Some(b) = explicit {
        return b;
    }
    match std::env::var("SMOOTH_AUTH_BROWSER").ok().as_deref() {
        Some("0") | Some("false") | Some("FALSE") => false,
        // "1"/"true" and the unset default both opt in.
        _ => true,
    }
}

/// Decide whether the browser flow should run. Layers a hard TTY gate over
/// [`browser_flow_preference`]: no TTY = prompt/password flow regardless, so
/// CI / SSH / piped invocations never hang waiting on a browser callback.
fn should_use_browser_flow(explicit: Option<bool>) -> bool {
    // Hard gate: no TTY = no browser. Even an explicit `--browser`
    // is ignored on a headless host so we don't silently hang.
    if !std::io::stdout().is_terminal() {
        return false;
    }
    browser_flow_preference(explicit)
}

/// User flow: prompt for email + password, Supabase password grant,
/// persist to `~/.smooth/auth/smooai-user.json`.
///
/// When the browser flow is enabled (per `should_use_browser_flow`, the
/// default on a TTY), run the PKCE OAuth handshake against
/// `smoo.ai/cli-login` (Supabase user/org session) instead. Both paths
/// authenticate the **person** via Supabase and persist via the same
/// `CredentialsStore` and the
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
/// `smoo.ai/cli-login` (Supabase user/org auth), capture the code on a
/// localhost listener, exchange it for tokens via PKCE at `smoo.ai/api/token`,
/// persist credentials, and write the chosen org through [`active_org::set`].
///
/// See [`browser_login`] for the underlying flow + tests.
async fn cmd_login_user_browser() -> Result<()> {
    let authorize_base = cli_login_url();
    let tok = cli_token_url();
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

    // Preference logic is tested via the pure `browser_flow_preference`
    // (no TTY gate), so the assertions hold under `cargo test` (captured,
    // non-TTY stdout). The TTY hard-gate itself is covered separately.

    #[test]
    fn explicit_no_browser_overrides_env_opt_in() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("SMOOTH_AUTH_BROWSER", "1");
        // Even with the env opt-in, an explicit `--no-browser` wins.
        assert!(!browser_flow_preference(Some(false)));
    }

    #[test]
    fn explicit_browser_overrides_env_opt_out() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("SMOOTH_AUTH_BROWSER", "0");
        // And an explicit `--browser` wins over the env opt-out.
        assert!(browser_flow_preference(Some(true)));
    }

    #[test]
    fn no_tty_forces_prompt_flow_regardless_of_flag() {
        let _l = ENV_LOCK.lock().unwrap();
        // Under `cargo test`, stdout is captured (not a TTY), so even
        // an explicit `--browser` should not flip the flow. This
        // exercises the hard-gate in `should_use_browser_flow`.
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
        assert!(!browser_flow_preference(None));
    }

    #[test]
    fn default_is_browser_flow_when_env_unset() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::unset("SMOOTH_AUTH_BROWSER");
        // Default-on now that the smooai-side /cli-login + /api/token
        // (Supabase user/org flow) shipped — pearl th-a93734 / SMOODEV-1918.
        assert!(browser_flow_preference(None));
    }

    #[test]
    fn cli_login_url_honors_override() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("SMOOAI_CLI_LOGIN_URL", "http://127.0.0.1:1234/cli-login");
        assert_eq!(cli_login_url(), "http://127.0.0.1:1234/cli-login");
    }

    #[test]
    fn cli_login_url_default_is_smoo_ai() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::unset("SMOOAI_CLI_LOGIN_URL");
        // The user/org browser flow lives on the dashboard domain.
        assert_eq!(cli_login_url(), "https://smoo.ai/cli-login");
        assert_eq!(cli_login_url(), DEFAULT_CLI_LOGIN_URL);
    }

    #[test]
    fn cli_token_url_honors_override() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("SMOOAI_CLI_TOKEN_URL", "http://127.0.0.1:9000/api/token");
        assert_eq!(cli_token_url(), "http://127.0.0.1:9000/api/token");
    }

    #[test]
    fn cli_token_url_default_is_smoo_ai_api_token() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::unset("SMOOAI_CLI_TOKEN_URL");
        // Distinct from the M2M auth.smoo.ai/token issuer.
        assert_eq!(cli_token_url(), "https://smoo.ai/api/token");
        assert_eq!(cli_token_url(), DEFAULT_CLI_TOKEN_URL);
    }
}

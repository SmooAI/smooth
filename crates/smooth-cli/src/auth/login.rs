//! Both flows behind `th auth login`.

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Input, Password};
use owo_colors::OwoColorize;
use smooai_client_shared::auth::m2m::client_credentials_grant;
use smooai_client_shared::auth::password::password_grant;
use smooai_client_shared::auth::storage::CredentialsStore;

/// User flow: prompt for email + password, Supabase password grant,
/// persist to `~/.smooth/auth/smooai-user.json`.
pub async fn cmd_login_user(email: Option<String>, password: Option<String>) -> Result<()> {
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

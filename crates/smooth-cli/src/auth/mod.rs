//! `th auth` — Smoo AI identity (user + service account).
//!
//! Two flows, one entry point:
//!
//! - **user (default)**: Supabase email + password against
//!   `db.smoo.ai`. Returns a Supabase JWT used by `th admin *`,
//!   user-attributed `th api *` calls, and (soon) the user-scoped
//!   `llm.smoo.ai` LLM session exchange. Session lives at
//!   `~/.smooth/auth/smooai-user.json`.
//!
//! - **M2M (`--m2m`)**: RFC 6749 `client_credentials` grant against
//!   `auth.smoo.ai/token`. For service accounts (CI, customer-website
//!   SSR, etc.). Session lives at `~/.smooth/auth/smooai.json`.
//!
//! A single host can carry both simultaneously — distinct files,
//! `th api *` resolves which to use per request (user first, M2M
//! fallback).
//!
//! Replaces the v1 `th admin login` (added 2026-05, never released)
//! and supersedes `th api login` (which becomes a deprecation alias).

use anyhow::Result;
use clap::Subcommand;

pub mod active_org;
pub mod browser_login;
pub mod login;
pub mod logout;
pub mod paths;
pub mod pkce;
pub mod profile;
pub mod whoami;

/// Default prod Smoo Supabase project URL. Override with
/// `SMOOAI_SUPABASE_URL` for staging / local dev.
pub const PROD_SUPABASE_URL: &str = "https://db.smoo.ai";

/// Default prod Smoo Supabase **anon** key. This is the public
/// publishable key — safe to embed in the binary distribution
/// (it's the same key every customer-website Next.js bundle ships
/// in its client-side JS). The matching service-role key is NEVER
/// touched by the CLI.
pub const PROD_SUPABASE_ANON_KEY: &str =
    "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6InhycWJxZ290Z2hpdGNmdW91a2RrIiwicm9sZSI6ImFub24iLCJpYXQiOjE3NDEwNDEyODksImV4cCI6MjA1NjYxNzI4OX0.KHwbyjdrBhCiP6Na8aY8b3fA6RNkCqJ4m-dmY4AOdmw";

#[derive(Debug, Subcommand)]
pub enum AuthCommands {
    /// Log in to Smoo AI. Defaults to user (email + password); pass
    /// `--m2m` to authenticate as a service account via
    /// `client_credentials` grant.
    Login {
        /// Switch to M2M `client_credentials` (service account)
        /// instead of the user email+password flow.
        #[arg(long)]
        m2m: bool,

        // ── User flow ────────────────────────────────
        /// Email address (user flow only). Prompted interactively
        /// if omitted.
        #[arg(long, conflicts_with = "client_id", conflicts_with = "client_secret")]
        email: Option<String>,
        /// Password (user flow only). Prompted interactively
        /// (without echo) if omitted. Avoid passing on the command
        /// line in interactive shells — it lands in shell history.
        #[arg(long, conflicts_with = "client_id", conflicts_with = "client_secret")]
        password: Option<String>,
        /// Open the browser for the OAuth2 + PKCE flow against
        /// `auth.smoo.ai/cli-login`. Pearl th-fcb579. Default is the
        /// prompt-based password flow until smooai-side `/cli-login`
        /// (pearl th-62e710) ships and `SMOOTH_AUTH_BROWSER=1` is
        /// removed as a gate.
        #[arg(
            long,
            conflicts_with = "no_browser",
            conflicts_with = "m2m",
            conflicts_with = "email",
            conflicts_with = "password"
        )]
        browser: bool,
        /// Force the prompt-based password flow even when the env
        /// gate (`SMOOTH_AUTH_BROWSER=1`) is set.
        #[arg(long = "no-browser", conflicts_with = "browser")]
        no_browser: bool,

        // ── M2M flow ─────────────────────────────────
        /// Service-account client_id (M2M flow only — implies --m2m).
        /// Prompted interactively if omitted.
        #[arg(long)]
        client_id: Option<String>,
        /// Service-account client_secret (M2M flow only — implies
        /// --m2m). Prompted interactively (without echo) if omitted.
        #[arg(long)]
        client_secret: Option<String>,
    },
    /// Clear stored session(s). By default clears the user session;
    /// pass `--m2m` to clear the M2M session instead, `--all` to
    /// clear both.
    Logout {
        /// Clear the M2M session at ~/.smooth/auth/smooai.json
        /// instead of the user session.
        #[arg(long, conflicts_with = "all")]
        m2m: bool,
        /// Clear both user and M2M sessions.
        #[arg(long)]
        all: bool,
    },
    /// Show currently-logged-in sessions (user + M2M).
    Whoami,
    /// Manage named auth profiles. Each profile bundles a user + M2M
    /// session so one host can hold several identities at once. Select a
    /// profile per command with `--profile <name>` / `SMOOAI_PROFILE`, or
    /// set a persistent default with `th auth profile use <name>`.
    Profile {
        #[command(subcommand)]
        cmd: ProfileCommands,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommands {
    /// List profiles and show which is active.
    List,
    /// Set the active profile (persisted in `<auth>/active`).
    Use {
        /// Profile name.
        name: String,
    },
    /// Delete a profile's stored sessions.
    Rm {
        /// Profile name.
        name: String,
    },
}

pub async fn dispatch(cmd: AuthCommands) -> Result<()> {
    match cmd {
        AuthCommands::Login {
            m2m,
            email,
            password,
            browser,
            no_browser,
            client_id,
            client_secret,
        } => {
            // --client-id / --client-secret implies --m2m even if
            // the flag wasn't passed (saves a keystroke).
            let m2m = m2m || client_id.is_some() || client_secret.is_some();
            if m2m {
                login::cmd_login_m2m(client_id, client_secret).await
            } else {
                // clap's `conflicts_with` guarantees `browser` and
                // `no_browser` aren't both set. Collapse the pair
                // into a single tri-state.
                let browser_choice = if browser {
                    Some(true)
                } else if no_browser {
                    Some(false)
                } else {
                    None
                };
                login::cmd_login_user(email, password, browser_choice).await
            }
        }
        AuthCommands::Logout { m2m, all } => logout::cmd_logout(m2m, all),
        AuthCommands::Whoami => whoami::cmd_whoami(),
        AuthCommands::Profile { cmd } => profile::dispatch(cmd),
    }
}

/// Resolve the prod Supabase URL: `SMOOAI_SUPABASE_URL` env var
/// first, then the baked-in prod default.
#[must_use]
pub fn supabase_url() -> String {
    std::env::var("SMOOAI_SUPABASE_URL").unwrap_or_else(|_| PROD_SUPABASE_URL.to_string())
}

/// Resolve the Supabase anon key. Same override pattern as
/// [`supabase_url`].
#[must_use]
pub fn supabase_anon_key() -> String {
    std::env::var("SMOOAI_SUPABASE_ANON_KEY").unwrap_or_else(|_| PROD_SUPABASE_ANON_KEY.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supabase_url_honors_env_override() {
        let prev = std::env::var("SMOOAI_SUPABASE_URL").ok();
        std::env::set_var("SMOOAI_SUPABASE_URL", "http://127.0.0.1:54331");
        assert_eq!(supabase_url(), "http://127.0.0.1:54331");
        match prev {
            Some(v) => std::env::set_var("SMOOAI_SUPABASE_URL", v),
            None => std::env::remove_var("SMOOAI_SUPABASE_URL"),
        }
    }

    #[test]
    fn prod_supabase_url_is_https_db_smoo_ai() {
        assert_eq!(PROD_SUPABASE_URL, "https://db.smoo.ai");
    }

    #[test]
    fn prod_anon_key_is_anon_role_not_service_role() {
        assert!(PROD_SUPABASE_ANON_KEY.starts_with("eyJ"));
        let parts: Vec<&str> = PROD_SUPABASE_ANON_KEY.split('.').collect();
        assert_eq!(parts.len(), 3);
        use base64::Engine;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]).expect("base64url payload");
        let payload = String::from_utf8_lossy(&payload);
        assert!(payload.contains("\"role\":\"anon\""), "MUST be anon, not service_role. Payload: {payload}");
    }
}

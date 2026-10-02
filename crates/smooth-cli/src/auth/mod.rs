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
pub mod refresh;
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
    /// Log in to Smoo AI. Defaults to the user/org browser flow
    /// (`smoo.ai/cli-login`, Supabase session) on a TTY; pass
    /// `--no-browser` for the email + password prompt, or `--m2m` to
    /// authenticate as a service account via `client_credentials`.
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
        /// DEPRECATED (hidden): a password on argv is visible in `ps` and
        /// lands in shell history. Still accepted, with a warning. Use
        /// `--password-stdin`, or omit it for the masked prompt.
        #[arg(long, hide = true, conflicts_with = "client_id", conflicts_with = "client_secret")]
        password: Option<String>,
        /// Read the password from stdin (user flow only), e.g.
        /// `op read … | th auth login --no-browser --email … --password-stdin`.
        /// Omit for the masked prompt.
        #[arg(
            long,
            conflicts_with = "password",
            conflicts_with = "client_id",
            conflicts_with = "client_secret",
            conflicts_with = "client_secret_stdin"
        )]
        password_stdin: bool,
        /// Open the browser for the OAuth2 + PKCE user/org flow against
        /// `smoo.ai/cli-login` (Supabase session). This is the **default**
        /// on a TTY; pass `--no-browser` for the password prompt or `--m2m`
        /// for a service account. Pearls th-fcb579 / th-a93734.
        #[arg(
            long,
            conflicts_with = "no_browser",
            conflicts_with = "m2m",
            conflicts_with = "email",
            conflicts_with = "password",
            conflicts_with = "password_stdin"
        )]
        browser: bool,
        /// Force the prompt-based Supabase password flow instead of the
        /// default browser flow (also via `SMOOTH_AUTH_BROWSER=0`).
        #[arg(long = "no-browser", conflicts_with = "browser")]
        no_browser: bool,

        // ── M2M flow ─────────────────────────────────
        /// Service-account client_id (M2M flow only — implies --m2m).
        /// Prompted interactively if omitted.
        #[arg(long)]
        client_id: Option<String>,
        /// DEPRECATED (hidden): a client_secret on argv is visible in `ps`
        /// and lands in shell history. Still accepted, with a warning. Use
        /// `--client-secret-stdin`, `SMOOAI_CLIENT_SECRET`, or the masked
        /// prompt.
        #[arg(long, hide = true)]
        client_secret: Option<String>,
        /// Read the service-account client_secret from stdin (M2M flow —
        /// implies --m2m). Without it: `SMOOAI_CLIENT_SECRET`, then a
        /// masked prompt.
        #[arg(long, conflicts_with = "client_secret")]
        client_secret_stdin: bool,
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
    /// Refresh the stored session now, headlessly. Defaults to the user
    /// session; pass `--m2m` to refresh the service-account session. No-op
    /// (and says so) when the token still has runway. Same silent-refresh
    /// path every `th api *` call uses — user sessions exchange the Supabase
    /// refresh token, M2M re-mints via `client_credentials`.
    Refresh {
        /// Refresh the M2M session at ~/.smooth/auth/smooai.json instead of
        /// the user session.
        #[arg(long)]
        m2m: bool,
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
    List {
        /// Print profiles as JSON instead of the rendered list.
        #[arg(long)]
        json: bool,
    },
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
            password_stdin,
            browser,
            no_browser,
            client_id,
            client_secret,
            client_secret_stdin,
        } => {
            // SMOODEV-3606: secrets on argv are deprecated, not yet removed —
            // existing scripts keep working, loudly.
            if password.is_some() {
                warn_secret_on_argv("--password", "--password-stdin");
            }
            if client_secret.is_some() {
                warn_secret_on_argv("--client-secret", "--client-secret-stdin (or SMOOAI_CLIENT_SECRET)");
            }
            let password = if password_stdin {
                Some(crate::secret_input::from_stdin("password")?)
            } else {
                password
            };
            let client_secret = if client_secret_stdin {
                Some(crate::secret_input::from_stdin("client_secret")?)
            } else {
                client_secret
            };
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
        AuthCommands::Refresh { m2m } => refresh::cmd_refresh(m2m).await,
        AuthCommands::Whoami => whoami::cmd_whoami().await,
        AuthCommands::Profile { cmd } => profile::dispatch(cmd),
    }
}

fn warn_secret_on_argv(flag: &str, instead: &str) {
    use owo_colors::OwoColorize;
    anstream::eprintln!(
        "  {} {flag} is deprecated: a secret on the command line is visible in `ps` and shell history. Use {instead} instead.",
        "warning:".yellow().bold()
    );
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

    /// CLI-Spec §flags: every platform `list` verb offers `--json`.
    #[test]
    fn profile_list_accepts_json_flag_and_defaults_to_off() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(subcommand)]
            cmd: ProfileCommands,
        }
        let on = Wrap::try_parse_from(["t", "list", "--json"]).expect("--json must parse");
        assert!(matches!(on.cmd, ProfileCommands::List { json: true }));

        let off = Wrap::try_parse_from(["t", "list"]).expect("bare list must still parse");
        assert!(matches!(off.cmd, ProfileCommands::List { json: false }), "--json must default to off");
    }

    /// SMOODEV-3606: secrets come in on stdin, not argv. The `-stdin`
    /// flags parse, `--client-secret-stdin` sits in the M2M flow, the
    /// legacy flags still parse (with a warning at dispatch) but are hidden.
    #[test]
    fn login_secret_flags_prefer_stdin_and_hide_argv_spelling() {
        use clap::{CommandFactory, Parser};

        #[derive(Parser)]
        struct Wrap {
            #[command(subcommand)]
            cmd: AuthCommands,
        }
        let p = |args: &[&str]| Wrap::try_parse_from(std::iter::once("t").chain(args.iter().copied()));

        let user = p(&["login", "--no-browser", "--email", "a@b.co", "--password-stdin"]).expect("--password-stdin parses");
        assert!(matches!(user.cmd, AuthCommands::Login { password_stdin: true, .. }));
        let m2m = p(&["login", "--client-id", "cid", "--client-secret-stdin"]).expect("--client-secret-stdin parses");
        assert!(matches!(m2m.cmd, AuthCommands::Login { client_secret_stdin: true, .. }));

        // Two sources for the same secret is a mistake, not a precedence rule.
        assert!(p(&["login", "--password", "x", "--password-stdin"]).is_err());
        assert!(p(&["login", "--client-secret", "x", "--client-secret-stdin"]).is_err());
        // A user password and an M2M secret on the same call make no sense.
        assert!(p(&["login", "--password-stdin", "--client-secret-stdin"]).is_err());

        let cmd = Wrap::command();
        let login = cmd.find_subcommand("login").expect("login subcommand");
        for legacy in ["password", "client-secret"] {
            let arg = login.get_arguments().find(|a| a.get_long() == Some(legacy)).expect("legacy flag kept");
            assert!(arg.is_hide_set(), "--{legacy} must be hidden from help");
        }
    }

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

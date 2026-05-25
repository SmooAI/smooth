//! `th admin` — Smoo AI superadmin operations.
//!
//! Distinct from `th api *` (which authenticates as a service
//! account via M2M `client_credentials`): these commands authenticate
//! as a *user* of the Smoo AI app via Supabase OAuth (PKCE +
//! localhost-callback), then hit the `/admin/*` endpoints on
//! `api.smoo.ai` that are gated by `requireSuperAdmin` middleware.
//!
//! Cardinal use case: collapse the 7-step "stand up a new customer
//! org" ceremony (create org → add member → assign role → enable
//! products → parent relationship → managed website → mint browser
//! key → mint server key → set GH secret → patch infra file) into one
//! command. Today only `login` / `logout` / `whoami` exist; the
//! cardinal command and its dependent `/admin/*` endpoints land in
//! follow-up pearls.

use anyhow::Result;
use clap::Subcommand;

pub mod login;

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
pub enum AdminCommands {
    /// Log in to the Smoo AI app as a user (Supabase OAuth, browser
    /// flow). The session is persisted to
    /// `~/.smooth/auth/smooai-user.json` (mode 0600) and used by
    /// subsequent `th admin *` commands. Distinct from `th api login`
    /// which authenticates as a service account.
    Login {
        /// Optional OAuth provider — `google`, `github`, `azure`,
        /// etc. Omit to land on Supabase's hosted-UI provider picker.
        #[arg(long)]
        provider: Option<String>,
    },
    /// Delete `~/.smooth/auth/smooai-user.json`. Idempotent.
    Logout,
    /// Show the currently-logged-in user (email, expiry).
    Whoami,
}

pub async fn dispatch(cmd: AdminCommands) -> Result<()> {
    match cmd {
        AdminCommands::Login { provider } => login::cmd_login(provider).await,
        AdminCommands::Logout => login::cmd_logout(),
        AdminCommands::Whoami => login::cmd_whoami(),
    }
}

/// Resolve the prod Supabase URL: `SMOOAI_SUPABASE_URL` env var
/// first, then the baked-in prod default. Override path exists so
/// staging / local-supabase dev doesn't require a rebuild.
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
    fn supabase_url_falls_back_to_prod_default() {
        let prev = std::env::var("SMOOAI_SUPABASE_URL").ok();
        std::env::remove_var("SMOOAI_SUPABASE_URL");
        assert_eq!(supabase_url(), PROD_SUPABASE_URL);
        if let Some(v) = prev {
            std::env::set_var("SMOOAI_SUPABASE_URL", v);
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
        // Guards against an accidental swap to the local-dev URL in
        // a future edit. The prod URL is canonical and shouldn't move
        // without a deliberate, tracked change.
        assert_eq!(PROD_SUPABASE_URL, "https://db.smoo.ai");
    }

    #[test]
    fn prod_anon_key_is_jwt_shaped() {
        // Sanity check that we didn't accidentally paste the
        // service-role key (which is also a JWT but has role=service_role
        // in the payload) — the anon key MUST decode to role=anon.
        // We don't full-decode here (no dep on a JWT crate); just verify
        // the JWT shape and that the base64-encoded payload mentions
        // "anon" rather than "service_role".
        assert!(PROD_SUPABASE_ANON_KEY.starts_with("eyJ"), "anon key must be a JWT");
        let parts: Vec<&str> = PROD_SUPABASE_ANON_KEY.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have 3 dot-separated segments");
        let payload_b64 = parts[1];
        // base64url decode the payload to verify role.
        use base64::Engine;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload_b64)
            .expect("payload is base64url");
        let payload = String::from_utf8_lossy(&decoded);
        assert!(
            payload.contains("\"role\":\"anon\""),
            "MUST be the anon key, not service_role. Payload: {payload}"
        );
    }
}

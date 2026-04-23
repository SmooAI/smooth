//! Client builder + config + run stub.
//!
//! The real multiplex loop is intentionally unimplemented — the ECS
//! rendezvous service doesn't exist yet (Jira SMOODEV-637, smooai
//! pearl th-8898f2). This file locks in the config surface so the
//! CLI can be wired up today and the loop can be filled in later
//! without another API redesign.

use url::Url;

use crate::slug::SlugPreference;
use crate::{Result, TunnelError};

/// Everything the client needs to know to dial the rendezvous.
///
/// Constructed via [`TunnelConfig::builder`] so the CLI can validate
/// individual fields incrementally and print clear errors.
#[derive(Debug, Clone)]
pub struct TunnelConfig {
    /// Rendezvous endpoint. Defaults to `wss://th.smoo.ai/tunnel`
    /// via [`TunnelConfig::production`]; dev/staging override it.
    pub service_url: Url,
    /// Local Big Smooth address the tunnel forwards to.
    /// Default: `http://127.0.0.1:4400`.
    pub local_target: Url,
    /// Short-lived token minted against the user's smooai Supabase
    /// session. The `th auth login` flow (not in this scaffold) is
    /// responsible for producing one.
    pub auth_token: String,
    /// Slug preference. See [`SlugPreference`].
    pub slug: SlugPreference,
    /// What goes into [`crate::protocol::ClientHello::user_agent`].
    pub user_agent: String,
}

impl TunnelConfig {
    /// Start a builder with production defaults. The caller still has
    /// to supply `auth_token`.
    ///
    /// # Panics
    ///
    /// Can only panic if the two compile-time-constant URLs
    /// (`wss://th.smoo.ai/tunnel` and `http://127.0.0.1:4400`) fail
    /// to parse, which they can't.
    #[must_use]
    #[allow(clippy::expect_used)] // static URL literals — parse can't fail
    pub fn production() -> TunnelConfigBuilder {
        TunnelConfigBuilder {
            service_url: Some(Url::parse("wss://th.smoo.ai/tunnel").expect("static URL parses")),
            local_target: Some(Url::parse("http://127.0.0.1:4400").expect("static URL parses")),
            auth_token: None,
            slug: SlugPreference::Ephemeral,
            user_agent: default_user_agent(),
        }
    }

    /// Empty builder for tests and for custom dev endpoints.
    #[must_use]
    pub fn builder() -> TunnelConfigBuilder {
        TunnelConfigBuilder {
            service_url: None,
            local_target: None,
            auth_token: None,
            slug: SlugPreference::Ephemeral,
            user_agent: default_user_agent(),
        }
    }
}

/// Incremental builder. `build()` validates everything in one shot
/// and returns a concrete error for the first problem it finds.
#[derive(Debug, Clone)]
pub struct TunnelConfigBuilder {
    service_url: Option<Url>,
    local_target: Option<Url>,
    auth_token: Option<String>,
    slug: SlugPreference,
    user_agent: String,
}

impl TunnelConfigBuilder {
    #[must_use]
    pub fn service_url(mut self, url: Url) -> Self {
        self.service_url = Some(url);
        self
    }
    #[must_use]
    pub fn local_target(mut self, url: Url) -> Self {
        self.local_target = Some(url);
        self
    }
    #[must_use]
    pub fn auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }
    #[must_use]
    pub fn slug(mut self, pref: SlugPreference) -> Self {
        self.slug = pref;
        self
    }
    #[must_use]
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }

    /// Finalize + validate.
    ///
    /// # Errors
    ///
    /// [`TunnelError::InvalidConfig`] if any required field is
    /// missing or a URL has the wrong scheme. [`TunnelError::InvalidSlug`]
    /// if the slug fails local validation.
    pub fn build(self) -> Result<TunnelConfig> {
        let service_url = self.service_url.ok_or_else(|| TunnelError::InvalidConfig("service_url is required".into()))?;
        if !matches!(service_url.scheme(), "ws" | "wss") {
            return Err(TunnelError::InvalidConfig(format!(
                "service_url must be ws:// or wss:// (got {})",
                service_url.scheme()
            )));
        }
        let local_target = self.local_target.ok_or_else(|| TunnelError::InvalidConfig("local_target is required".into()))?;
        if !matches!(local_target.scheme(), "http" | "https") {
            return Err(TunnelError::InvalidConfig(format!(
                "local_target must be http:// or https:// (got {})",
                local_target.scheme()
            )));
        }
        let auth_token = self.auth_token.ok_or_else(|| TunnelError::InvalidConfig("auth_token is required".into()))?;
        if auth_token.trim().is_empty() {
            return Err(TunnelError::InvalidConfig("auth_token must not be empty".into()));
        }
        self.slug.validate()?;
        Ok(TunnelConfig {
            service_url,
            local_target,
            auth_token,
            slug: self.slug,
            user_agent: self.user_agent,
        })
    }
}

/// Default `User-Agent` string if the caller doesn't override.
fn default_user_agent() -> String {
    format!("th-tunnel/{}", env!("CARGO_PKG_VERSION"))
}

/// The client end of the tunnel.
///
/// Construct with [`TunnelConfig`]; call [`TunnelClient::run`] to
/// dial + block until the session ends. `run` returns
/// [`TunnelError::NotImplementedYet`] until the ECS rendezvous
/// service is live (tracked in smooai pearl th-8898f2 / SMOODEV-637).
#[derive(Debug, Clone)]
pub struct TunnelClient {
    config: TunnelConfig,
}

impl TunnelClient {
    #[must_use]
    pub const fn new(config: TunnelConfig) -> Self {
        Self { config }
    }

    /// Read-only view of the config. Used by the CLI to print a
    /// status banner before / during the run.
    #[must_use]
    pub const fn config(&self) -> &TunnelConfig {
        &self.config
    }

    /// Dial the rendezvous, do the hello handshake, and multiplex
    /// inbound requests against the local target until the session
    /// ends.
    ///
    /// # Errors
    ///
    /// Currently always [`TunnelError::NotImplementedYet`]. Removing
    /// that branch is the signal that the scaffold grew into a real
    /// implementation.
    // `async` stays even though the scaffold body has no awaits —
    // every real implementation will be async-heavy (WS I/O,
    // multiplex loop), and flipping a CLI-visible signature later
    // would churn call sites.
    #[allow(clippy::unused_async)]
    pub async fn run(&self) -> Result<()> {
        tracing::info!(
            service = %self.config.service_url,
            local = %self.config.local_target,
            slug = %self.config.slug,
            user_agent = %self.config.user_agent,
            "th tunnel: scaffold run (no network yet — see SMOODEV-637)"
        );
        Err(TunnelError::NotImplementedYet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_token() -> String {
        "ey.test.token".to_string()
    }

    #[test]
    fn production_builder_has_defaults() {
        let cfg = TunnelConfig::production().auth_token(sample_token()).build().expect("build");
        assert_eq!(cfg.service_url.as_str(), "wss://th.smoo.ai/tunnel");
        // `url::Url` normalizes to add a trailing slash when the
        // path is empty — don't pin the exact string, just the host.
        assert_eq!(cfg.local_target.host_str(), Some("127.0.0.1"));
        assert_eq!(cfg.local_target.port(), Some(4400));
        assert!(matches!(cfg.slug, SlugPreference::Ephemeral));
        assert!(cfg.user_agent.starts_with("th-tunnel/"));
    }

    #[test]
    fn missing_service_url_rejected() {
        let err = TunnelConfig::builder()
            .local_target(Url::parse("http://127.0.0.1:4400").unwrap())
            .auth_token(sample_token())
            .build()
            .unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("service_url")));
    }

    #[test]
    fn missing_local_target_rejected() {
        let err = TunnelConfig::builder()
            .service_url(Url::parse("wss://th.smoo.ai/tunnel").unwrap())
            .auth_token(sample_token())
            .build()
            .unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("local_target")));
    }

    #[test]
    fn missing_auth_token_rejected() {
        let err = TunnelConfig::production().build().unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("auth_token")));
    }

    #[test]
    fn empty_auth_token_rejected() {
        let err = TunnelConfig::production().auth_token("   ").build().unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("auth_token")));
    }

    #[test]
    fn wrong_service_scheme_rejected() {
        let err = TunnelConfig::production()
            .service_url(Url::parse("http://th.smoo.ai/tunnel").unwrap())
            .auth_token(sample_token())
            .build()
            .unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("ws://")));
    }

    #[test]
    fn wrong_local_scheme_rejected() {
        let err = TunnelConfig::production()
            .local_target(Url::parse("wss://127.0.0.1:4400").unwrap())
            .auth_token(sample_token())
            .build()
            .unwrap_err();
        assert!(matches!(err, TunnelError::InvalidConfig(msg) if msg.contains("http://")));
    }

    #[test]
    fn invalid_slug_preference_rejected() {
        let err = TunnelConfig::production()
            .auth_token(sample_token())
            .slug(SlugPreference::Requested("-bad".into()))
            .build()
            .unwrap_err();
        assert!(matches!(err, TunnelError::InvalidSlug(_)));
    }

    #[tokio::test]
    async fn run_returns_not_implemented_yet() {
        let cfg = TunnelConfig::production().auth_token(sample_token()).build().expect("build");
        let client = TunnelClient::new(cfg);
        let err = client.run().await.unwrap_err();
        assert!(matches!(err, TunnelError::NotImplementedYet), "got {err:?}");
    }
}

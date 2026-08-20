//! Silent auto-refresh helpers for both auth flavors — **the** choke
//! point every credential load goes through.
//!
//! - [`refresh_user_session`] exchanges a stored Supabase `refresh_token`
//!   for a fresh `access_token` at `{supabase}/auth/v1/token`.
//! - [`refresh_m2m_session`] re-runs the OAuth 2.0 `client_credentials`
//!   grant at `auth.smoo.ai/token` using a stored client_id / client_secret.
//! - [`fresh_credentials_from`] loads a store and applies whichever of
//!   the two [`decide`] picks. Every caller that needs a usable token
//!   goes through here rather than re-deriving the branch (pearl
//!   th-2273b8 — `config.rs` had hand-rolled its own copy, and
//!   `SmoothApiClient::ensure_fresh_token` only ever knew about M2M).
//!
//! **Single-writer rule**: refresh is lazy / on-demand only. Supabase
//! rotates refresh tokens with a 10-second reuse grace, so two
//! components refreshing the same file concurrently revoke each other
//! and kill the session. The daemon's credential heartbeat owns the
//! background cadence; the CLI must only ever refresh in the request
//! path. Convention alone did not hold that line across concurrent
//! agent sessions, so [`refresh_locked`] now enforces it with the
//! cross-process lock in [`smooth_api_client::credential_lock`] (th-5c0189).

use anyhow::{Context, Result};
use smooai_client_shared::auth::refresh::refresh_session;
use smooai_client_shared::auth::storage::{Credentials, CredentialsStore};

use crate::auth::{supabase_url, PROD_SUPABASE_ANON_KEY};
use smooth_api_client::credential_lock;

/// What a loaded session needs before it can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// Still valid (60s safety margin) — use as-is.
    NotNeeded,
    /// Expired M2M session: re-mint via `client_credentials`.
    M2m,
    /// Expired user session: exchange the Supabase `refresh_token`.
    User,
}

/// Decide how to freshen `creds`. Pure — no I/O, so every branch is
/// unit-testable without a network.
///
/// M2M wins over user when both kinds of material are present: an M2M
/// session's `refresh_token` (when the server returns one) expires
/// with its access token, so re-minting is the only path that works.
///
/// # Errors
/// The session is expired and carries no refresh material at all —
/// nothing to do but `th auth login`.
pub fn decide(creds: &Credentials) -> Result<Refresh> {
    if !creds.is_expired() {
        return Ok(Refresh::NotNeeded);
    }
    if creds.client_id.is_some() && creds.client_secret.is_some() {
        return Ok(Refresh::M2m);
    }
    if creds.refresh_token.is_some() {
        return Ok(Refresh::User);
    }
    anyhow::bail!("session expired and has no refresh token — run `th auth login` again")
}

/// Load a session from `store`, silently refreshing it when expired
/// and persisting the rotated token. `missing_hint` is the error when
/// the store is empty (callers word it for their own flow).
///
/// Persisting matters: Supabase revokes the old refresh token on every
/// successful exchange, so skipping the save breaks the *next* refresh.
///
/// # Errors
/// No session on disk, no refresh material, or the grant itself failed
/// — each carrying a `th auth login` hint.
pub async fn fresh_credentials_from(http: &reqwest::Client, store: &CredentialsStore, missing_hint: &str) -> Result<Credentials> {
    let creds = store.load().context("load session")?.ok_or_else(|| anyhow::anyhow!("{missing_hint}"))?;
    // Unlocked fast path: a still-valid session needs no writer, so the
    // overwhelmingly common `th` invocation never touches the lock.
    if decide(&creds)? == Refresh::NotNeeded {
        return Ok(creds);
    }
    refresh_locked(http, store, missing_hint).await
}

/// The refresh proper, serialized against every other `th` process
/// through the credentials lock.
///
/// Re-reads the store *after* taking the lock: whoever we queued behind
/// has already rotated the token, and Supabase revoked ours the moment
/// theirs was issued. Using their result is both cheaper and the only
/// thing that keeps the session alive — minting a second token would
/// revoke the one now live on disk.
///
/// # Errors
/// No session on disk (worded by `missing_hint`), no refresh material,
/// the grant itself failing, or the rotated token failing to persist.
pub(crate) async fn refresh_locked(http: &reqwest::Client, store: &CredentialsStore, missing_hint: &str) -> Result<Credentials> {
    let _lock = credential_lock(store.path()).context("lock the credentials file for refresh")?;
    let creds = store.load().context("load session")?.ok_or_else(|| anyhow::anyhow!("{missing_hint}"))?;
    let refreshed = match decide(&creds)? {
        // Another process refreshed while we waited — its token is the
        // live one.
        Refresh::NotNeeded => return Ok(creds),
        Refresh::M2m => refresh_m2m_session(http, &creds).await.context("auto-refresh M2M client_credentials grant")?,
        Refresh::User => refresh_user_session(http, &creds)
            .await
            .context("silent session refresh failed — run `th auth login` again")?,
    };
    // Not best-effort. The exchange already revoked the old refresh
    // token server-side, so dropping this write leaves the *next* run
    // holding a dead session — reporting success here is how a broken
    // persist stays invisible until login breaks.
    store.save(&refreshed).context("persist the rotated session")?;
    Ok(refreshed)
}

/// Load the `th auth login` user session, silently refreshing it when
/// expired and persisting the rotated `refresh_token` (pearl th-32d00e —
/// login is once-per-machine; the session lives as long as its refresh
/// token). Errors, each with a `th auth login` hint, only when: no
/// session exists, the session is expired with no refresh material, or
/// the refresh grant itself fails.
pub async fn fresh_user_credentials(http: &reqwest::Client) -> Result<Credentials> {
    let store = CredentialsStore::default_user().context("locate user credentials store")?;
    fresh_user_credentials_from(http, &store).await
}

/// [`fresh_user_credentials`] against an explicit store (testable).
pub async fn fresh_user_credentials_from(http: &reqwest::Client, store: &CredentialsStore) -> Result<Credentials> {
    fresh_credentials_from(http, store, "not logged in as a user — run `th auth login` first").await
}

/// `th auth refresh` — freshen the stored session **now**, headlessly.
/// Default targets the user session; `--m2m` targets the service-account
/// session. Whichever store is chosen, [`fresh_credentials_from`] inspects the
/// loaded credentials and picks the right grant (Supabase exchange vs.
/// `client_credentials` re-mint) — this command adds no refresh logic of its
/// own, it just exposes the existing choke point (th-1d3362).
///
/// # Errors
/// No session on disk, an expired session with no refresh material, or the
/// grant itself failing — each already carries a `th auth login` hint.
pub async fn cmd_refresh(m2m: bool) -> Result<()> {
    let http = reqwest::Client::new();
    let (store, hint, kind) = if m2m {
        (
            CredentialsStore::default_m2m().context("locate the M2M credentials store")?,
            "no M2M session — run `th auth login --m2m` first",
            "M2M",
        )
    } else {
        (
            CredentialsStore::default_user().context("locate the user credentials store")?,
            "not logged in as a user — run `th auth login` first",
            "user",
        )
    };
    refresh_store(&http, &store, hint, kind).await.map(|_| ())
}

/// The refresh + user-facing reporting, against an explicit store (testable —
/// `cmd_refresh` only resolves which store the `--m2m` flag selects). Returns
/// the [`Refresh`] action taken so tests can assert refreshed-vs-already-fresh
/// without scraping stdout.
///
/// # Errors
/// No session on disk (worded by `hint`), an expired session with no refresh
/// material, or the grant itself failing.
async fn refresh_store(http: &reqwest::Client, store: &CredentialsStore, hint: &str, kind: &str) -> Result<Refresh> {
    use anstream::println;
    use owo_colors::OwoColorize;

    // Peek before acting so we can report refreshed-vs-already-fresh honestly.
    let before = store.load().context("load session")?.ok_or_else(|| anyhow::anyhow!("{hint}"))?;
    let action = decide(&before)?;
    if action == Refresh::NotNeeded {
        println!("{} {kind} session already fresh", "✓".green().bold());
        if let Some(exp) = before.expires_at {
            println!("  {} expires {}", "ℹ".dimmed(), exp.format("%Y-%m-%d %H:%M UTC").to_string().dimmed());
        }
        return Ok(action);
    }

    let fresh = fresh_credentials_from(http, store, hint).await?;
    println!("{} {kind} session refreshed", "✓".green().bold());
    if let Some(exp) = fresh.expires_at {
        println!(
            "  {} access token expires {}",
            "ℹ".dimmed(),
            exp.format("%Y-%m-%d %H:%M UTC").to_string().dimmed()
        );
    }
    Ok(action)
}

/// Exchange the stored Supabase `refresh_token` for a fresh
/// `access_token` + new `refresh_token`. Preserves the user-display
/// fields (`user`, `active_org_id`) from the previous credentials.
///
/// # Errors
/// - Credentials carry no `refresh_token`
/// - Network failure POSTing to `/auth/v1/token`
/// - Supabase returns non-2xx (refresh_token revoked, anon key wrong,
///   Supabase project paused, etc.)
/// - Response missing `access_token`
pub async fn refresh_user_session(http: &reqwest::Client, previous: &Credentials) -> Result<Credentials> {
    let anon_key = std::env::var("SMOOAI_SUPABASE_ANON_KEY").unwrap_or_else(|_| PROD_SUPABASE_ANON_KEY.to_string());
    refresh_session(http, &supabase_url(), &anon_key, previous).await
}

/// Re-run the OAuth `client_credentials` grant against `auth.smoo.ai`
/// using the stored client_id + client_secret. Preserves
/// `active_org_id` from the previous credentials.
///
/// # Errors
/// - Credentials carry no `client_id` / `client_secret`
/// - Network failure / non-2xx from `auth.smoo.ai`
pub async fn refresh_m2m_session(http: &reqwest::Client, previous: &Credentials) -> Result<Credentials> {
    let (Some(cid), Some(csecret)) = (previous.client_id.as_deref(), previous.client_secret.as_deref()) else {
        anyhow::bail!("M2M session has no stored client_id / client_secret — re-run `th auth login --m2m`");
    };
    use smooai_client_shared::auth::m2m::client_credentials_grant;
    let mut refreshed = client_credentials_grant(http, cid, csecret).await.context("client_credentials grant")?;
    refreshed.active_org_id = previous.active_org_id.clone();
    Ok(refreshed)
}

#[cfg(test)]
mod fresh_user_credentials_tests {
    use chrono::Utc;
    use smooai_client_shared::auth::storage::CredentialKind;

    use super::*;

    fn creds(kind: CredentialKind, expires_at: Option<chrono::DateTime<Utc>>, refresh: Option<&str>) -> Credentials {
        let m2m = kind == CredentialKind::M2m;
        Credentials {
            access_token: "tok".into(),
            refresh_token: refresh.map(str::to_string),
            expires_at,
            user: Some("u@example.com".into()),
            active_org_id: None,
            client_id: m2m.then(|| "cid".to_string()),
            client_secret: m2m.then(|| "csecret".to_string()),
            kind,
            created_at: Utc::now(),
        }
    }

    fn write_creds(dir: &std::path::Path, expires_at: Option<chrono::DateTime<Utc>>, refresh: Option<&str>) -> CredentialsStore {
        let store = CredentialsStore::at(dir.join("smooai-user.json"));
        store.save(&creds(CredentialKind::User, expires_at, refresh)).unwrap();
        store
    }

    fn http() -> reqwest::Client {
        reqwest::Client::new()
    }

    // ── decide(): the whole branch table, no network ──────────────

    #[test]
    fn decide_fresh_user_session_needs_nothing() {
        let c = creds(CredentialKind::User, Some(Utc::now() + chrono::Duration::hours(1)), Some("rtok"));
        assert_eq!(decide(&c).unwrap(), Refresh::NotNeeded);
    }

    #[test]
    fn decide_expired_user_session_with_refresh_token_refreshes() {
        let c = creds(CredentialKind::User, Some(Utc::now() - chrono::Duration::hours(1)), Some("rtok"));
        assert_eq!(decide(&c).unwrap(), Refresh::User);
    }

    #[test]
    fn decide_expired_user_session_without_refresh_token_errors() {
        let c = creds(CredentialKind::User, Some(Utc::now() - chrono::Duration::hours(1)), None);
        let msg = format!("{:#}", decide(&c).unwrap_err());
        assert!(msg.contains("th auth login"), "got: {msg}");
    }

    #[test]
    fn decide_expired_m2m_session_re_mints() {
        let c = creds(CredentialKind::M2m, Some(Utc::now() - chrono::Duration::hours(1)), None);
        assert_eq!(decide(&c).unwrap(), Refresh::M2m);
    }

    #[test]
    fn decide_m2m_wins_over_a_stale_refresh_token() {
        // An M2M grant can return a refresh_token whose TTL matches the
        // access token — re-minting is the only path that works, so the
        // M2M branch must take priority.
        let c = creds(CredentialKind::M2m, Some(Utc::now() - chrono::Duration::hours(1)), Some("rtok"));
        assert_eq!(decide(&c).unwrap(), Refresh::M2m);
    }

    #[test]
    fn decide_session_without_expiry_is_left_alone() {
        assert_eq!(decide(&creds(CredentialKind::User, None, Some("rtok"))).unwrap(), Refresh::NotNeeded);
    }

    #[test]
    fn decide_within_60s_safety_margin_refreshes() {
        let c = creds(CredentialKind::User, Some(Utc::now() + chrono::Duration::seconds(30)), Some("rtok"));
        assert_eq!(decide(&c).unwrap(), Refresh::User);
    }

    #[tokio::test]
    async fn valid_session_passes_through_without_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let store = write_creds(dir.path(), Some(Utc::now() + chrono::Duration::hours(1)), None);
        let creds = fresh_user_credentials_from(&http(), &store).await.unwrap();
        assert_eq!(creds.access_token, "tok");
    }

    #[tokio::test]
    async fn missing_session_says_login() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsStore::at(dir.path().join("smooai-user.json"));
        let err = fresh_user_credentials_from(&http(), &store).await.unwrap_err();
        assert!(format!("{err:#}").contains("th auth login"));
    }

    #[tokio::test]
    async fn expired_without_refresh_token_says_login_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = write_creds(dir.path(), Some(Utc::now() - chrono::Duration::hours(1)), None);
        let err = fresh_user_credentials_from(&http(), &store).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("no refresh token") && msg.contains("th auth login"), "got: {msg}");
    }

    #[tokio::test]
    async fn expired_m2m_session_takes_the_m2m_path_untouched() {
        // M2M must keep re-minting via client_credentials, never the
        // Supabase refresh grant. Point the token endpoint at a closed
        // port and assert we failed on *that* grant.
        let _guard = ENV_LOCK.lock().await;
        let prev = std::env::var("SMOOAI_AUTH_URL").ok();
        std::env::set_var("SMOOAI_AUTH_URL", "http://127.0.0.1:9/token");
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsStore::at(dir.path().join("smooai.json"));
        store
            .save(&creds(CredentialKind::M2m, Some(Utc::now() - chrono::Duration::hours(1)), None))
            .unwrap();
        let result = fresh_credentials_from(&http(), &store, "nope").await;
        restore("SMOOAI_AUTH_URL", prev);
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("client_credentials"), "expected the M2M grant to be attempted, got: {msg}");
    }

    // ── refresh over the wire, against a local stub ───────────────

    /// `SMOOAI_SUPABASE_URL` / `SMOOAI_AUTH_URL` are process-global, so
    /// the tests that override them can't run concurrently.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn restore(key: &str, prev: Option<String>) {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    /// One-shot Supabase `/auth/v1/token` stub. Returns its base URL.
    fn stub_supabase(body: &'static str) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        std::thread::spawn(move || {
            if let Ok(req) = server.recv() {
                let resp = tiny_http::Response::from_string(body).with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap());
                let _ = req.respond(resp);
            }
        });
        format!("http://{addr}")
    }

    /// Same stub, serving every request and counting them. The count is
    /// the whole point: an exchange is a *token rotation*, so "how many
    /// times did we exchange" is exactly "how many times did we revoke
    /// somebody's live refresh token".
    fn stub_supabase_counting(body: &'static str, hits: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let resp = tiny_http::Response::from_string(body).with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap());
                let _ = req.respond(resp);
            }
        });
        format!("http://{addr}")
    }

    // ── th-5c0189: concurrent `th` processes must not revoke each other ──

    /// Four sessions all find the same expired credentials and all want a
    /// refresh. Before the lock, each POSTed to Supabase and each got a
    /// rotated token that invalidated the others' — one `rename` won and
    /// the survivor's token was NOT the one live server-side, so the
    /// session was dead until `th auth login`.
    ///
    /// Asserts on outcome, not timing: exactly one exchange happened, all
    /// four callers came back with the same usable token, and the token on
    /// disk is the rotated one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_refreshes_exchange_the_token_exactly_once() {
        let _guard = ENV_LOCK.lock().await;
        let prev = std::env::var("SMOOAI_SUPABASE_URL").ok();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::env::set_var(
            "SMOOAI_SUPABASE_URL",
            stub_supabase_counting(
                r#"{"access_token":"fresh-jwt","refresh_token":"rotated-rtok","expires_in":3600,"user":{"id":"u-1","email":"u@example.com"}}"#,
                std::sync::Arc::clone(&hits),
            ),
        );
        let dir = tempfile::tempdir().unwrap();
        let store = write_creds(dir.path(), Some(Utc::now() - chrono::Duration::hours(1)), Some("old-rtok"));

        let mut handles = Vec::new();
        for _ in 0..4 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                fresh_user_credentials_from(&http(), &store).await.map(|c| c.access_token)
            }));
        }
        let mut tokens = Vec::new();
        for h in handles {
            tokens.push(h.await.unwrap());
        }
        restore("SMOOAI_SUPABASE_URL", prev);

        for token in tokens {
            assert_eq!(token.unwrap(), "fresh-jwt", "every caller must end up with the live token");
        }
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "each extra exchange revokes the previous refresh token — that is the session-killer"
        );
        let on_disk = store.load().unwrap().unwrap();
        assert_eq!(
            on_disk.refresh_token.as_deref(),
            Some("rotated-rtok"),
            "the rotated token must be the one persisted"
        );
    }

    /// A refresh that can't be persisted must not report success: the
    /// exchange already revoked the old refresh token, so the caller is
    /// holding the last usable copy of a session the next run can't reach.
    /// `let _ = store.save(...)` printed "✓ session refreshed" and exited 0
    /// in exactly that situation.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_persist_is_an_error_not_a_silent_success() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_LOCK.lock().await;
        let prev = std::env::var("SMOOAI_SUPABASE_URL").ok();
        std::env::set_var(
            "SMOOAI_SUPABASE_URL",
            stub_supabase(r#"{"access_token":"fresh-jwt","refresh_token":"rotated-rtok","expires_in":3600,"user":{"id":"u-1","email":"u@example.com"}}"#),
        );
        let dir = tempfile::tempdir().unwrap();
        let store = write_creds(dir.path(), Some(Utc::now() - chrono::Duration::hours(1)), Some("old-rtok"));
        // Pre-create the lock sidecar: a read-only directory still allows
        // opening an existing file, just not creating a new one — which is
        // precisely the seam we want (the load reads, the save can't write).
        std::fs::File::create(dir.path().join("smooai-user.lock")).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let running_as_root = std::fs::File::create(dir.path().join("root-probe")).is_ok();

        let result = refresh_locked(&http(), &store, "hint").await;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        restore("SMOOAI_SUPABASE_URL", prev);

        // Root ignores directory permissions, so the seam doesn't exist there.
        if running_as_root {
            return;
        }
        let err = format!("{:#}", result.expect_err("a dropped rotated token must surface as an error"));
        assert!(err.contains("persist the rotated session"), "got: {err}");
    }

    #[tokio::test]
    async fn expired_user_session_refreshes_and_persists_the_rotated_token() {
        let _guard = ENV_LOCK.lock().await;
        let prev = std::env::var("SMOOAI_SUPABASE_URL").ok();
        std::env::set_var(
            "SMOOAI_SUPABASE_URL",
            stub_supabase(r#"{"access_token":"fresh-jwt","refresh_token":"rotated-rtok","expires_in":3600,"user":{"id":"u-1","email":"u@example.com"}}"#),
        );
        let dir = tempfile::tempdir().unwrap();
        let store = write_creds(dir.path(), Some(Utc::now() - chrono::Duration::hours(1)), Some("old-rtok"));
        let result = fresh_user_credentials_from(&http(), &store).await;
        restore("SMOOAI_SUPABASE_URL", prev);

        let refreshed = result.unwrap();
        assert_eq!(refreshed.access_token, "fresh-jwt");
        // Supabase revokes the old refresh token on exchange — losing
        // the rotated one would kill the *next* refresh.
        let on_disk = store.load().unwrap().unwrap();
        assert_eq!(on_disk.refresh_token.as_deref(), Some("rotated-rtok"));
        assert_eq!(on_disk.access_token, "fresh-jwt");
        assert_eq!(on_disk.kind, CredentialKind::User, "kind must survive the round-trip");
        assert!(!on_disk.is_expired());
    }

    #[tokio::test]
    async fn expired_with_revoked_refresh_token_fails_with_login_hint() {
        let _guard = ENV_LOCK.lock().await;
        let prev = std::env::var("SMOOAI_SUPABASE_URL").ok();
        // Point Supabase at a closed port so the test never leaves the box.
        std::env::set_var("SMOOAI_SUPABASE_URL", "http://127.0.0.1:9");
        let dir = tempfile::tempdir().unwrap();
        let store = write_creds(dir.path(), Some(Utc::now() - chrono::Duration::hours(1)), Some("bogus"));
        let result = fresh_user_credentials_from(&http(), &store).await;
        restore("SMOOAI_SUPABASE_URL", prev);
        assert!(format!("{:#}", result.unwrap_err()).contains("th auth login"));
    }

    // ── refresh_store: the `th auth refresh` command's testable core ──

    #[tokio::test]
    async fn refresh_store_reports_already_fresh_without_touching_the_network() {
        // A session with an hour of runway needs no grant — no stub, no env.
        let dir = tempfile::tempdir().unwrap();
        let store = write_creds(dir.path(), Some(Utc::now() + chrono::Duration::hours(1)), Some("rtok"));
        let action = refresh_store(&http(), &store, "hint", "user").await.unwrap();
        assert_eq!(action, Refresh::NotNeeded);
    }

    #[tokio::test]
    async fn refresh_store_missing_session_errors_with_the_login_hint() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsStore::at(dir.path().join("smooai.json"));
        let err = refresh_store(&http(), &store, "no M2M session — run `th auth login --m2m` first", "M2M")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("th auth login --m2m"));
    }

    #[tokio::test]
    async fn refresh_store_re_mints_an_expired_m2m_session_headlessly() {
        // The exact capability the command exists to expose: an expired M2M
        // session re-mints via `client_credentials` with no browser, and the
        // fresh token lands on disk.
        let _guard = ENV_LOCK.lock().await;
        let prev = std::env::var("SMOOAI_AUTH_URL").ok();
        std::env::set_var(
            "SMOOAI_AUTH_URL",
            stub_supabase(r#"{"access_token":"fresh-m2m-jwt","token_type":"Bearer","expires_in":3600}"#),
        );
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsStore::at(dir.path().join("smooai.json"));
        store
            .save(&creds(CredentialKind::M2m, Some(Utc::now() - chrono::Duration::hours(1)), None))
            .unwrap();

        let action = refresh_store(&http(), &store, "hint", "M2M").await;
        restore("SMOOAI_AUTH_URL", prev);

        assert_eq!(action.unwrap(), Refresh::M2m);
        let on_disk = store.load().unwrap().unwrap();
        assert_eq!(on_disk.access_token, "fresh-m2m-jwt");
        assert!(!on_disk.is_expired(), "re-minted token must have runway");
    }
}

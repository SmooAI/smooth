//! The relay link's credential state machine + its observable status
//! (pearl th-37c286).
//!
//! The bug this exists for: a daemon that booted before the user signed in to
//! Smoo armed its relay, and a phone's `list_peers` then came back empty —
//! which looks exactly like "the daemon is offline". The relay authenticates a
//! socket BEFORE it registers it as a peer and acks with `{"type":"connected"}`;
//! the old supervisor logged "connected" at the WS upgrade and never looked
//! for that ack, and only ever re-read the credentials after a socket drop.
//!
//! So the link now follows the credentials store, not just the socket:
//!
//! - no usable session → dial nothing, report `signed_out` / `session_expired`;
//! - a session appears (login, or the credential heartbeat renewing an expired
//!   one) → dial at once, without waiting out a retry timer;
//! - socket open but no `connected` ack yet → `authenticating`, and if the ack
//!   never comes, `unauthenticated` — loudly, never "online";
//! - the signed-in user changes → reconnect as the new user;
//! - the token rotates → re-authenticate if the link had not authenticated yet;
//!   an authenticated link keeps going (the relay checks the token at connect
//!   only, so re-dialling on every hourly rotation would just drop the phones);
//! - the session is removed (logout) or expires beyond renewal → disconnect.
//!
//! Everything that decides is a pure function over plain values, so the rules
//! are tested without a relay, a clock, or a credentials file.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use smooai_client_shared::auth::storage::Credentials;
use tokio::sync::watch;

/// The credentials store reduced to what the relay link cares about. Never
/// holds the token itself — only a fingerprint, so it can sit in a `watch`
/// and be logged without leaking anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredView {
    /// No session on disk: signed out, or never signed in.
    SignedOut,
    /// A session is on disk but its access token is past expiry — the
    /// credential heartbeat could not renew it, so a human must sign in again.
    Expired { user: Option<String> },
    /// A usable session.
    Usable { user: Option<String>, token_fp: u64 },
}

impl CredView {
    /// Reduce what `CredentialsStore::load` returned. Pure over `now`.
    pub fn observe(creds: Option<&Credentials>, now: DateTime<Utc>) -> Self {
        let Some(c) = creds else { return Self::SignedOut };
        if c.access_token.is_empty() {
            return Self::SignedOut;
        }
        if c.expires_at.is_some_and(|exp| exp <= now) {
            return Self::Expired { user: c.user.clone() };
        }
        Self::Usable {
            user: c.user.clone(),
            token_fp: token_fingerprint(&c.access_token),
        }
    }

    /// The view of a session the supervisor actually dialled with — the token
    /// it put on the wire, which may be newer than the last poll saw.
    pub fn dialled(user: Option<String>, token: &str) -> Self {
        Self::Usable {
            user,
            token_fp: token_fingerprint(token),
        }
    }

    pub const fn is_usable(&self) -> bool {
        matches!(self, Self::Usable { .. })
    }
}

/// A stable, non-reversible handle on a token, for change detection only.
pub fn token_fingerprint(token: &str) -> u64 {
    let mut h = DefaultHasher::new();
    token.hash(&mut h);
    h.finish()
}

/// Where the open socket is in the relay's handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Upgrade accepted, `{"type":"connected"}` not seen yet — the relay has
    /// NOT registered this daemon as a peer.
    Authenticating,
    /// The relay acked: phones can see and reach this daemon.
    Online,
}

/// What to do with an open socket when the credentials change under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredDecision {
    /// Nothing that matters changed.
    Keep,
    /// Close and dial again with the new session, right away.
    Reconnect,
    /// Close and stay off the relay until a usable session appears.
    Disconnect,
}

/// The rule for a credentials change while a socket is open. `dialled` is the
/// session the socket was opened with; `now` is what the store says now.
pub fn on_cred_change(link: Link, dialled: &CredView, now: &CredView) -> CredDecision {
    let (
        CredView::Usable {
            user: was_user,
            token_fp: was_fp,
        },
        CredView::Usable { user, token_fp },
    ) = (dialled, now)
    else {
        // Signed out, or expired beyond renewal: this user's session is over.
        return if now.is_usable() { CredDecision::Reconnect } else { CredDecision::Disconnect };
    };
    if was_user != user {
        return CredDecision::Reconnect;
    }
    if was_fp == token_fp {
        return CredDecision::Keep;
    }
    match link {
        // Still waiting on the ack — the old token may be why. Try the new one.
        Link::Authenticating => CredDecision::Reconnect,
        // The relay checks the token at connect only; a rotation doesn't
        // invalidate an authenticated socket, and re-dialling would drop every
        // phone bridge once an hour.
        Link::Online => CredDecision::Keep,
    }
}

/// What the relay link is doing, as the app and the logs report it. The whole
/// point is that `authenticating` / `unauthenticated` are never `online`, and
/// none of them is mistaken for "the daemon is off".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayPhase {
    /// `SMOOTH_RELAY=0` — this daemon never dials the relay.
    Disabled,
    /// No Smoo session: nothing is dialled until one appears.
    SignedOut,
    /// A session is on disk but expired beyond renewal.
    SessionExpired,
    /// Another daemon on this machine holds this relay device id.
    IdentityBusy,
    /// Dialling the relay.
    Connecting,
    /// Socket open, waiting for the relay to authenticate this daemon.
    Authenticating,
    /// Authenticated and registered as a peer.
    Online,
    /// The socket opened but the relay never acknowledged auth — this daemon
    /// is NOT a peer, however connected it looks.
    Unauthenticated,
    /// The relay rejected the token (4401 / 401).
    AuthRejected,
    /// Dropped; retrying with backoff.
    Offline,
}

impl RelayPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::SignedOut => "signed_out",
            Self::SessionExpired => "session_expired",
            Self::IdentityBusy => "identity_busy",
            Self::Connecting => "connecting",
            Self::Authenticating => "authenticating",
            Self::Online => "online",
            Self::Unauthenticated => "unauthenticated",
            Self::AuthRejected => "auth_rejected",
            Self::Offline => "offline",
        }
    }

    /// Phones can reach this daemon only here.
    pub const fn reachable(self) -> bool {
        matches!(self, Self::Online)
    }
}

/// The phase a daemon should report while it has no usable session.
pub const fn waiting_phase(view: &CredView) -> RelayPhase {
    match view {
        CredView::Expired { .. } => RelayPhase::SessionExpired,
        CredView::SignedOut | CredView::Usable { .. } => RelayPhase::SignedOut,
    }
}

/// One status snapshot: the phase, a sentence a human can act on, and when
/// the phase last changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelayStatus {
    pub state: RelayPhase,
    pub detail: String,
    pub since: DateTime<Utc>,
}

/// Shared, cheaply cloned status cell. The supervisor writes, routes read.
#[derive(Clone)]
pub struct RelayStatusHandle(Arc<watch::Sender<RelayStatus>>);

impl RelayStatusHandle {
    pub fn new(state: RelayPhase, detail: impl Into<String>) -> Self {
        let (tx, _) = watch::channel(RelayStatus {
            state,
            detail: detail.into(),
            since: Utc::now(),
        });
        Self(Arc::new(tx))
    }

    /// A handle for a daemon whose relay is switched off.
    pub fn disabled() -> Self {
        Self::new(
            RelayPhase::Disabled,
            "The relay is off for this engine (SMOOTH_RELAY=0) — phones cannot reach it.",
        )
    }

    /// Set the phase + detail. Returns `true` when the phase changed, so the
    /// caller logs transitions rather than every retry.
    pub fn set(&self, state: RelayPhase, detail: impl Into<String>) -> bool {
        let detail = detail.into();
        let mut changed = false;
        self.0.send_if_modified(|s| {
            changed = s.state != state;
            if !changed && s.detail == detail {
                return false;
            }
            if changed {
                s.since = Utc::now();
            }
            s.state = state;
            s.detail = detail;
            true
        });
        changed
    }

    pub fn get(&self) -> RelayStatus {
        self.0.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<RelayStatus> {
        self.0.subscribe()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use chrono::Duration;
    use smooai_client_shared::auth::storage::CredentialKind;

    use super::*;

    fn creds(user: &str, token: &str, expires_in: Option<i64>) -> Credentials {
        Credentials {
            access_token: token.into(),
            refresh_token: Some("r".into()),
            expires_at: expires_in.map(|s| Utc::now() + Duration::seconds(s)),
            user: Some(user.into()),
            active_org_id: None,
            client_id: None,
            client_secret: None,
            kind: CredentialKind::User,
            created_at: Utc::now(),
        }
    }

    fn usable(user: &str, token: &str) -> CredView {
        CredView::dialled(Some(user.into()), token)
    }

    // ── observe ──────────────────────────────────────────────────────────────

    #[test]
    fn no_file_is_signed_out() {
        assert_eq!(CredView::observe(None, Utc::now()), CredView::SignedOut);
    }

    #[test]
    fn an_empty_token_is_signed_out_not_usable() {
        assert_eq!(CredView::observe(Some(&creds("a", "", Some(3600))), Utc::now()), CredView::SignedOut);
    }

    #[test]
    fn a_past_expiry_session_is_expired_and_keeps_who() {
        let v = CredView::observe(Some(&creds("a@x", "t", Some(-10))), Utc::now());
        assert_eq!(v, CredView::Expired { user: Some("a@x".into()) });
        assert!(!v.is_usable());
    }

    #[test]
    fn a_live_session_is_usable_with_a_fingerprint_not_the_token() {
        let v = CredView::observe(Some(&creds("a", "secret-token", Some(3600))), Utc::now());
        assert_eq!(v, usable("a", "secret-token"));
        assert!(!format!("{v:?}").contains("secret-token"), "the view must never carry the token");
    }

    #[test]
    fn unknown_expiry_is_usable() {
        assert!(CredView::observe(Some(&creds("a", "t", None)), Utc::now()).is_usable());
    }

    #[test]
    fn fingerprints_differ_per_token_and_are_stable() {
        assert_eq!(token_fingerprint("a"), token_fingerprint("a"));
        assert_ne!(token_fingerprint("a"), token_fingerprint("b"));
    }

    // ── the state machine ────────────────────────────────────────────────────

    #[test]
    fn no_creds_arms_nothing_and_does_not_claim_to_be_connected() {
        let v = CredView::SignedOut;
        assert!(!v.is_usable(), "the supervisor dials only a usable session");
        let phase = waiting_phase(&v);
        assert_eq!(phase, RelayPhase::SignedOut);
        assert!(!phase.reachable());
    }

    #[test]
    fn an_expired_session_waits_as_session_expired() {
        assert_eq!(waiting_phase(&CredView::Expired { user: None }), RelayPhase::SessionExpired);
    }

    #[test]
    fn unchanged_creds_keep_the_link() {
        let a = usable("a", "t1");
        assert_eq!(on_cred_change(Link::Online, &a, &a), CredDecision::Keep);
        assert_eq!(on_cred_change(Link::Authenticating, &a, &a), CredDecision::Keep);
    }

    #[test]
    fn a_rotation_re_authenticates_a_link_that_never_authenticated() {
        assert_eq!(
            on_cred_change(Link::Authenticating, &usable("a", "t1"), &usable("a", "t2")),
            CredDecision::Reconnect
        );
    }

    #[test]
    fn a_rotation_leaves_an_authenticated_link_alone() {
        assert_eq!(on_cred_change(Link::Online, &usable("a", "t1"), &usable("a", "t2")), CredDecision::Keep);
    }

    #[test]
    fn a_different_user_reconnects_even_when_online() {
        assert_eq!(on_cred_change(Link::Online, &usable("a", "t1"), &usable("b", "t2")), CredDecision::Reconnect);
    }

    #[test]
    fn logout_disconnects() {
        for link in [Link::Online, Link::Authenticating] {
            assert_eq!(on_cred_change(link, &usable("a", "t1"), &CredView::SignedOut), CredDecision::Disconnect);
        }
    }

    #[test]
    fn expiry_beyond_renewal_disconnects() {
        assert_eq!(
            on_cred_change(Link::Online, &usable("a", "t1"), &CredView::Expired { user: Some("a".into()) }),
            CredDecision::Disconnect
        );
    }

    #[test]
    fn only_online_is_reachable() {
        for p in [
            RelayPhase::Disabled,
            RelayPhase::SignedOut,
            RelayPhase::SessionExpired,
            RelayPhase::IdentityBusy,
            RelayPhase::Connecting,
            RelayPhase::Authenticating,
            RelayPhase::Unauthenticated,
            RelayPhase::AuthRejected,
            RelayPhase::Offline,
        ] {
            assert!(!p.reachable(), "{} must not read as reachable", p.as_str());
        }
        assert!(RelayPhase::Online.reachable());
    }

    // ── status cell ──────────────────────────────────────────────────────────

    #[test]
    fn status_reports_phase_changes_and_keeps_since_on_detail_only_updates() {
        let h = RelayStatusHandle::new(RelayPhase::SignedOut, "waiting");
        let first = h.get().since;
        assert!(h.set(RelayPhase::Connecting, "dialling"), "a phase change is reported");
        let since = h.get().since;
        assert!(since >= first);
        assert!(!h.set(RelayPhase::Connecting, "dialling again"), "same phase is not a transition");
        let s = h.get();
        assert_eq!(s.detail, "dialling again");
        assert_eq!(s.since, since, "since marks the phase change, not the detail");
    }

    #[test]
    fn status_serializes_snake_case() {
        let h = RelayStatusHandle::new(RelayPhase::Unauthenticated, "x");
        let v = serde_json::to_value(h.get()).unwrap();
        assert_eq!(v["state"], "unauthenticated");
        assert_eq!(v["detail"], "x");
    }

    #[test]
    fn disabled_handle_says_so() {
        assert_eq!(RelayStatusHandle::disabled().get().state, RelayPhase::Disabled);
    }

    #[tokio::test]
    async fn subscribers_see_updates() {
        let h = RelayStatusHandle::new(RelayPhase::SignedOut, "");
        let mut rx = h.subscribe();
        h.set(RelayPhase::Online, "ok");
        rx.changed().await.unwrap();
        assert_eq!(rx.borrow().state, RelayPhase::Online);
    }
}

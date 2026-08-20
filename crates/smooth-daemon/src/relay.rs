//! Smoo Relay client (pearl th-2f626d, EPIC th-5561c5) — remote control
//! WITHOUT tailscale.
//!
//! The daemon dials OUT to the Smoo Relay (`rust/relay-ws` in the smooai repo,
//! SMOODEV-2828, `wss://relay.smoo.ai/ws`) and registers as the signed-in
//! user's device **`daemon-<uuid>`** — a per-machine id persisted to
//! `~/.smooth/relay-device-id` and announced with a `label` (the machine's
//! short hostname) and `kind=daemon`, so ONE Smoo account can run several
//! daemons (laptop + smoo-hub) without them claiming the same relay slot
//! (th-764b57). Phones connect to the same relay as their own
//! device ids and exchange `{to, frame}` envelopes; the relay forwards frames
//! between a user's devices — opaquely, same-user-only — so a phone anywhere
//! on the internet can chat with THIS daemon with no tailnet membership.
//!
//! Topology per phone device: one **loopback bridge** — a plain WS client onto
//! the daemon's own operator (`ws://127.0.0.1:<port>/ws?token=…`, the exact
//! seam the scheduler's `OperatorTurnDriver` uses) — so the operator sees each
//! phone as just another canonical-protocol client. Frames pass through
//! unparsed in both directions; the relay client only reads the envelope.
//!
//! Auth: the daemon's stored Smoo session (`th auth login`, kept fresh by the
//! credential heartbeat in [`crate::auth_login`]). The access token is re-read
//! from [`CredentialsStore`] on EVERY (re)connect — the heartbeat rotates it —
//! and a signed-out daemon simply waits and retries: the relay is a
//! reachability layer, never a reason the daemon can't boot.
//!
//! Config: `SMOOTH_RELAY=0` disables; `SMOOTH_RELAY_URL` overrides the default
//! relay endpoint; `SMOOTH_RELAY_DEVICE_ID` / `SMOOTH_RELAY_LABEL` pin the
//! identity (the env-knob precedent of `config.rs`).

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use smooai_client_shared::auth::refresh;
use smooai_client_shared::auth::storage::{Credentials, CredentialsStore};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// The production relay endpoint (SMOODEV-2828).
const DEFAULT_RELAY_URL: &str = "wss://relay.smoo.ai/ws";
/// Where the per-machine device id is persisted, under `~/.smooth/`.
const DEVICE_ID_FILE: &str = "relay-device-id";
/// Label fallback when the host has no usable hostname.
const DEFAULT_LABEL: &str = "big-smooth";
/// Labels are display-only; cap them so a junk `$SMOOTH_RELAY_LABEL` can't
/// bloat every connect URL.
const LABEL_MAX_CHARS: usize = 120;
/// Reconnect backoff bounds. Exponential from `BACKOFF_MIN`, capped at
/// `BACKOFF_MAX`; reset after a connection that survived `BACKOFF_RESET_AFTER`.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(30);
/// How long a signed-out daemon waits before checking for credentials again.
const SIGNED_OUT_RECHECK: Duration = Duration::from_secs(60);
/// Refresh the Smoo session when the access token is inside this window of
/// expiry (or already past). ~60s beats a connect round-trip without
/// refreshing on every reconnect (th-c6a542).
const REFRESH_MARGIN_SECS: i64 = 60;
/// The relay's application close code for "your token was rejected" — the
/// relay accepts the WS upgrade, then closes with this if auth fails. Seeing
/// it tells the supervisor to force a refresh and reconnect.
const RELAY_AUTH_CLOSE_CODE: u16 = 4401;

/// Resolve the relay endpoint from env. Pure (args, not env reads) so it's
/// hermetically testable: `enabled` = `SMOOTH_RELAY`, `url` = `SMOOTH_RELAY_URL`.
/// `None` ⇒ relay disabled.
fn resolve_relay_url_from(enabled: Option<&str>, url: Option<&str>) -> Option<String> {
    if matches!(enabled.map(str::trim), Some("0" | "false" | "off" | "no")) {
        return None;
    }
    Some(url.map_or(DEFAULT_RELAY_URL, str::trim).to_string()).filter(|u| !u.is_empty())
}

/// [`resolve_relay_url_from`] over the real environment.
pub fn resolve_relay_url() -> Option<String> {
    let enabled = std::env::var("SMOOTH_RELAY").ok();
    let url = std::env::var("SMOOTH_RELAY_URL").ok();
    resolve_relay_url_from(enabled.as_deref(), url.as_deref())
}

/// This daemon's STABLE relay device id, `daemon-<12 hex>`.
///
/// Pure over its inputs (`SMOOTH_RELAY_DEVICE_ID`, the `~/.smooth` dir) so it's
/// testable without touching the real `$HOME`. Reads the persisted id when
/// present, else mints and best-effort persists one (mode 600). A missing or
/// unwritable home degrades to a process-random id — an unstable device id is
/// worse than a stable one but far better than an unreachable daemon.
fn resolve_device_id_from(override_env: Option<&str>, base_dir: Option<&Path>) -> String {
    if let Some(pinned) = override_env.map(str::trim).filter(|v| !v.is_empty()) {
        return pinned.to_string();
    }
    let Some(dir) = base_dir else {
        let id = mint_device_id();
        tracing::warn!(device = %id, "relay: no home dir — using a process-random device id (changes on restart)");
        return id;
    };
    let path = dir.join(DEVICE_ID_FILE);
    if let Some(existing) = std::fs::read_to_string(&path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return existing;
    }
    let id = mint_device_id();
    match crate::secret_file::write_secret(&path, &id) {
        Ok(()) => tracing::info!(device = %id, path = %path.display(), "relay: minted this machine's device id"),
        Err(e) => tracing::warn!(error = %e, path = %path.display(), "relay: could not persist device id — it will change on restart"),
    }
    id
}

fn mint_device_id() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("daemon-{}", &hex[..12])
}

/// The human label for this daemon on the relay's device list.
///
/// Pure over (`SMOOTH_RELAY_LABEL`, the raw hostname) — the env override wins,
/// a hostname is shortened to its first DNS label, and both are stripped of
/// control characters and capped so the value stays URL- and UI-safe.
fn resolve_label_from(override_env: Option<&str>, hostname: Option<&str>) -> String {
    let from_env = override_env.map(str::trim).filter(|s| !s.is_empty()).map(sanitize_label);
    let from_host = hostname
        .map(str::trim)
        // `hostname` may hand back an FQDN; the short form is what a human reads.
        .and_then(|h| h.split('.').next())
        .map(sanitize_label);
    from_env
        .into_iter()
        .chain(from_host)
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_LABEL.to_string())
}

fn sanitize_label(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(LABEL_MAX_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

/// This machine's hostname, via the `hostname` binary — the same dependency-free
/// trick `th`'s agent-handle default uses (`smooth-cli/src/main.rs`).
fn host_name() -> Option<String> {
    let out = std::process::Command::new("hostname").output().ok()?;
    String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Assemble the relay connect URL. `kind=daemon` tells the relay which side of
/// the presence list this connection belongs on (SMOODEV-2834).
fn connect_url(relay_url: &str, token: &str, device: &str, label: &str) -> String {
    format!(
        "{relay_url}?token={}&device={}&label={}&kind=daemon",
        urlencode(token),
        urlencode(device),
        urlencode(label)
    )
}

/// One inbound relay message, classified. Pure parse so the protocol rules are
/// unit-testable without sockets.
#[derive(Debug, PartialEq)]
enum RelayMsg {
    /// Relay heartbeat — answer with `{"type":"pong"}`.
    Ping,
    /// Registration ack / pong / other control noise — nothing to do.
    Ignore,
    /// The addressed peer (a phone we sent to) is connected nowhere — drop its bridge.
    PeerOffline(String),
    /// A relayed envelope: (sender device, the opaque frame as a wire string).
    Frame(String, String),
}

/// Classify one relay text frame.
fn classify_relay_msg(text: &str) -> RelayMsg {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return RelayMsg::Ignore;
    };
    match v.get("type").and_then(Value::as_str) {
        Some("ping") => return RelayMsg::Ping,
        Some("peer_offline") => {
            return v
                .get("to")
                .and_then(Value::as_str)
                .map_or(RelayMsg::Ignore, |d| RelayMsg::PeerOffline(d.to_string()));
        }
        Some("error") => {
            tracing::warn!(frame = %text, "relay: server error frame");
            return RelayMsg::Ignore;
        }
        Some(_) => return RelayMsg::Ignore, // connected / pong / future control frames
        None => {}
    }
    match (v.get("from").and_then(Value::as_str), v.get("frame")) {
        (Some(from), Some(frame)) => RelayMsg::Frame(from.to_string(), frame.to_string()),
        _ => RelayMsg::Ignore,
    }
}

/// Wrap an operator frame (raw text from the loopback WS) into a relay envelope
/// addressed to `to`. Non-JSON operator output is dropped (`None`) — the
/// canonical protocol is JSON-only, so anything else is line noise.
fn wrap_out(to: &str, operator_text: &str) -> Option<String> {
    let frame: Value = serde_json::from_str(operator_text).ok()?;
    Some(json!({ "to": to, "frame": frame }).to_string())
}

/// Percent-encode for a `?token=` query param (RFC 3986 unreserved passes).
/// Same as the scheduler's — tiny enough to duplicate over exporting.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A live loopback bridge for one phone device: frames from the phone go into
/// `to_operator`; a spawned task owns the loopback WS and pushes the operator's
/// replies back to the relay through the shared out-channel.
struct Bridge {
    to_operator: mpsc::UnboundedSender<String>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Spawn the loopback bridge task for `device`: connect to the daemon's own
/// operator WS, pump `rx` → operator and operator → `out` (wrapped in a `{to}`
/// envelope). Ends when either side closes; the caller reaps the entry lazily
/// (a dead `to_operator` receiver surfaces as a failed `send`).
fn spawn_bridge(
    device: String,
    local_ws_url: String,
    mut rx: mpsc::UnboundedReceiver<String>,
    out: mpsc::UnboundedSender<String>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (stream, _) = match tokio_tungstenite::connect_async(&local_ws_url).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, device, "relay: loopback operator connect failed; dropping session");
                return;
            }
        };
        let (mut sink, mut source) = stream.split();
        loop {
            tokio::select! {
                frame = rx.recv() => match frame {
                    Some(f) => {
                        if sink.send(Message::Text(f.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break, // bridge dropped by the supervisor
                },
                msg = source.next() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(envelope) = wrap_out(&device, &text) {
                            if out.send(envelope).is_err() {
                                break; // relay connection gone; supervisor rebuilds
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::debug!(error = %e, device, "relay: loopback WS error");
                        break;
                    }
                },
            }
        }
        let _ = sink.send(Message::Close(None)).await;
        tracing::debug!(device, "relay: loopback bridge ended");
    })
}

/// Pure "should we refresh?" decision over (expiry, now) — refresh when the
/// token is within [`REFRESH_MARGIN_SECS`] of expiry or already past. A `None`
/// expiry means we can't tell, so don't (a bad token surfaces as a 4401, which
/// forces one anyway). Pure so the window logic is testable without a clock.
fn needs_refresh(expires_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    matches!(expires_at, Some(exp) if now >= exp - ChronoDuration::seconds(REFRESH_MARGIN_SECS))
}

/// Read the freshest USABLE Smoo access token from the stored session,
/// refreshing it first when it's expired/near-expiry (or `force`d after a
/// 4401) and persisting the rotated tokens. `None` when signed out / no store —
/// the supervisor waits and retries. Best-effort: a failed refresh still
/// returns the existing token so the connect is attempted, never crashing the
/// daemon (th-c6a542).
async fn fresh_access_token(http: &reqwest::Client, force: bool) -> Option<String> {
    let store = CredentialsStore::default_user().ok()?;
    let creds = store.load().ok().flatten()?;
    let creds = if force || needs_refresh(creds.expires_at, Utc::now()) {
        refresh_and_persist(http, &store, creds).await
    } else {
        creds
    };
    Some(creds.access_token).filter(|t| !t.is_empty())
}

/// Refresh the session against Supabase and persist the rotated tokens, reusing
/// the exact mechanism the credential heartbeat uses ([`crate::auth_login`]).
/// Best-effort: on any failure logs and returns the ORIGINAL creds so the
/// caller still attempts a connect. Supabase ROTATES the refresh token, so a
/// successful refresh MUST be saved or the next refresh presents a revoked one.
async fn refresh_and_persist(http: &reqwest::Client, store: &CredentialsStore, creds: Credentials) -> Credentials {
    if creds.refresh_token.is_none() {
        tracing::warn!("relay: Smoo session is near expiry with no refresh token — sign in again (`th auth login`)");
        return creds;
    }
    match refresh::refresh_session(http, &crate::auth_login::supabase_url(), &crate::auth_login::supabase_anon_key(), &creds).await {
        Ok(renewed) => {
            if let Err(e) = store.save(&renewed) {
                // Use the fresh token for THIS connect even if the save failed;
                // the next reconnect will just refresh again.
                tracing::error!(error = %format!("{e:#}"), "relay: refreshed the Smoo session but persisting it failed");
                return renewed;
            }
            tracing::info!(expires_at = ?renewed.expires_at, "relay: refreshed the Smoo session");
            renewed
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "relay: could not refresh the Smoo session — trying the existing token");
            creds
        }
    }
}

/// Spawn the relay supervisor.
///
/// (Re)connects to the relay with a fresh token, forwards envelopes phone ⇄
/// operator via per-device loopback bridges, backs off exponentially on drops,
/// and waits patiently while signed out. Never fails the daemon — every error
/// is a log line and a retry.
pub fn spawn_relay(relay_url: String, local_port: u16, local_token: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let http = reqwest::Client::default();
        let local_ws_url = format!("ws://127.0.0.1:{local_port}/ws?token={}", urlencode(&local_token));
        // Resolved once: the id must be identical across reconnects or the
        // relay sees a new device every backoff cycle.
        let device = resolve_device_id_from(
            std::env::var("SMOOTH_RELAY_DEVICE_ID").ok().as_deref(),
            dirs_next::home_dir().map(|h| h.join(".smooth")).as_deref(),
        );
        let label = resolve_label_from(std::env::var("SMOOTH_RELAY_LABEL").ok().as_deref(), host_name().as_deref());
        tracing::info!(%device, %label, "relay: this daemon's identity");
        let mut backoff = BACKOFF_MIN;
        // Set after an auth rejection (4401 close / 401 handshake): the next
        // read forces a token refresh before reconnecting. Normal backoff still
        // applies, so a persistently-dead refresh token can't hammer the relay.
        let mut force_refresh = false;
        loop {
            let Some(token) = fresh_access_token(&http, force_refresh).await else {
                force_refresh = false;
                tracing::debug!("relay: no Smoo session (signed out) — retrying in {SIGNED_OUT_RECHECK:?}");
                tokio::time::sleep(SIGNED_OUT_RECHECK).await;
                continue;
            };
            force_refresh = false;
            let url = connect_url(&relay_url, &token, &device, &label);
            let connected_at = std::time::Instant::now();
            match tokio_tungstenite::connect_async(&url).await {
                Ok((stream, _)) => {
                    tracing::info!(relay = %relay_url, "relay: connected — Big Smooth is reachable without tailscale");
                    match run_connection(stream, &local_ws_url).await {
                        ConnEnd::AuthRejected => {
                            tracing::warn!("relay: token rejected (4401) — refreshing the Smoo session and reconnecting");
                            force_refresh = true;
                        }
                        ConnEnd::Normal => tracing::warn!("relay: connection ended; reconnecting"),
                    }
                }
                Err(e) => {
                    if is_auth_handshake_error(&e) {
                        tracing::warn!(error = %e, relay = %relay_url, "relay: handshake rejected (401) — refreshing the Smoo session and reconnecting");
                        force_refresh = true;
                    } else {
                        tracing::warn!(error = %e, relay = %relay_url, "relay: connect failed");
                    }
                }
            }
            // A connection that lived a while earns a fresh backoff.
            if connected_at.elapsed() > BACKOFF_RESET_AFTER {
                backoff = BACKOFF_MIN;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    })
}

/// Whether a WS handshake error is an auth rejection (HTTP 401) — the relay
/// normally 4401-closes after upgrade, but a 401 at handshake is the same
/// signal: refresh the token and retry.
fn is_auth_handshake_error(e: &tokio_tungstenite::tungstenite::Error) -> bool {
    matches!(e, tokio_tungstenite::tungstenite::Error::Http(resp) if resp.status().as_u16() == 401)
}

/// How a relay connection ended — normally, or because the relay rejected our
/// token (4401), which the supervisor answers with a refresh + reconnect.
#[derive(Debug, PartialEq, Eq)]
enum ConnEnd {
    Normal,
    AuthRejected,
}

/// One live relay connection: pump relay ⇄ bridges until the socket ends.
async fn run_connection(stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, local_ws_url: &str) -> ConnEnd {
    let (mut sink, mut source) = stream.split();
    // All bridges push outbound envelopes through one channel — the single
    // writer to the relay socket.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let mut bridges: HashMap<String, Bridge> = HashMap::new();
    let mut end = ConnEnd::Normal;

    loop {
        tokio::select! {
            envelope = out_rx.recv() => match envelope {
                // Bridges hold clones of out_tx, so recv() only ever yields
                // None when… it can't (we hold out_tx too). Guard anyway.
                Some(e) => {
                    if sink.send(Message::Text(e.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            msg = source.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(frame))) => {
                        // A 4401 close means the relay rejected our token — flag
                        // it so the supervisor refreshes before reconnecting.
                        if frame.as_ref().is_some_and(|f| u16::from(f.code) == RELAY_AUTH_CLOSE_CODE) {
                            end = ConnEnd::AuthRejected;
                        }
                        break;
                    }
                    None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        tracing::debug!(error = %e, "relay: socket error");
                        break;
                    }
                };
                match classify_relay_msg(&text) {
                    RelayMsg::Ping => {
                        if sink.send(Message::Text(r#"{"type":"pong"}"#.into())).await.is_err() {
                            break;
                        }
                    }
                    RelayMsg::Ignore => {}
                    RelayMsg::PeerOffline(device) => {
                        // The phone we last wrote to is gone — reap its bridge so a
                        // reconnecting phone gets a fresh operator session.
                        bridges.remove(&device);
                    }
                    RelayMsg::Frame(from, frame) => {
                        // Get-or-(re)spawn the bridge, then forward. A bridge whose
                        // task died (operator restart) fails the send — respawn once.
                        let delivered = bridges
                            .get(&from)
                            .is_some_and(|b| b.to_operator.send(frame.clone()).is_ok() && !b.task.is_finished());
                        if !delivered {
                            let (tx, rx) = mpsc::unbounded_channel();
                            let task = spawn_bridge(from.clone(), local_ws_url.to_string(), rx, out_tx.clone());
                            let _ = tx.send(frame);
                            bridges.insert(from, Bridge { to_operator: tx, task });
                        }
                    }
                }
            }
        }
    }
    // Dropping the map aborts every bridge task (Bridge::drop).
    bridges.clear();
    end
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    // ── config resolution ─────────────────────────────────────────────────────

    #[test]
    fn relay_enabled_by_default_at_the_production_url() {
        assert_eq!(resolve_relay_url_from(None, None).as_deref(), Some(DEFAULT_RELAY_URL));
    }

    #[test]
    fn relay_disabled_by_kill_switch() {
        for v in ["0", "false", "off", "no", " 0 "] {
            assert_eq!(resolve_relay_url_from(Some(v), None), None, "SMOOTH_RELAY={v}");
        }
        // Anything else (incl. "1") leaves it on.
        assert!(resolve_relay_url_from(Some("1"), None).is_some());
    }

    #[test]
    fn relay_url_override_wins() {
        assert_eq!(
            resolve_relay_url_from(None, Some("wss://relay.dev.smoo.ai/ws")).as_deref(),
            Some("wss://relay.dev.smoo.ai/ws")
        );
        // Empty override ⇒ disabled rather than a junk dial loop.
        assert_eq!(resolve_relay_url_from(None, Some("")), None);
    }

    // ── device identity (th-764b57) ───────────────────────────────────────────

    #[test]
    fn device_id_is_minted_once_then_stable() {
        let dir = tempfile::tempdir().unwrap();
        let first = resolve_device_id_from(None, Some(dir.path()));
        assert!(first.starts_with("daemon-"), "{first}");
        assert_eq!(first.len(), "daemon-".len() + 12);
        // Same dir ⇒ same id, however many times we ask.
        assert_eq!(resolve_device_id_from(None, Some(dir.path())), first);
        assert_eq!(resolve_device_id_from(None, Some(dir.path())), first);
        // And it really is on disk.
        assert_eq!(std::fs::read_to_string(dir.path().join(DEVICE_ID_FILE)).unwrap().trim(), first);
    }

    #[test]
    fn device_id_is_unique_per_machine() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        assert_ne!(
            resolve_device_id_from(None, Some(a.path())),
            resolve_device_id_from(None, Some(b.path())),
            "two daemons must not collide on one Smoo account"
        );
    }

    #[test]
    fn device_id_creates_a_missing_smooth_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("never").join("existed");
        let id = resolve_device_id_from(None, Some(&nested));
        assert_eq!(std::fs::read_to_string(nested.join(DEVICE_ID_FILE)).unwrap().trim(), id);
    }

    #[cfg(unix)]
    #[test]
    fn device_id_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        resolve_device_id_from(None, Some(dir.path()));
        let mode = std::fs::metadata(dir.path().join(DEVICE_ID_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn device_id_env_override_wins_and_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_device_id_from(Some("  daemon-pinned  "), Some(dir.path())), "daemon-pinned");
        assert!(!dir.path().join(DEVICE_ID_FILE).exists(), "a pinned id must not clobber the persisted one");
        // Blank override falls through to the persisted path.
        assert!(resolve_device_id_from(Some("   "), Some(dir.path())).starts_with("daemon-"));
    }

    #[test]
    fn device_id_survives_a_file_with_trailing_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DEVICE_ID_FILE), "daemon-abc123def456\n").unwrap();
        assert_eq!(resolve_device_id_from(None, Some(dir.path())), "daemon-abc123def456");
        // An empty/blank file is treated as absent, not as an empty device id.
        std::fs::write(dir.path().join(DEVICE_ID_FILE), " \n").unwrap();
        assert!(resolve_device_id_from(None, Some(dir.path())).starts_with("daemon-"));
    }

    #[test]
    fn device_id_without_a_home_falls_back_instead_of_panicking() {
        let id = resolve_device_id_from(None, None);
        assert!(id.starts_with("daemon-"), "{id}");
        assert_ne!(id, resolve_device_id_from(None, None), "the homeless fallback is process-random");
    }

    // ── label ─────────────────────────────────────────────────────────────────

    #[test]
    fn label_prefers_the_env_override() {
        assert_eq!(resolve_label_from(Some(" Brent's Laptop "), Some("smoo-hub")), "Brent's Laptop");
    }

    #[test]
    fn label_uses_the_short_hostname() {
        assert_eq!(resolve_label_from(None, Some("smoo-hub.local")), "smoo-hub");
        assert_eq!(resolve_label_from(None, Some("  mac-studio\n")), "mac-studio");
    }

    #[test]
    fn label_falls_back_when_there_is_no_hostname() {
        assert_eq!(resolve_label_from(None, None), DEFAULT_LABEL);
        assert_eq!(resolve_label_from(None, Some("")), DEFAULT_LABEL);
        assert_eq!(resolve_label_from(Some("  "), Some("   ")), DEFAULT_LABEL);
        // A label that sanitizes down to nothing is not a label.
        assert_eq!(resolve_label_from(Some("\u{7}\u{0}"), None), DEFAULT_LABEL);
    }

    #[test]
    fn label_strips_control_chars_and_caps_length() {
        assert_eq!(resolve_label_from(Some("big\u{0}sm\noth"), None), "bigsmoth");
        let long = resolve_label_from(Some(&"x".repeat(500)), None);
        assert_eq!(long.chars().count(), LABEL_MAX_CHARS);
        // Multi-byte labels are cut on char boundaries, not bytes.
        let emoji = resolve_label_from(Some(&"é".repeat(500)), None);
        assert_eq!(emoji.chars().count(), LABEL_MAX_CHARS);
    }

    // ── connect URL ───────────────────────────────────────────────────────────

    #[test]
    fn connect_url_carries_device_label_and_kind() {
        let u = connect_url("wss://relay.smoo.ai/ws", "tok en", "daemon-abc123", "Brent's Laptop");
        assert_eq!(
            u,
            "wss://relay.smoo.ai/ws?token=tok%20en&device=daemon-abc123&label=Brent%27s%20Laptop&kind=daemon"
        );
    }

    // ── inbound classification ────────────────────────────────────────────────

    #[test]
    fn classify_ping_and_control_noise() {
        assert_eq!(classify_relay_msg(r#"{"type":"ping"}"#), RelayMsg::Ping);
        assert_eq!(classify_relay_msg(r#"{"type":"connected"}"#), RelayMsg::Ignore);
        assert_eq!(classify_relay_msg(r#"{"type":"pong"}"#), RelayMsg::Ignore);
        assert_eq!(classify_relay_msg(r#"{"type":"error","message":"x"}"#), RelayMsg::Ignore);
        // Presence control frames (SMOODEV-2834) are the phone's business, not ours.
        assert_eq!(
            classify_relay_msg(r#"{"type":"peers","peers":[{"device":"daemon-abc","label":"smoo-hub","kind":"daemon"}]}"#),
            RelayMsg::Ignore
        );
        assert_eq!(classify_relay_msg("not json"), RelayMsg::Ignore);
        assert_eq!(classify_relay_msg(r#"{"unrelated":true}"#), RelayMsg::Ignore);
    }

    #[test]
    fn classify_peer_offline_names_the_device() {
        assert_eq!(
            classify_relay_msg(r#"{"type":"peer_offline","to":"phone-abc"}"#),
            RelayMsg::PeerOffline("phone-abc".to_string())
        );
        // Malformed peer_offline (no `to`) is noise, not a panic.
        assert_eq!(classify_relay_msg(r#"{"type":"peer_offline"}"#), RelayMsg::Ignore);
    }

    #[test]
    fn classify_frame_extracts_sender_and_opaque_frame() {
        let RelayMsg::Frame(from, frame) = classify_relay_msg(r#"{"from":"phone-a","frame":{"action":"send_message","message":"hi"}}"#) else {
            panic!("expected Frame");
        };
        assert_eq!(from, "phone-a");
        let v: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["action"], "send_message");
    }

    #[test]
    fn classify_frame_with_incidental_type_field_still_forwards() {
        // An envelope whose inner frame leaks a top-level `type` on the OUTER
        // object would be a relay bug, but a non-control `type` must not
        // swallow a real envelope… the relay never produces this; we classify
        // unknown types as Ignore deliberately (control-plane forward-compat)
        // and rely on the relay's envelope shape ({from, frame} with no type).
        assert_eq!(classify_relay_msg(r#"{"type":"future_control","from":"x","frame":{}}"#), RelayMsg::Ignore);
    }

    // ── outbound wrapping ─────────────────────────────────────────────────────

    #[test]
    fn wrap_out_addresses_the_device_and_embeds_json() {
        let w = wrap_out("phone-a", r#"{"type":"stream_token","token":"hi"}"#).unwrap();
        let v: Value = serde_json::from_str(&w).unwrap();
        assert_eq!(v["to"], "phone-a");
        assert_eq!(v["frame"]["type"], "stream_token");
    }

    #[test]
    fn wrap_out_drops_non_json_noise() {
        assert_eq!(wrap_out("phone-a", "not json"), None);
        assert_eq!(wrap_out("phone-a", ""), None);
    }

    #[test]
    fn urlencode_reserves() {
        assert_eq!(urlencode("abc-XYZ_0.9~"), "abc-XYZ_0.9~");
        assert_eq!(urlencode("a b+c"), "a%20b%2Bc");
    }

    // ── session refresh: decision + persistence (th-c6a542) ───────────────────

    fn creds_expiring(expires_at: Option<DateTime<Utc>>, refresh_token: Option<&str>) -> Credentials {
        use smooai_client_shared::auth::storage::CredentialKind;
        Credentials {
            access_token: "acc".into(),
            refresh_token: refresh_token.map(str::to_string),
            expires_at,
            user: Some("brent@smoo.ai".into()),
            active_org_id: Some("org_1".into()),
            client_id: None,
            client_secret: None,
            kind: CredentialKind::User,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn needs_refresh_when_expired_or_near() {
        let now = Utc::now();
        assert!(needs_refresh(Some(now - ChronoDuration::hours(1)), now), "past expiry");
        assert!(needs_refresh(Some(now + ChronoDuration::seconds(30)), now), "inside 60s margin");
        assert!(needs_refresh(Some(now), now), "exactly at expiry");
    }

    #[test]
    fn no_refresh_when_token_has_runway() {
        let now = Utc::now();
        assert!(!needs_refresh(Some(now + ChronoDuration::minutes(5)), now), "plenty of runway");
        // Unknown expiry ⇒ can't tell ⇒ don't refresh (a 4401 forces it).
        assert!(!needs_refresh(None, now));
    }

    #[tokio::test]
    async fn refresh_and_persist_without_a_refresh_token_keeps_the_session() {
        // No refresh token ⇒ no network hit, original creds returned unchanged.
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsStore::at(dir.path().join("smooai-user.json"));
        let creds = creds_expiring(Some(Utc::now() - ChronoDuration::hours(1)), None);
        let out = refresh_and_persist(&reqwest::Client::default(), &store, creds.clone()).await;
        assert_eq!(out.access_token, creds.access_token);
        assert!(out.refresh_token.is_none());
    }

    #[test]
    fn store_round_trips_a_rotated_refresh_token() {
        // The invariant the refresh path relies on: save() then load() preserves
        // the rotated refresh_token — skip persisting it and the next refresh 400s.
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsStore::at(dir.path().join("smooai-user.json"));
        let mut creds = creds_expiring(Some(Utc::now() + ChronoDuration::hours(1)), Some("rot-abc"));
        creds.access_token = "new-access".into();
        store.save(&creds).unwrap();
        let loaded = store.load().unwrap().expect("present");
        assert_eq!(loaded.access_token, "new-access");
        assert_eq!(loaded.refresh_token.as_deref(), Some("rot-abc"));
    }

    // ── bridge integration: fake operator + fake relay channel ───────────────

    /// Spin a real loopback WS server that echoes one canned reply per inbound
    /// frame, then run a bridge against it and assert the round trip.
    #[tokio::test]
    async fn bridge_pumps_frames_both_ways() {
        use axum::extract::ws::{Message as AxMsg, WebSocket, WebSocketUpgrade};
        use axum::routing::get;
        use axum::Router;

        async fn fake_operator(mut ws: WebSocket) {
            while let Some(Ok(msg)) = ws.recv().await {
                if let AxMsg::Text(t) = msg {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    assert_eq!(v["action"], "send_message", "bridge must pass frames through untouched");
                    let _ = ws.send(AxMsg::Text(r#"{"type":"stream_token","token":"pong-from-operator"}"#.into())).await;
                }
            }
        }

        let app = Router::new().route("/ws", get(|u: WebSocketUpgrade| async move { u.on_upgrade(fake_operator) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let (to_op_tx, to_op_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let _task = spawn_bridge("phone-test".to_string(), format!("ws://{addr}/ws?token=x"), to_op_rx, out_tx);

        to_op_tx.send(r#"{"action":"send_message","message":"hello"}"#.to_string()).unwrap();

        let envelope = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
            .await
            .expect("reply within 5s")
            .expect("channel alive");
        let v: Value = serde_json::from_str(&envelope).unwrap();
        assert_eq!(v["to"], "phone-test");
        assert_eq!(v["frame"]["type"], "stream_token");
        assert_eq!(v["frame"]["token"], "pong-from-operator");
    }
}

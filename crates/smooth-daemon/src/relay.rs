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
//! from [`CredentialsStore`] on EVERY (re)connect — the heartbeat rotates it.
//! A signed-out daemon dials nothing and waits: the relay is a reachability
//! layer, never a reason the daemon can't boot. The link follows the
//! credentials file too, not just the socket (th-37c286): a login dials at
//! once, a logout leaves, and a socket the relay never acknowledges with
//! `{"type":"connected"}` is reported as `unauthenticated`, never online —
//! rules in [`crate::relay_status`].
//!
//! **Flow channel (th-7f0af3).** An envelope whose frame carries
//! `"channel":"flow"` is bridged to the daemon's flow WS (`/api/flow/ws`)
//! instead of the operator WS — a second loopback bridge per phone. No
//! `channel` ⇒ operator, unchanged. Outbound `flow.output` to a phone is
//! coalesced to ~30 fps and split into ≤16 KiB frames; phones never see raw
//! scrollback.
//!
//! **End-to-end encryption (th-d98fde).** A paired phone's flow frames are
//! sealed on the phone and opened here — the relay brokers ciphertext only.
//! [`FlowGuard`] is the per-phone policy inside a flow bridge: it finishes
//! pairings, opens per-connection sessions, decrypts inbound data frames,
//! encrypts outbound ones, and rejects plaintext from a paired (or revoked)
//! phone with a visible `flow.error`. See `flow_e2e.rs` for the protocol.
//!
//! **Two daemons, one machine (th-a1bb12).** The SmoothFlow app runs its own
//! child daemon next to Big Smooth. Both used to read the same
//! `~/.smooth/relay-device-id`, connect as one device, and the relay's presence
//! flapped between two sockets — phones landed on whichever connected last. Now
//! the app pins a second id (`SMOOTH_RELAY_DEVICE_ID`, minted into
//! `~/.smooth/smoothflow-relay-device-id`) and announces `kind=flow`
//! (`SMOOTH_RELAY_KIND`), so the relay's device list carries Big Smooth and
//! SmoothFlow as two peers and SmoothFlow phones can prefer the flow one. As a
//! belt-and-braces guard every daemon also holds an advisory lock on its
//! device id (`~/.smooth/relay-locks/<device>.lock`): a second process on the
//! same machine that resolves the SAME id logs an error and stays off the
//! relay until the first lets go, instead of racing it.
//!
//! Config: `SMOOTH_RELAY=0` disables; `SMOOTH_RELAY_URL` overrides the default
//! relay endpoint; `SMOOTH_RELAY_DEVICE_ID` / `SMOOTH_RELAY_LABEL` pin the
//! identity and `SMOOTH_RELAY_KIND` (`daemon` | `flow`) the presence kind (the
//! env-knob precedent of `config.rs`).

use std::collections::HashMap;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use smooai_client_shared::auth::storage::{Credentials, CredentialsStore};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

use crate::flow_e2e::{self, E2eSession, Inbound, PairingState, PENDING_OUT_MAX};
use crate::relay_status::{on_cred_change, waiting_phase, CredDecision, CredView, Link, RelayPhase, RelayStatusHandle};

/// The production relay endpoint (SMOODEV-2828).
const DEFAULT_RELAY_URL: &str = "wss://relay.smoo.ai/ws";
/// Where the per-machine device id is persisted, under `~/.smooth/`.
const DEVICE_ID_FILE: &str = "relay-device-id";
/// Where per-device-id advisory locks live, under `~/.smooth/` (th-a1bb12).
const LOCK_DIR: &str = "relay-locks";
/// Label fallback when the host has no usable hostname.
const DEFAULT_LABEL: &str = "big-smooth";
/// Suffix a flow-only daemon (the SmoothFlow app's child) adds to its
/// hostname label so a phone's device list tells the two daemons apart.
const FLOW_LABEL_SUFFIX: &str = " · SmoothFlow";
/// Labels are display-only; cap them so a junk `$SMOOTH_RELAY_LABEL` can't
/// bloat every connect URL.
const LABEL_MAX_CHARS: usize = 120;
/// Reconnect backoff bounds. Exponential from `BACKOFF_MIN`, capped at
/// `BACKOFF_MAX`; reset after a connection that survived `BACKOFF_RESET_AFTER`.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(30);
/// Safety net for a signed-out daemon: re-check the credentials this often
/// even if the watcher never reports a change.
const SIGNED_OUT_RECHECK: Duration = Duration::from_secs(60);
/// How often the credential watcher re-reads the credentials file. It's a
/// small local read; 5s means a `th auth login` (or the heartbeat renewing an
/// expired session) puts the daemon on the relay within seconds instead of
/// waiting out a retry timer — or a restart (th-37c286).
const CRED_POLL: Duration = Duration::from_secs(5);
/// How long an open socket may wait for the relay's `{"type":"connected"}`
/// auth ack before we call it what it is — connected but NOT a peer — and
/// re-dial with a refreshed token (th-37c286).
const AUTH_ACK_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a connected socket may go without a single inbound frame before
/// we treat it as dead and re-dial. The relay pings every 30s
/// (`rust/relay-ws` `HEARTBEAT_INTERVAL`), so silence this long means the pings
/// stopped arriving: a half-open TCP leg after sleep or a network change. The
/// daemon only ever WRITES in reply to a ping, so without this watchdog such a
/// socket never errors and the daemon sits "connected" while the relay has
/// long since dropped it as a peer — phones see it offline for days (th-6c500f).
const RELAY_SILENCE_TIMEOUT: Duration = Duration::from_secs(75);
/// How often a daemon whose device id another local process holds re-checks
/// the lock (the other daemon may have quit).
const IDENTITY_RECHECK: Duration = Duration::from_secs(30);
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

/// Which side of the relay's device list this daemon sits on — the `?kind=`
/// of the connect URL (`rust/relay-ws` `presence::sanitize_kind`, SMOODEV-2834
/// / SMOODEV-3142). Anything the relay does not know degrades to `phone` there,
/// so this is a closed enum, never free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RelayKind {
    /// Big Smooth — the personal agent daemon (chat + flow engine).
    #[default]
    Daemon,
    /// A flow-only daemon: the SmoothFlow app's child (th-a1bb12).
    Flow,
}

impl RelayKind {
    /// The wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Flow => "flow",
        }
    }

    /// What this daemon calls itself in logs.
    const fn product(self) -> &'static str {
        match self {
            Self::Daemon => "Big Smooth",
            Self::Flow => "SmoothFlow",
        }
    }
}

/// `SMOOTH_RELAY_KIND` → [`RelayKind`]. Unset / empty / `daemon` ⇒ `Daemon`;
/// `flow` ⇒ `Flow` (case-insensitive, trimmed). Junk is logged and treated as
/// `Daemon` — the relay would have coerced it to `phone`, which would hide the
/// daemon from every picker.
fn resolve_kind_from(override_env: Option<&str>) -> RelayKind {
    match override_env.map(str::trim).filter(|v| !v.is_empty()) {
        None => RelayKind::Daemon,
        Some(v) if v.eq_ignore_ascii_case("daemon") => RelayKind::Daemon,
        Some(v) if v.eq_ignore_ascii_case("flow") => RelayKind::Flow,
        Some(v) => {
            tracing::warn!(value = %v, "relay: SMOOTH_RELAY_KIND must be `daemon` or `flow` — using `daemon`");
            RelayKind::Daemon
        }
    }
}

/// This daemon's relay identity — resolved ONCE at boot so the pairing QR and
/// the relay connection agree on the device id across reconnects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayIdentity {
    pub device: String,
    pub label: String,
    pub kind: RelayKind,
}

/// Resolve [`RelayIdentity`] from the environment + `~/.smooth`.
pub fn device_identity() -> RelayIdentity {
    let kind = resolve_kind_from(std::env::var("SMOOTH_RELAY_KIND").ok().as_deref());
    let device = resolve_device_id_from(
        std::env::var("SMOOTH_RELAY_DEVICE_ID").ok().as_deref(),
        dirs_next::home_dir().map(|h| h.join(".smooth")).as_deref(),
    );
    let label = resolve_label_from(std::env::var("SMOOTH_RELAY_LABEL").ok().as_deref(), host_name().as_deref(), kind);
    RelayIdentity { device, label, kind }
}

/// Holds this process's claim on a relay device id for its lifetime; dropping
/// it (or dying) releases the claim. Advisory, same mechanism as
/// `single_instance::InstanceLock`.
#[derive(Debug)]
pub struct IdentityLock {
    _file: File,
}

/// The outcome of trying to claim a device id on this machine.
#[derive(Debug)]
pub enum IdentityClaim {
    /// Ours now — keep the lock alive for as long as the identity is in use.
    Held(IdentityLock),
    /// Another live process on this machine holds the same device id.
    Busy,
    /// No lock dir / unwritable — nothing to enforce; the caller proceeds.
    Unavailable(String),
}

/// The file name a device id locks under: the relay grammar is
/// `[A-Za-z0-9._-]`, but `SMOOTH_RELAY_DEVICE_ID` is free text, so anything
/// else becomes `_`.
fn lock_file_name(device: &str) -> String {
    let safe: String = device
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .collect();
    format!("{safe}.lock")
}

/// Try to claim `device` under `<dir>/relay-locks/`. Pure over its dir for tests.
fn claim_identity(dir: Option<&Path>, device: &str) -> IdentityClaim {
    let Some(dir) = dir else {
        return IdentityClaim::Unavailable("no home dir".into());
    };
    let locks = dir.join(LOCK_DIR);
    if let Err(e) = std::fs::create_dir_all(&locks) {
        return IdentityClaim::Unavailable(format!("creating {}: {e}", locks.display()));
    }
    let path = locks.join(lock_file_name(device));
    let file = match File::options().create(true).truncate(false).write(true).open(&path) {
        Ok(f) => f,
        Err(e) => return IdentityClaim::Unavailable(format!("opening {}: {e}", path.display())),
    };
    match file.try_lock() {
        Ok(()) => IdentityClaim::Held(IdentityLock { _file: file }),
        Err(TryLockError::WouldBlock) => IdentityClaim::Busy,
        Err(TryLockError::Error(e)) => IdentityClaim::Unavailable(format!("locking {}: {e}", path.display())),
    }
}

/// Where [`claim_identity`] locks for the real daemon: `~/.smooth`.
fn identity_lock_dir() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".smooth"))
}

fn mint_device_id() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("daemon-{}", &hex[..12])
}

/// The human label for this daemon on the relay's device list.
///
/// Pure over (`SMOOTH_RELAY_LABEL`, the raw hostname, the kind) — the env
/// override wins verbatim; otherwise a hostname shortened to its first DNS
/// label, with ` · SmoothFlow` appended for a flow-only daemon so a phone's
/// device list reads `smoo-hub` / `smoo-hub · SmoothFlow`. Both are stripped of
/// control characters and capped so the value stays URL- and UI-safe.
fn resolve_label_from(override_env: Option<&str>, hostname: Option<&str>, kind: RelayKind) -> String {
    let from_env = override_env.map(str::trim).filter(|s| !s.is_empty()).map(sanitize_label);
    let from_host = hostname
        .map(str::trim)
        // `hostname` may hand back an FQDN; the short form is what a human reads.
        .and_then(|h| h.split('.').next())
        .map(sanitize_label)
        .filter(|s| !s.is_empty())
        .map(|host| match kind {
            RelayKind::Daemon => host,
            RelayKind::Flow => sanitize_label(&format!("{host}{FLOW_LABEL_SUFFIX}")),
        });
    from_env.into_iter().chain(from_host).find(|s| !s.is_empty()).unwrap_or_else(|| match kind {
        RelayKind::Daemon => DEFAULT_LABEL.to_string(),
        RelayKind::Flow => format!("{DEFAULT_LABEL}{FLOW_LABEL_SUFFIX}"),
    })
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

/// Assemble the relay connect URL. `kind=` tells the relay which side of the
/// presence list this connection belongs on (SMOODEV-2834; `flow` since
/// SMOODEV-3142).
fn connect_url(relay_url: &str, token: &str, device: &str, label: &str, kind: RelayKind) -> String {
    format!(
        "{relay_url}?token={}&device={}&label={}&kind={}",
        urlencode(token),
        urlencode(device),
        urlencode(label),
        kind.as_str()
    )
}

/// One inbound relay message, classified. Pure parse so the protocol rules are
/// unit-testable without sockets.
#[derive(Debug, PartialEq)]
enum RelayMsg {
    /// Relay heartbeat — answer with `{"type":"pong"}`.
    Ping,
    /// `{"type":"connected"}` — the relay verified our token and registered
    /// this daemon as a peer. Until this arrives we are NOT reachable, however
    /// open the socket looks (th-37c286).
    Connected,
    /// Pong / other control noise — nothing to do.
    Ignore,
    /// The addressed peer (a phone we sent to) is connected nowhere — drop its bridge.
    PeerOffline(String),
    /// A relayed envelope: (sender device, the opaque frame as a wire string).
    Frame(String, String),
    /// A relayed envelope for the flow channel (`"channel":"flow"` in the frame).
    FlowFrame(String, String),
}

/// Classify one relay text frame.
fn classify_relay_msg(text: &str) -> RelayMsg {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return RelayMsg::Ignore;
    };
    match v.get("type").and_then(Value::as_str) {
        Some("ping") => return RelayMsg::Ping,
        Some("connected") => return RelayMsg::Connected,
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
        Some(_) => return RelayMsg::Ignore, // pong / future control frames
        None => {}
    }
    match (v.get("from").and_then(Value::as_str), v.get("frame")) {
        (Some(from), Some(frame)) if smooth_flow::protocol::is_flow_frame(frame) => RelayMsg::FlowFrame(from.to_string(), frame.to_string()),
        (Some(from), Some(frame)) => RelayMsg::Frame(from.to_string(), frame.to_string()),
        _ => RelayMsg::Ignore,
    }
}

/// Phone cap on one `flow.output` frame's decoded bytes.
pub const PHONE_OUTPUT_MAX_BYTES: usize = 16 * 1024;
/// Phone cap on output frame rate (~30 fps).
pub const PHONE_OUTPUT_TICK: Duration = Duration::from_millis(33);

/// Coalesces `flow.output` bytes per session between ticks and re-emits them
/// as ≤[`PHONE_OUTPUT_MAX_BYTES`] frames — the phone-side throttle. Pure.
#[derive(Default)]
pub struct OutputCoalescer {
    pending: Vec<(String, Vec<u8>)>,
    seq: u64,
}

impl OutputCoalescer {
    /// Absorb one wire frame. Returns `false` (untouched) when it is not a
    /// `flow.output`, so the caller forwards it as-is.
    pub fn absorb(&mut self, frame_text: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(frame_text) else { return false };
        if v.get("type").and_then(Value::as_str) != Some("flow.output") {
            return false;
        }
        let Some(id) = v.get("id").and_then(Value::as_str) else { return false };
        let bytes = v
            .get("data_b64")
            .and_then(Value::as_str)
            .and_then(|b| base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b).ok())
            .unwrap_or_default();
        match self.pending.iter_mut().find(|(i, _)| i == id) {
            Some((_, buf)) => buf.extend(bytes),
            None => self.pending.push((id.to_string(), bytes)),
        }
        true
    }

    /// True when a tick would emit something.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Drain everything as wire frames, chunked to the byte cap.
    pub fn drain(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for (id, buf) in self.pending.drain(..) {
            for chunk in buf.chunks(PHONE_OUTPUT_MAX_BYTES) {
                self.seq += 1;
                out.push(
                    json!({
                        "channel": "flow",
                        "type": "flow.output",
                        "id": id,
                        "seq": self.seq,
                        "data_b64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, chunk),
                    })
                    .to_string(),
                );
            }
        }
        out
    }
}

/// Wrap an operator frame (raw text from the loopback WS) into a relay envelope
/// addressed to `to`. Non-JSON operator output is dropped (`None`) — the
/// canonical protocol is JSON-only, so anything else is line noise.
fn wrap_out(to: &str, operator_text: &str) -> Option<String> {
    let frame: Value = serde_json::from_str(operator_text).ok()?;
    Some(json!({ "to": to, "frame": frame }).to_string())
}

/// The daemon's own flow WS, for the per-phone flow bridge.
fn flow_ws_url(local_port: u16, token: &str) -> String {
    format!("ws://127.0.0.1:{local_port}/api/flow/ws?token={}", urlencode(token))
}

/// The bridge-map key for a phone's flow bridge (distinct from its operator
/// bridge, which is keyed by the bare device id).
fn flow_bridge_key(device: &str) -> String {
    format!("{device}\u{1}flow")
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

/// What a flow bridge does with one frame from the phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Forward this plaintext flow frame to the engine's flow WS.
    ToEngine(String),
    /// Send this wire frame back to the phone (enveloped by the bridge).
    ToPhone(String),
}

/// The per-phone end-to-end policy inside a flow bridge (th-d98fde). Pure over
/// its inputs — no sockets — so every branch is unit-testable.
pub struct FlowGuard {
    device: String,
    pairing: Arc<PairingState>,
    session: Option<E2eSession>,
    /// Engine frames that arrived for a paired phone before its session opened
    /// (the engine's on-connect `flow.hello` races the phone's `flow.e2e.open`).
    pending_out: Vec<String>,
}

impl FlowGuard {
    pub fn new(device: impl Into<String>, pairing: Arc<PairingState>) -> Self {
        Self {
            device: device.into(),
            pairing,
            session: None,
            pending_out: Vec::new(),
        }
    }

    /// Whether an encrypted session is open right now.
    pub const fn session_open(&self) -> bool {
        self.session.is_some()
    }

    /// One frame from the phone → actions.
    pub fn inbound(&mut self, frame_text: &str) -> Vec<Action> {
        let paired = self.pairing.is_paired(&self.device);
        match flow_e2e::classify(frame_text) {
            Inbound::Plain(text) => {
                if paired {
                    tracing::warn!(device = %self.device, "relay e2e: paired phone sent plaintext — rejected");
                    vec![Action::ToPhone(flow_e2e::error_frame(
                        "e2e_required",
                        "this phone is paired with this Mac; send encrypted frames",
                    ))]
                } else if self.pairing.plaintext_required_to_fail() {
                    vec![Action::ToPhone(flow_e2e::error_frame(
                        "e2e_required",
                        "this Mac only accepts encrypted flow frames; pair it first",
                    ))]
                } else {
                    vec![Action::ToEngine(text)]
                }
            }
            Inbound::Pair {
                pairing_id,
                phone_public_key,
                n,
                ct,
            } => match self.pairing.complete(&self.device, &pairing_id, &phone_public_key, n, &ct) {
                Ok(reply) => {
                    // A fresh pairing invalidates any session under the old key.
                    self.session = None;
                    self.pending_out.clear();
                    vec![Action::ToPhone(reply)]
                }
                Err(e) => {
                    tracing::warn!(device = %self.device, error = %e, "relay e2e: pairing failed");
                    vec![Action::ToPhone(flow_e2e::error_frame(
                        "pair_failed",
                        "pairing failed — show a fresh QR and scan again",
                    ))]
                }
            },
            Inbound::Open { salt } => match self.pairing.open_session(&self.device, &salt) {
                Ok((session, reply)) => {
                    self.session = Some(session);
                    let mut actions = vec![Action::ToPhone(reply)];
                    for text in std::mem::take(&mut self.pending_out) {
                        actions.extend(self.outbound(&text).into_iter().map(Action::ToPhone));
                    }
                    actions
                }
                Err(e) => {
                    tracing::debug!(device = %self.device, error = %e, "relay e2e: session open refused");
                    vec![Action::ToPhone(flow_e2e::error_frame(
                        "e2e_not_paired",
                        "this phone is not paired with this Mac",
                    ))]
                }
            },
            Inbound::Data { n, ct } => {
                if !paired {
                    self.session = None;
                    return vec![Action::ToPhone(flow_e2e::error_frame("e2e_revoked", "this phone's pairing was revoked"))];
                }
                let Some(session) = self.session.as_mut() else {
                    return vec![Action::ToPhone(flow_e2e::error_frame("e2e_not_open", "send flow.e2e.open first"))];
                };
                match session.open_frame(n, &ct) {
                    Ok(plaintext) => {
                        self.pairing.touch(&self.device);
                        vec![Action::ToEngine(plaintext)]
                    }
                    Err(e) => {
                        tracing::warn!(device = %self.device, error = %e, "relay e2e: frame rejected");
                        vec![Action::ToPhone(flow_e2e::error_frame(
                            "e2e_bad_frame",
                            "frame did not authenticate or was replayed",
                        ))]
                    }
                }
            }
            Inbound::Malformed(why) => vec![Action::ToPhone(flow_e2e::error_frame("e2e_bad_frame", why))],
        }
    }

    /// One frame from the engine → wire frames for the phone (0..n).
    pub fn outbound(&mut self, text: &str) -> Vec<String> {
        let paired = self.pairing.is_paired(&self.device);
        if let Some(session) = self.session.as_mut() {
            if !paired {
                self.session = None;
                return Vec::new();
            }
            return match session.seal_frame(text) {
                Ok(wire) => vec![wire],
                Err(e) => {
                    tracing::warn!(device = %self.device, error = %e, "relay e2e: seal failed; frame dropped");
                    Vec::new()
                }
            };
        }
        if paired {
            if self.pending_out.len() < PENDING_OUT_MAX {
                self.pending_out.push(text.to_string());
            }
            return Vec::new();
        }
        if self.pairing.plaintext_required_to_fail() {
            return Vec::new();
        }
        vec![text.to_string()]
    }
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
fn spawn_bridge(device: String, local_ws_url: String, rx: mpsc::UnboundedReceiver<String>, out: mpsc::UnboundedSender<String>) -> tokio::task::JoinHandle<()> {
    spawn_bridge_with(device, local_ws_url, rx, out, None)
}

/// Engine → phone through the guard (or untouched for the operator flavour).
fn outbound_frames(flow: &mut Option<FlowGuard>, text: &str) -> Vec<String> {
    match flow.as_mut() {
        Some(guard) => guard.outbound(text),
        None => vec![text.to_string()],
    }
}

/// Why an inbound pump stopped: the loopback sink is gone (rebuild the bridge)
/// or the relay out-channel is gone (the supervisor is rebuilding everything).
enum PumpEnd {
    Loopback,
    Relay,
}

/// Apply one phone frame's actions: plaintext to the engine's WS, replies back
/// to the phone through the relay out-channel.
async fn apply_actions<S>(actions: Vec<Action>, device: &str, sink: &mut S, out: &mpsc::UnboundedSender<String>) -> Result<(), PumpEnd>
where
    S: SinkExt<Message> + Unpin,
{
    for action in actions {
        match action {
            Action::ToEngine(text) => {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    return Err(PumpEnd::Loopback);
                }
            }
            Action::ToPhone(text) => {
                if let Some(envelope) = wrap_out(device, &text) {
                    if out.send(envelope).is_err() {
                        return Err(PumpEnd::Relay);
                    }
                }
            }
        }
    }
    Ok(())
}

/// The bridge body. `flow = Some(guard)` is the flow-channel flavour: the
/// guard enforces end-to-end encryption per phone, and `flow.output` is
/// coalesced to ~30 fps and ≤16 KiB per frame before it reaches a phone.
fn spawn_bridge_with(
    device: String,
    local_ws_url: String,
    mut rx: mpsc::UnboundedReceiver<String>,
    out: mpsc::UnboundedSender<String>,
    mut flow: Option<FlowGuard>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let throttle_output = flow.is_some();
        let (stream, _) = match tokio_tungstenite::connect_async(&local_ws_url).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, device, "relay: loopback operator connect failed; dropping session");
                return;
            }
        };
        let (mut sink, mut source) = stream.split();
        let mut coalescer = OutputCoalescer::default();
        let mut tick = tokio::time::interval(PHONE_OUTPUT_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let actions_for = |flow: &mut Option<FlowGuard>, f: String| match flow.as_mut() {
            Some(guard) => guard.inbound(&f),
            None => vec![Action::ToEngine(f)],
        };
        // The frame that opened this bridge (a phone's `flow.pair` / `flow.e2e.open`
        // / nudge) is already queued: process it BEFORE reading the engine, so its
        // on-connect `flow.hello` is judged against the phone's real pairing state
        // rather than racing it (th-d98fde).
        while let Ok(f) = rx.try_recv() {
            match apply_actions(actions_for(&mut flow, f), &device, &mut sink, &out).await {
                Ok(()) => {}
                Err(PumpEnd::Loopback) => {
                    let _ = sink.send(Message::Close(None)).await;
                    return;
                }
                Err(PumpEnd::Relay) => return,
            }
        }
        'pump: loop {
            tokio::select! {
                frame = rx.recv() => match frame {
                    Some(f) => match apply_actions(actions_for(&mut flow, f), &device, &mut sink, &out).await {
                        Ok(()) => {}
                        Err(PumpEnd::Loopback) => break 'pump,
                        Err(PumpEnd::Relay) => return,
                    },
                    None => break, // bridge dropped by the supervisor
                },
                _ = tick.tick(), if throttle_output && !coalescer.is_empty() => {
                    for text in coalescer.drain() {
                        for wire in outbound_frames(&mut flow, &text) {
                            if let Some(envelope) = wrap_out(&device, &wire) {
                                if out.send(envelope).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                },
                msg = source.next() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        if throttle_output && coalescer.absorb(&text) {
                            continue;
                        }
                        for wire in outbound_frames(&mut flow, &text) {
                            if let Some(envelope) = wrap_out(&device, &wire) {
                                if out.send(envelope).is_err() {
                                    break 'pump; // relay connection gone; supervisor rebuilds
                                }
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

/// What the supervisor can dial with right now.
#[derive(Debug)]
enum TokenOutcome {
    /// A token to put on the wire, and the user it belongs to.
    Dial { token: String, user: Option<String> },
    /// Nothing usable — signed out, or expired beyond renewal. Carries the view
    /// so the status says which.
    NoSession(CredView),
}

/// Turn what a (possibly refreshed) credentials read produced into something
/// to dial. Pure over `now`: an access token already past expiry is NOT
/// dialled — the relay would reject it and the supervisor would hammer it —
/// the daemon waits for a sign-in instead (th-37c286).
fn token_outcome(creds: Option<Credentials>, now: DateTime<Utc>) -> TokenOutcome {
    match (CredView::observe(creds.as_ref(), now), creds) {
        (CredView::Usable { .. }, Some(c)) => TokenOutcome::Dial {
            token: c.access_token,
            user: c.user,
        },
        (view, _) => TokenOutcome::NoSession(view),
    }
}

/// Read the freshest USABLE Smoo access token from the stored session,
/// refreshing it first when it's expired/near-expiry (or `force`d after a
/// 4401) and persisting the rotated tokens. [`TokenOutcome::NoSession`] when
/// signed out / no store / expired beyond renewal — the supervisor waits for
/// the credentials to change. Best-effort: a failed refresh of a token that
/// still has runway returns the existing token so the connect is attempted,
/// never crashing the daemon (th-c6a542).
async fn fresh_access_token(http: &reqwest::Client, force: bool) -> TokenOutcome {
    let Some(store) = CredentialsStore::default_user().ok() else {
        return TokenOutcome::NoSession(CredView::SignedOut);
    };
    let creds = store.load().ok().flatten();
    let Some(creds) = creds else {
        return TokenOutcome::NoSession(CredView::SignedOut);
    };
    if !force && !needs_refresh(creds.expires_at, Utc::now()) {
        return token_outcome(Some(creds), Utc::now());
    }
    // Refresh under the SHARED credential lock (th-c6a542). The old path here
    // refreshed unlocked, so it raced the credential heartbeat and any `th`
    // process on the one rotating Supabase refresh token — two exchanges of the
    // same token trip reuse-detection and revoke the family, which is why
    // smoo-hub fell off the relay ~hourly until a full `th auth login`.
    // `only_if_due = !force`: a plain expiry refresh no-ops if a peer already did
    // it; a post-4401 `force` still refreshes. Best-effort: on any failure keep
    // the existing token so the connect is still attempted (a dead token
    // resurfaces as a 4401) and the daemon never crashes.
    let creds = match crate::auth_login::refresh_user_session_locked(
        http,
        &store,
        &crate::auth_login::supabase_url(),
        &crate::auth_login::supabase_anon_key(),
        !force,
    )
    .await
    {
        Ok(renewed) => {
            tracing::info!(expires_at = ?renewed.expires_at, "relay: refreshed the Smoo session");
            renewed
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "relay: could not refresh the Smoo session — trying the existing token");
            creds
        }
    };
    token_outcome(Some(creds), Utc::now())
}

/// Watch the Smoo credentials file and publish a [`CredView`] whenever what it
/// says changes — a `th auth login`, the credential heartbeat renewing the
/// session, a logout, a different user. The supervisor dials on these rather
/// than only on a socket drop, which is what left a daemon that booted before
/// sign-in off the relay until a restart (th-37c286).
///
/// A read error (the file caught mid-write) keeps the last view instead of
/// reporting "signed out" and dropping a healthy link.
fn spawn_credential_watch() -> watch::Receiver<CredView> {
    fn read() -> Option<CredView> {
        let Ok(store) = CredentialsStore::default_user() else {
            return Some(CredView::SignedOut);
        };
        store.load().ok().map(|creds| CredView::observe(creds.as_ref(), Utc::now()))
    }
    let (tx, rx) = watch::channel(read().unwrap_or(CredView::SignedOut));
    tokio::spawn(async move {
        while !tx.is_closed() {
            tokio::time::sleep(CRED_POLL).await;
            if let Some(view) = read() {
                tx.send_if_modified(|v| {
                    if *v == view {
                        return false;
                    }
                    *v = view;
                    true
                });
            }
        }
    });
    rx
}

/// Resolve when the credential view changes. Never resolves if the watcher is
/// gone, so a dead watcher can't turn the supervisor into a hot loop.
async fn creds_changed(rx: &mut watch::Receiver<CredView>) {
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The human sentence for a daemon waiting on a sign-in.
const fn waiting_detail(phase: RelayPhase) -> &'static str {
    match phase {
        RelayPhase::SessionExpired => {
            "The Smoo session expired and could not be renewed — sign in again (`smoo auth login`) and this daemon rejoins the relay on its own."
        }
        _ => "Not signed in to Smoo — nothing is dialled until you sign in (`smoo auth login`); the daemon then joins the relay on its own.",
    }
}

/// Record why a connection failed and say whether the next dial must force a
/// token refresh (the relay rejected the token, or never acknowledged it).
fn report_failed_end(status: &RelayStatusHandle, end: &ConnEnd) -> bool {
    match end {
        ConnEnd::AuthRejected => {
            status.set(
                RelayPhase::AuthRejected,
                "The relay rejected this daemon's Smoo token; refreshing the session and retrying.",
            );
            tracing::warn!("relay: token rejected — refreshing the Smoo session and reconnecting");
            true
        }
        ConnEnd::NoAck => {
            status.set(
                RelayPhase::Unauthenticated,
                format!(
                    "The relay accepted the socket but never authenticated this daemon (no ack in {}s), so it is NOT a peer — phones see it as offline. Re-dialling with a refreshed session.",
                    AUTH_ACK_TIMEOUT.as_secs()
                ),
            );
            true
        }
        ConnEnd::Silent => {
            status.set(
                RelayPhase::Offline,
                format!(
                    "The relay went silent (no heartbeat in {}s) — the socket was dead without saying so. Re-dialling.",
                    RELAY_SILENCE_TIMEOUT.as_secs()
                ),
            );
            false
        }
        ConnEnd::Normal | ConnEnd::CredsChanged | ConnEnd::SignedOut => {
            if status.get().state != RelayPhase::Offline {
                status.set(RelayPhase::Offline, "The relay connection dropped; reconnecting.");
            }
            tracing::warn!("relay: connection ended; reconnecting");
            false
        }
    }
}

/// Spawn the relay supervisor.
///
/// (Re)connects to the relay with a fresh token, forwards envelopes phone ⇄
/// operator via per-device loopback bridges, backs off exponentially on drops,
/// and waits for a sign-in while signed out. The link follows the credentials
/// store as well as the socket (th-37c286): a login dials at once, a logout
/// disconnects, a changed user re-authenticates, and a socket the relay never
/// acknowledges is reported as `unauthenticated` — never as online. Every
/// phase lands in `status` for `/api/flow/pairings` (the app's Phones pane).
/// Never fails the daemon — every error is a log line and a retry.
///
/// `identity` is resolved once by the caller ([`device_identity`]) — the id
/// must be identical across reconnects (or the relay sees a new device every
/// backoff cycle) and identical to what the pairing QR advertises. `pairing`
/// is the shared end-to-end pairing authority for the flow bridges.
pub fn spawn_relay(
    relay_url: String,
    local_port: u16,
    local_token: String,
    identity: RelayIdentity,
    pairing: Arc<PairingState>,
    status: RelayStatusHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let http = reqwest::Client::default();
        let local_ws_url = format!("ws://127.0.0.1:{local_port}/ws?token={}", urlencode(&local_token));
        let flow_ws_url = flow_ws_url(local_port, &local_token);
        let RelayIdentity { device, label, kind } = identity;
        tracing::info!(%device, %label, kind = kind.as_str(), "relay: this daemon's identity");
        let lock_dir = identity_lock_dir();
        let mut creds_rx = spawn_credential_watch();
        // Held for the life of the task once claimed; `None` while another
        // local process owns the id (or when there is nothing to lock).
        let mut claim: Option<IdentityLock> = None;
        let mut lock_unavailable = false;
        let mut backoff = BACKOFF_MIN;
        // Set after an auth rejection (4401 close / 401 handshake / no ack):
        // the next read forces a token refresh before reconnecting. Normal
        // backoff still applies, so a persistently-dead refresh token can't
        // hammer the relay.
        let mut force_refresh = false;
        loop {
            if claim.is_none() && !lock_unavailable {
                match claim_identity(lock_dir.as_deref(), &device) {
                    IdentityClaim::Held(lock) => claim = Some(lock),
                    IdentityClaim::Busy => {
                        status.set(
                            RelayPhase::IdentityBusy,
                            format!("Another daemon on this Mac is already on the relay as {device}; this one stays off until it lets go."),
                        );
                        tracing::error!(
                            %device,
                            "relay: another daemon on this machine is ALREADY online as this device id — not connecting with it \
                             (phones would flap between the two). If that is Big Smooth and this is SmoothFlow's child, give this one \
                             its own SMOOTH_RELAY_DEVICE_ID (the app does: ~/.smooth/smoothflow-relay-device-id, th-a1bb12); \
                             otherwise stop the other daemon. Re-checking in {IDENTITY_RECHECK:?}"
                        );
                        tokio::time::sleep(IDENTITY_RECHECK).await;
                        continue;
                    }
                    IdentityClaim::Unavailable(why) => {
                        tracing::warn!(%device, %why, "relay: cannot lock this device id locally — connecting without the duplicate-identity guard");
                        lock_unavailable = true;
                    }
                }
            }
            // Mark the current view seen BEFORE reading the store, so any change
            // from here on (a logout mid-dial included) wakes the connection.
            creds_rx.borrow_and_update();
            let (token, user) = match fresh_access_token(&http, force_refresh).await {
                TokenOutcome::Dial { token, user } => (token, user),
                TokenOutcome::NoSession(view) => {
                    force_refresh = false;
                    let phase = waiting_phase(&view);
                    let detail = waiting_detail(phase);
                    if status.set(phase, detail) {
                        tracing::info!(state = phase.as_str(), "relay: {detail}");
                    }
                    // Wake on the credentials changing; the timeout is only a
                    // safety net under the watcher.
                    let _ = tokio::time::timeout(SIGNED_OUT_RECHECK, creds_changed(&mut creds_rx)).await;
                    backoff = BACKOFF_MIN;
                    continue;
                }
            };
            force_refresh = false;
            let dialled = CredView::dialled(user, &token);
            let url = connect_url(&relay_url, &token, &device, &label, kind);
            status.set(RelayPhase::Connecting, format!("Dialling {relay_url}."));
            let connected_at = std::time::Instant::now();
            let end = match tokio_tungstenite::connect_async(&url).await {
                Ok((stream, _)) => {
                    // The upgrade is NOT authentication: the relay verifies the
                    // token, then registers the peer and acks `connected`.
                    tracing::info!(relay = %relay_url, kind = kind.as_str(), "relay: socket open — waiting for the relay to authenticate this daemon");
                    status.set(
                        RelayPhase::Authenticating,
                        "Socket open; waiting for the relay to authenticate this daemon. Phones cannot see it yet.",
                    );
                    let ctx = ConnCtx {
                        local_ws_url: &local_ws_url,
                        flow_ws_url: &flow_ws_url,
                        pairing: &pairing,
                        dialled: &dialled,
                        status: &status,
                        device: &device,
                        kind,
                        ack_timeout: AUTH_ACK_TIMEOUT,
                        silence_timeout: RELAY_SILENCE_TIMEOUT,
                    };
                    run_connection(stream, &ctx, &mut creds_rx).await
                }
                Err(e) if is_auth_handshake_error(&e) => {
                    tracing::warn!(error = %e, relay = %relay_url, "relay: handshake rejected (401) — refreshing the Smoo session and reconnecting");
                    ConnEnd::AuthRejected
                }
                Err(e) => {
                    tracing::warn!(error = %e, relay = %relay_url, "relay: connect failed");
                    status.set(RelayPhase::Offline, format!("Cannot reach {relay_url}; retrying."));
                    ConnEnd::Normal
                }
            };
            if matches!(end, ConnEnd::CredsChanged | ConnEnd::SignedOut) {
                // Straight back to the top: re-dial with the new session, or
                // report signed-out and wait. No backoff — this isn't a failure.
                backoff = BACKOFF_MIN;
                continue;
            }
            force_refresh = report_failed_end(&status, &end);
            // A connection that lived a while earns a fresh backoff.
            if connected_at.elapsed() > BACKOFF_RESET_AFTER {
                backoff = BACKOFF_MIN;
            }
            // Back off — but a credentials change (a fresh login after a
            // rejection, say) cuts the wait short.
            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                () = creds_changed(&mut creds_rx) => {
                    backoff = BACKOFF_MIN;
                    continue;
                }
            }
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

/// How a relay connection ended. Each variant is a different answer from the
/// supervisor: back off, refresh-and-retry, or go straight back to the top.
#[derive(Debug, PartialEq, Eq)]
enum ConnEnd {
    /// Dropped (or never connected) — back off and retry.
    Normal,
    /// The relay rejected our token (4401 close / 401 handshake) — refresh.
    AuthRejected,
    /// The socket opened but the relay never acked auth — connected, NOT a
    /// peer (th-37c286). Refresh and retry.
    NoAck,
    /// The Smoo session changed under the socket — re-dial with it now.
    CredsChanged,
    /// Signed out (or expired beyond renewal) — leave the relay and wait.
    SignedOut,
    /// No inbound frame (not even the relay's heartbeat ping) for
    /// [`RELAY_SILENCE_TIMEOUT`] — a silently dead socket (th-6c500f). Re-dial.
    Silent,
}

/// What a live connection needs from the supervisor.
struct ConnCtx<'a> {
    local_ws_url: &'a str,
    flow_ws_url: &'a str,
    pairing: &'a Arc<PairingState>,
    /// The session this socket was dialled with.
    dialled: &'a CredView,
    status: &'a RelayStatusHandle,
    device: &'a str,
    kind: RelayKind,
    /// How long to wait for the relay's auth ack ([`AUTH_ACK_TIMEOUT`]).
    ack_timeout: Duration,
    /// How long the socket may be silent before it counts as dead
    /// ([`RELAY_SILENCE_TIMEOUT`]).
    silence_timeout: Duration,
}

/// One live relay connection: pump relay ⇄ bridges until the socket ends, the
/// relay fails to authenticate us in time, or the credentials change enough to
/// matter ([`on_cred_change`]).
#[allow(
    clippy::too_many_lines,
    reason = "one select loop over the socket, the auth-ack deadline and the credentials watch"
)]
async fn run_connection(
    stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    ctx: &ConnCtx<'_>,
    creds: &mut watch::Receiver<CredView>,
) -> ConnEnd {
    let ConnCtx {
        local_ws_url,
        flow_ws_url,
        pairing,
        dialled,
        status,
        device,
        kind,
        ack_timeout,
        silence_timeout,
    } = *ctx;
    let (mut sink, mut source) = stream.split();
    // All bridges push outbound envelopes through one channel — the single
    // writer to the relay socket.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let mut bridges: HashMap<String, Bridge> = HashMap::new();
    let mut end = ConnEnd::Normal;
    let mut link = Link::Authenticating;
    let ack_deadline = tokio::time::sleep(ack_timeout);
    tokio::pin!(ack_deadline);
    // Reset on every inbound frame; firing means the heartbeat stopped.
    let silence = tokio::time::sleep(silence_timeout);
    tokio::pin!(silence);

    loop {
        tokio::select! {
            () = &mut ack_deadline, if link == Link::Authenticating => {
                tracing::error!(
                    %device,
                    "relay: CONNECTED BUT UNAUTHENTICATED — the relay accepted the socket but has not acknowledged auth after \
                     {ack_timeout:?}, so this daemon is NOT registered as a peer and phones will see it as offline. \
                     Refreshing the Smoo session and re-dialling."
                );
                end = ConnEnd::NoAck;
                break;
            }
            () = &mut silence => {
                tracing::warn!(
                    %device,
                    "relay: no frame from the relay in {silence_timeout:?} (it pings every 30s) — the socket is dead without \
                     having said so, and phones see this daemon as offline. Re-dialling."
                );
                end = ConnEnd::Silent;
                break;
            }
            () = creds_changed(creds) => {
                let now = creds.borrow_and_update().clone();
                match on_cred_change(link, dialled, &now) {
                    CredDecision::Keep => {}
                    CredDecision::Reconnect => {
                        tracing::info!("relay: the Smoo session changed — re-authenticating with it");
                        end = ConnEnd::CredsChanged;
                        break;
                    }
                    CredDecision::Disconnect => {
                        tracing::info!("relay: the Smoo session ended (signed out or expired) — leaving the relay");
                        end = ConnEnd::SignedOut;
                        break;
                    }
                }
            }
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
                if matches!(msg, Some(Ok(_))) {
                    silence.as_mut().reset(tokio::time::Instant::now() + silence_timeout);
                }
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
                    RelayMsg::Connected => {
                        if link != Link::Online {
                            link = Link::Online;
                            status.set(RelayPhase::Online, format!("Registered on the relay as {device} — phones can reach this {}.", kind.product()));
                            tracing::info!(%device, kind = kind.as_str(), "relay: authenticated — {} is a relay peer; phones can reach it without tailscale", kind.product());
                        }
                    }
                    RelayMsg::Ping => {
                        if sink.send(Message::Text(r#"{"type":"pong"}"#.into())).await.is_err() {
                            break;
                        }
                    }
                    RelayMsg::Ignore => {}
                    RelayMsg::PeerOffline(device) => {
                        // The phone we last wrote to is gone — reap its bridges so a
                        // reconnecting phone gets fresh sessions.
                        bridges.remove(&device);
                        bridges.remove(&flow_bridge_key(&device));
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
                    RelayMsg::FlowFrame(from, frame) => {
                        // Same shape onto the flow WS, with the phone output caps.
                        let key = flow_bridge_key(&from);
                        let delivered = bridges
                            .get(&key)
                            .is_some_and(|b| b.to_operator.send(frame.clone()).is_ok() && !b.task.is_finished());
                        if !delivered {
                            let (tx, rx) = mpsc::unbounded_channel();
                            let guard = FlowGuard::new(from.clone(), pairing.clone());
                            let task = spawn_bridge_with(from, flow_ws_url.to_string(), rx, out_tx.clone(), Some(guard));
                            let _ = tx.send(frame);
                            bridges.insert(key, Bridge { to_operator: tx, task });
                        }
                    }
                }
            }
        }
    }
    // Dropping the map aborts every bridge task (Bridge::drop).
    bridges.clear();
    if matches!(end, ConnEnd::CredsChanged | ConnEnd::SignedOut | ConnEnd::NoAck | ConnEnd::Silent) {
        // We're the ones leaving — say so, rather than letting the relay time us out.
        let _ = sink.send(Message::Close(None)).await;
    }
    end
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use smooai_client_shared::auth::storage::Credentials;

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
        assert_eq!(
            resolve_label_from(Some(" Brent's Laptop "), Some("smoo-hub"), RelayKind::Daemon),
            "Brent's Laptop"
        );
        // Verbatim for a flow daemon too — the app composes its own label.
        assert_eq!(
            resolve_label_from(Some("smoo-hub · SmoothFlow"), Some("smoo-hub"), RelayKind::Flow),
            "smoo-hub · SmoothFlow"
        );
    }

    #[test]
    fn label_uses_the_short_hostname() {
        assert_eq!(resolve_label_from(None, Some("smoo-hub.local"), RelayKind::Daemon), "smoo-hub");
        assert_eq!(resolve_label_from(None, Some("  mac-studio\n"), RelayKind::Daemon), "mac-studio");
    }

    #[test]
    fn flow_daemon_label_says_so() {
        assert_eq!(resolve_label_from(None, Some("smoo-hub.local"), RelayKind::Flow), "smoo-hub · SmoothFlow");
        assert_eq!(resolve_label_from(None, None, RelayKind::Flow), "big-smooth · SmoothFlow");
    }

    #[test]
    fn label_falls_back_when_there_is_no_hostname() {
        assert_eq!(resolve_label_from(None, None, RelayKind::Daemon), DEFAULT_LABEL);
        assert_eq!(resolve_label_from(None, Some(""), RelayKind::Daemon), DEFAULT_LABEL);
        assert_eq!(resolve_label_from(Some("  "), Some("   "), RelayKind::Daemon), DEFAULT_LABEL);
        // A label that sanitizes down to nothing is not a label.
        assert_eq!(resolve_label_from(Some("\u{7}\u{0}"), None, RelayKind::Daemon), DEFAULT_LABEL);
    }

    #[test]
    fn label_strips_control_chars_and_caps_length() {
        assert_eq!(resolve_label_from(Some("big\u{0}sm\noth"), None, RelayKind::Daemon), "bigsmoth");
        let long = resolve_label_from(Some(&"x".repeat(500)), None, RelayKind::Daemon);
        assert_eq!(long.chars().count(), LABEL_MAX_CHARS);
        // Multi-byte labels are cut on char boundaries, not bytes.
        let emoji = resolve_label_from(Some(&"é".repeat(500)), None, RelayKind::Daemon);
        assert_eq!(emoji.chars().count(), LABEL_MAX_CHARS);
    }

    // ── connect URL ───────────────────────────────────────────────────────────

    #[test]
    fn connect_url_carries_device_label_and_kind() {
        let u = connect_url("wss://relay.smoo.ai/ws", "tok en", "daemon-abc123", "Brent's Laptop", RelayKind::Daemon);
        assert_eq!(
            u,
            "wss://relay.smoo.ai/ws?token=tok%20en&device=daemon-abc123&label=Brent%27s%20Laptop&kind=daemon"
        );
        let u = connect_url("wss://relay.smoo.ai/ws", "t", "daemon-flow01", "smoo-hub · SmoothFlow", RelayKind::Flow);
        assert_eq!(
            u,
            "wss://relay.smoo.ai/ws?token=t&device=daemon-flow01&label=smoo-hub%20%C2%B7%20SmoothFlow&kind=flow"
        );
    }

    // ── kind (th-a1bb12) ──────────────────────────────────────────────────────

    #[test]
    fn kind_defaults_to_daemon_and_accepts_flow() {
        assert_eq!(resolve_kind_from(None), RelayKind::Daemon);
        assert_eq!(resolve_kind_from(Some("")), RelayKind::Daemon);
        assert_eq!(resolve_kind_from(Some("daemon")), RelayKind::Daemon);
        assert_eq!(resolve_kind_from(Some(" flow ")), RelayKind::Flow);
        assert_eq!(resolve_kind_from(Some("FLOW")), RelayKind::Flow);
    }

    #[test]
    fn junk_kind_stays_a_daemon_rather_than_becoming_a_phone() {
        // The relay coerces unknown kinds to `phone`, which would hide the
        // daemon from every picker — never send what we did not recognise.
        for junk in ["phone", "root", "flow\u{0}", "daemon; drop"] {
            assert_eq!(resolve_kind_from(Some(junk)), RelayKind::Daemon, "junk kind: {junk:?}");
        }
    }

    // ── identity lock (th-a1bb12) ─────────────────────────────────────────────

    #[test]
    fn identity_is_claimed_once_and_busy_for_a_second_holder() {
        let dir = tempfile::tempdir().unwrap();
        let first = claim_identity(Some(dir.path()), "daemon-abc123");
        assert!(matches!(first, IdentityClaim::Held(_)), "{first:?}");
        assert!(dir.path().join(LOCK_DIR).join("daemon-abc123.lock").is_file());
        // A second open of the same path is a distinct file description, so
        // the OS reports the conflict even within one process.
        assert!(matches!(claim_identity(Some(dir.path()), "daemon-abc123"), IdentityClaim::Busy));
        // A different id on the same machine is a different lock.
        assert!(matches!(claim_identity(Some(dir.path()), "daemon-flow01"), IdentityClaim::Held(_)));
        drop(first);
        // Released on drop. Polled, not asserted once: a sibling test that is
        // mid-spawn shares our open file descriptions until its child execs
        // (O_CLOEXEC), and flock follows the description — seen once under a
        // load average of 23.
        let reclaimed = (0..50).any(|_| {
            let held = matches!(claim_identity(Some(dir.path()), "daemon-abc123"), IdentityClaim::Held(_));
            if !held {
                std::thread::sleep(Duration::from_millis(20));
            }
            held
        });
        assert!(reclaimed, "released on drop");
    }

    #[test]
    fn identity_lock_tolerates_junk_ids_and_a_missing_home() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(claim_identity(Some(dir.path()), "../../etc/passwd"), IdentityClaim::Held(_)));
        assert_eq!(lock_file_name("../../etc/passwd"), ".._.._etc_passwd.lock");
        assert!(
            dir.path().join(LOCK_DIR).join(".._.._etc_passwd.lock").is_file(),
            "the lock stays inside relay-locks/"
        );
        assert!(matches!(claim_identity(None, "daemon-abc123"), IdentityClaim::Unavailable(_)));
    }

    // ── inbound classification ────────────────────────────────────────────────

    #[test]
    fn classify_ping_and_control_noise() {
        assert_eq!(classify_relay_msg(r#"{"type":"ping"}"#), RelayMsg::Ping);
        // The auth ack is NOT noise: it is the only proof we're a peer (th-37c286).
        assert_eq!(classify_relay_msg(r#"{"type":"connected"}"#), RelayMsg::Connected);
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
    fn classify_routes_flow_channel_frames_separately() {
        let m = classify_relay_msg(r#"{"from":"phone-1","frame":{"channel":"flow","type":"flow.attach","id":"fs-1","cols":80,"rows":24}}"#);
        match m {
            RelayMsg::FlowFrame(from, frame) => {
                assert_eq!(from, "phone-1");
                assert!(frame.contains("flow.attach"));
            }
            other => panic!("{other:?}"),
        }
        // No channel ⇒ operator, unchanged.
        assert!(matches!(
            classify_relay_msg(r#"{"from":"phone-1","frame":{"action":"send_message","message":"hi"}}"#),
            RelayMsg::Frame(..)
        ));
        // A non-flow channel is still the operator's business.
        assert!(matches!(
            classify_relay_msg(r#"{"from":"phone-1","frame":{"channel":"other","type":"x"}}"#),
            RelayMsg::Frame(..)
        ));
        assert_ne!(flow_bridge_key("phone-1"), "phone-1");
        assert!(flow_ws_url(4400, "t k").ends_with("/api/flow/ws?token=t%20k"));
    }

    #[test]
    fn output_coalescer_caps_frame_size_and_merges_chunks() {
        let mut c = OutputCoalescer::default();
        assert!(
            !c.absorb(r#"{"channel":"flow","type":"flow.session","session":{}}"#),
            "non-output passes through"
        );
        assert!(c.is_empty());
        let big = vec![b'x'; PHONE_OUTPUT_MAX_BYTES + 10];
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &big);
        assert!(c.absorb(&json!({"channel":"flow","type":"flow.output","id":"fs-1","seq":1,"data_b64":b64}).to_string()));
        assert!(c.absorb(&json!({"channel":"flow","type":"flow.output","id":"fs-1","seq":2,"data_b64":"YWI="}).to_string()));
        assert!(c.absorb(&json!({"channel":"flow","type":"flow.output","id":"fs-2","seq":1,"data_b64":"eg=="}).to_string()));
        assert!(!c.is_empty());
        let frames = c.drain();
        assert!(c.is_empty());
        assert_eq!(frames.len(), 3, "fs-1 = 16 KiB + 12 bytes, fs-2 = 1 byte: {}", frames.len());
        let decoded: Vec<(String, Vec<u8>)> = frames
            .iter()
            .map(|f| {
                let v: Value = serde_json::from_str(f).unwrap();
                assert_eq!(v["type"], "flow.output");
                assert_eq!(v["channel"], "flow");
                (
                    v["id"].as_str().unwrap().to_string(),
                    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, v["data_b64"].as_str().unwrap()).unwrap(),
                )
            })
            .collect();
        assert_eq!(decoded[0].1.len(), PHONE_OUTPUT_MAX_BYTES);
        assert_eq!(decoded[1].1.len(), 12, "the 10-byte tail merged with the 2-byte follow-up");
        assert_eq!(decoded[2], ("fs-2".to_string(), b"z".to_vec()));
        let seqs: Vec<u64> = frames
            .iter()
            .map(|f| serde_json::from_str::<Value>(f).unwrap()["seq"].as_u64().unwrap())
            .collect();
        assert!(seqs.windows(2).all(|w| w[1] > w[0]));
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

    // The refresh itself now runs under the shared credential lock in
    // `auth_login::refresh_user_session_locked` (th-c6a542) — its behaviour
    // (lock, re-read, only_if_due short-circuit, no-refresh-token bail) is tested
    // there. Here we only own the decision gate and the persistence invariant.
    #[test]
    fn creds_expiring_builds_a_user_session_the_decision_gate_reads() {
        let c = creds_expiring(Some(Utc::now() - ChronoDuration::hours(1)), None);
        assert!(needs_refresh(c.expires_at, Utc::now()), "an hour past expiry is due for refresh");
        assert!(c.refresh_token.is_none());
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

    // ── credential-driven relay link (th-37c286) ─────────────────────────────

    #[test]
    fn token_outcome_dials_only_a_usable_session() {
        let now = Utc::now();
        assert!(matches!(token_outcome(None, now), TokenOutcome::NoSession(CredView::SignedOut)));
        let expired = creds_expiring(Some(now - ChronoDuration::minutes(1)), Some("r"));
        assert!(
            matches!(token_outcome(Some(expired), now), TokenOutcome::NoSession(CredView::Expired { .. })),
            "an expired token (refresh failed) must not be dialled — the relay would only reject it"
        );
        let live = creds_expiring(Some(now + ChronoDuration::hours(1)), Some("r"));
        match token_outcome(Some(live), now) {
            TokenOutcome::Dial { token, user } => {
                assert_eq!(token, "acc");
                assert_eq!(user.as_deref(), Some("brent@smoo.ai"));
            }
            other @ TokenOutcome::NoSession(_) => panic!("expected a dial, got {other:?}"),
        }
    }

    /// A fake relay: accepts the upgrade, optionally acks `connected`, and
    /// reports whether the daemon closed the socket from its side.
    async fn fake_relay(ack: bool) -> (std::net::SocketAddr, tokio::sync::oneshot::Receiver<bool>) {
        use axum::extract::ws::{Message as AxMsg, WebSocketUpgrade};
        use axum::routing::get;
        use axum::Router;

        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let closed_tx = Arc::new(std::sync::Mutex::new(Some(closed_tx)));
        let app = Router::new().route(
            "/ws",
            get(move |u: WebSocketUpgrade| {
                let closed_tx = closed_tx.clone();
                async move {
                    u.on_upgrade(move |mut ws| async move {
                        if ack {
                            let _ = ws.send(AxMsg::Text(r#"{"type":"connected"}"#.into())).await;
                        }
                        let mut client_closed = false;
                        while let Some(msg) = ws.recv().await {
                            if matches!(msg, Ok(AxMsg::Close(_))) {
                                client_closed = true;
                                break;
                            }
                        }
                        if let Some(tx) = closed_tx.lock().unwrap().take() {
                            let _ = tx.send(client_closed);
                        }
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (addr, closed_rx)
    }

    struct Harness {
        status: RelayStatusHandle,
        creds_tx: watch::Sender<CredView>,
        creds_rx: watch::Receiver<CredView>,
        dialled: CredView,
        pairing: Arc<PairingState>,
    }

    fn harness() -> Harness {
        let dialled = CredView::dialled(Some("a@x".into()), "t1");
        let (creds_tx, creds_rx) = watch::channel(dialled.clone());
        Harness {
            status: RelayStatusHandle::new(RelayPhase::Authenticating, ""),
            creds_tx,
            creds_rx,
            dialled,
            pairing: Arc::new(crate::flow_e2e::tests::state()),
        }
    }

    async fn run_against(addr: std::net::SocketAddr, h: &mut Harness, ack_timeout: Duration) -> ConnEnd {
        run_against_with(addr, h, ack_timeout, Duration::from_secs(30)).await
    }

    async fn run_against_with(addr: std::net::SocketAddr, h: &mut Harness, ack_timeout: Duration, silence_timeout: Duration) -> ConnEnd {
        let (stream, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await.unwrap();
        let ctx = ConnCtx {
            local_ws_url: "ws://127.0.0.1:1/ws",
            flow_ws_url: "ws://127.0.0.1:1/api/flow/ws",
            pairing: &h.pairing,
            dialled: &h.dialled,
            status: &h.status,
            device: "daemon-test",
            kind: RelayKind::Flow,
            ack_timeout,
            silence_timeout,
        };
        tokio::time::timeout(Duration::from_secs(10), run_connection(stream, &ctx, &mut h.creds_rx))
            .await
            .expect("the connection must end on its own")
    }

    /// A fake relay that acks, sends `pings` heartbeats `every` apart, then goes
    /// silent while keeping the socket open — the half-open socket th-6c500f
    /// left a daemon stuck on for two days.
    async fn relay_that_goes_quiet(pings: u32, every: Duration) -> (std::net::SocketAddr, tokio::sync::oneshot::Receiver<bool>) {
        use axum::extract::ws::{Message as AxMsg, WebSocketUpgrade};
        use axum::routing::get;
        use axum::Router;

        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let closed_tx = Arc::new(std::sync::Mutex::new(Some(closed_tx)));
        let app = Router::new().route(
            "/ws",
            get(move |u: WebSocketUpgrade| {
                let closed_tx = closed_tx.clone();
                async move {
                    u.on_upgrade(move |ws| async move {
                        let (mut tx, mut rx) = ws.split();
                        let _ = tx.send(AxMsg::Text(r#"{"type":"connected"}"#.into())).await;
                        tokio::spawn(async move {
                            for _ in 0..pings {
                                tokio::time::sleep(every).await;
                                if tx.send(AxMsg::Text(r#"{"type":"ping"}"#.into())).await.is_err() {
                                    return;
                                }
                            }
                            // Silent from here on, but hold the sender so the socket stays open.
                            std::future::pending::<()>().await;
                            drop(tx);
                        });
                        let mut client_closed = false;
                        while let Some(msg) = rx.next().await {
                            if matches!(msg, Ok(AxMsg::Close(_))) {
                                client_closed = true;
                                break;
                            }
                        }
                        if let Some(tx) = closed_tx.lock().unwrap().take() {
                            let _ = tx.send(client_closed);
                        }
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (addr, closed_rx)
    }

    #[tokio::test]
    async fn heartbeats_keep_the_link_up_and_silence_takes_it_down() {
        // Pings every 100ms for ~1s against a 400ms silence window: the link
        // must outlive the window many times over, then drop once pings stop.
        let (addr, closed) = relay_that_goes_quiet(10, Duration::from_millis(100)).await;
        let mut h = harness();
        let started = std::time::Instant::now();
        let end = run_against_with(addr, &mut h, Duration::from_secs(5), Duration::from_millis(400)).await;
        assert_eq!(end, ConnEnd::Silent);
        assert!(
            started.elapsed() >= Duration::from_millis(900),
            "heartbeats must reset the silence timer (ended after {:?})",
            started.elapsed()
        );
        assert!(!report_failed_end(&h.status, &end), "silence is a dead socket, not a bad token");
        assert_eq!(h.status.get().state, RelayPhase::Offline, "a silent link must never keep reading as online");
        assert!(h.status.get().detail.contains("silent"));
        assert!(closed.await.unwrap(), "the daemon closes the socket it gave up on");
    }

    #[tokio::test]
    async fn a_socket_the_relay_never_acks_is_unauthenticated_not_online() {
        let (addr, closed) = fake_relay(false).await;
        let mut h = harness();
        let end = run_against(addr, &mut h, Duration::from_millis(300)).await;
        assert_eq!(end, ConnEnd::NoAck);
        assert_ne!(h.status.get().state, RelayPhase::Online, "no ack must never read as online");
        // The supervisor's report for this end is the observable state.
        assert!(report_failed_end(&h.status, &end), "no ack forces a token refresh");
        assert_eq!(h.status.get().state, RelayPhase::Unauthenticated);
        assert!(h.status.get().detail.contains("NOT a peer"));
        assert!(closed.await.unwrap(), "the daemon closes the socket it gave up on");
    }

    #[tokio::test]
    async fn the_ack_puts_the_link_online_and_a_logout_takes_it_off() {
        let (addr, closed) = fake_relay(true).await;
        let mut h = harness();
        let status = h.status.clone();
        let mut watch_status = status.subscribe();
        let creds_tx = h.creds_tx.clone();
        tokio::spawn(async move {
            while watch_status.borrow_and_update().state != RelayPhase::Online {
                watch_status.changed().await.unwrap();
            }
            // Online — a token rotation must NOT drop it...
            creds_tx.send(CredView::dialled(Some("a@x".into()), "t2")).unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            // ...a logout must.
            creds_tx.send(CredView::SignedOut).unwrap();
        });
        let end = run_against(addr, &mut h, Duration::from_secs(5)).await;
        assert_eq!(end, ConnEnd::SignedOut);
        assert!(closed.await.unwrap(), "leaving on logout closes the socket");
    }

    #[tokio::test]
    async fn a_rotation_before_the_ack_re_authenticates() {
        let (addr, _closed) = fake_relay(false).await;
        let mut h = harness();
        let creds_tx = h.creds_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            creds_tx.send(CredView::dialled(Some("a@x".into()), "t2")).unwrap();
        });
        let end = run_against(addr, &mut h, Duration::from_secs(5)).await;
        assert_eq!(end, ConnEnd::CredsChanged);
    }

    #[tokio::test]
    async fn a_new_user_re_authenticates_even_when_online() {
        let (addr, _closed) = fake_relay(true).await;
        let mut h = harness();
        let mut watch_status = h.status.subscribe();
        let creds_tx = h.creds_tx.clone();
        tokio::spawn(async move {
            while watch_status.borrow_and_update().state != RelayPhase::Online {
                watch_status.changed().await.unwrap();
            }
            creds_tx.send(CredView::dialled(Some("b@x".into()), "t9")).unwrap();
        });
        let end = run_against(addr, &mut h, Duration::from_secs(5)).await;
        assert_eq!(end, ConnEnd::CredsChanged);
    }

    #[test]
    fn a_plain_drop_reports_offline_without_forcing_a_refresh() {
        let status = RelayStatusHandle::new(RelayPhase::Online, "");
        assert!(!report_failed_end(&status, &ConnEnd::Normal));
        assert_eq!(status.get().state, RelayPhase::Offline);
        assert!(report_failed_end(&status, &ConnEnd::AuthRejected));
        assert_eq!(status.get().state, RelayPhase::AuthRejected);
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

    /// th-d33afa: the phone's FIRST `channel:flow` frame (its `flow.hello`
    /// nudge) is what opens the flow bridge; the engine's on-connect hello
    /// comes back first, then the reply to the nudge — both enveloped to the
    /// phone with the flow channel intact.
    #[tokio::test]
    async fn first_flow_frame_opens_the_flow_bridge_and_hello_comes_back() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = smooth_flow::Engine::open(smooth_flow::EngineConfig {
            db_path: tmp.path().join("flow.db"),
            default_project: tmp.path().to_path_buf(),
            version: "t".into(),
            machine_label: "m".into(),
            home: tmp.path().join("home"),
            daemon_url: None,
        })
        .unwrap();
        let app = crate::flow_route::flow_router(engine, Some("tok".into()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // Exactly what run_connection does on RelayMsg::FlowFrame.
        let nudge = r#"{"channel":"flow","type":"flow.hello"}"#;
        let RelayMsg::FlowFrame(from, frame) = classify_relay_msg(&json!({"from":"phone-9","frame":serde_json::from_str::<Value>(nudge).unwrap()}).to_string())
        else {
            panic!("a flow.hello envelope is a flow frame")
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let guard = FlowGuard::new(from.clone(), Arc::new(crate::flow_e2e::tests::state()));
        let _task = spawn_bridge_with(from, flow_ws_url(addr.port(), "tok"), rx, out_tx, Some(guard));
        tx.send(frame).unwrap();

        let mut hellos = 0;
        while hellos < 2 {
            let envelope = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
                .await
                .expect("hello within 5s")
                .unwrap();
            let v: Value = serde_json::from_str(&envelope).unwrap();
            assert_eq!(v["to"], "phone-9");
            assert_eq!(v["frame"]["channel"], "flow");
            assert_eq!(v["frame"]["type"], "flow.hello", "{v}");
            assert_eq!(v["frame"]["daemon"]["version"], "t");
            hellos += 1;
        }
    }

    // ── end-to-end guard (th-d98fde) ────────────────────────────────────────

    use crate::flow_e2e::tests::{phone_pair_frame, state as pairing_state, CODE, DAEMON_SALT, DAEMON_SECRET, PAIRING_ID, PHONE_DEVICE, PHONE_SALT};
    use crate::flow_e2e::{b64, derive_session_key, open, seal, unb64, DIR_DAEMON_TO_PHONE, DIR_PHONE_TO_DAEMON};

    fn error_code(action: &Action) -> String {
        let Action::ToPhone(text) = action else {
            panic!("expected a reply, got {action:?}")
        };
        let v: Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["type"], "flow.error", "{v}");
        v["code"].as_str().unwrap().to_string()
    }

    /// Pair the fixture phone through the guard and open a session; returns
    /// (guard, phone-side session key).
    fn paired_guard() -> (FlowGuard, Arc<PairingState>, [u8; 32]) {
        let st = Arc::new(pairing_state());
        let qr = st.begin_with(DAEMON_SECRET, CODE, PAIRING_ID.into());
        let mut guard = FlowGuard::new(PHONE_DEVICE, st.clone());
        let (frame, pairing_key) = phone_pair_frame(&qr, r#"{"type":"flow.pair.hello","label":"x","platform":"ios"}"#);
        let actions = guard.inbound(&frame);
        assert_eq!(actions.len(), 1);
        let Action::ToPhone(reply) = &actions[0] else { panic!("{actions:?}") };
        let v: Value = serde_json::from_str(reply).unwrap();
        assert_eq!(v["type"], "flow.pair");
        let pt = open(&pairing_key, DIR_DAEMON_TO_PHONE, 1, &unb64(v["ct"].as_str().unwrap()).unwrap()).unwrap();
        assert!(String::from_utf8(pt).unwrap().contains("flow.pair.ok"));
        (guard, st, pairing_key)
    }

    #[test]
    fn unpaired_phone_passes_plaintext_both_ways() {
        let st = Arc::new(pairing_state());
        let mut guard = FlowGuard::new("phone-unpaired", st);
        assert_eq!(
            guard.inbound(r#"{"channel":"flow","type":"flow.hello"}"#),
            vec![Action::ToEngine(r#"{"channel":"flow","type":"flow.hello"}"#.into())]
        );
        assert_eq!(
            guard.outbound(r#"{"channel":"flow","type":"flow.hello"}"#),
            vec![r#"{"channel":"flow","type":"flow.hello"}"#.to_string()]
        );
        // An open from an unpaired phone is refused, visibly.
        assert_eq!(
            error_code(&guard.inbound(&json!({"channel":"flow","v":1,"type":"flow.e2e.open","salt":b64(&PHONE_SALT)}).to_string())[0]),
            "e2e_not_paired"
        );
    }

    #[test]
    fn required_mode_refuses_plaintext_from_unpaired_phones() {
        let st = Arc::new(PairingState::new(crate::flow_e2e::tests::engine(), "daemon-x".into(), "x".into(), true));
        let mut guard = FlowGuard::new("phone-unpaired", st);
        assert_eq!(error_code(&guard.inbound(r#"{"channel":"flow","type":"flow.hello"}"#)[0]), "e2e_required");
        assert!(
            guard.outbound(r#"{"channel":"flow","type":"flow.hello"}"#).is_empty(),
            "nothing leaves in the clear"
        );
    }

    #[test]
    fn paired_phone_pairs_opens_and_exchanges_sealed_frames() {
        let (mut guard, _st, pairing_key) = paired_guard();
        // Engine output before the session opens is buffered, not sent in the clear.
        assert!(guard.outbound(r#"{"channel":"flow","type":"flow.hello","sessions":[]}"#).is_empty());
        // Plaintext from a paired phone is refused.
        assert_eq!(error_code(&guard.inbound(r#"{"channel":"flow","type":"flow.hello"}"#)[0]), "e2e_required");
        // Data before open is refused.
        assert_eq!(error_code(&guard.inbound(r#"{"channel":"flow","v":1,"n":1,"ct":"AAAA"}"#)[0]), "e2e_not_open");
        // Open → reply + the buffered hello, both sealed.
        let actions = guard.inbound(&json!({"channel":"flow","v":1,"type":"flow.e2e.open","salt":b64(&PHONE_SALT)}).to_string());
        assert_eq!(actions.len(), 2, "{actions:?}");
        let Action::ToPhone(open_reply) = &actions[0] else { panic!() };
        let v: Value = serde_json::from_str(open_reply).unwrap();
        assert_eq!(v["type"], "flow.e2e.open");
        let daemon_salt: [u8; 16] = unb64(v["salt"].as_str().unwrap()).unwrap().try_into().unwrap();
        let session_key = derive_session_key(&pairing_key, &PHONE_SALT, &daemon_salt);
        let Action::ToPhone(sealed_hello) = &actions[1] else { panic!() };
        let v: Value = serde_json::from_str(sealed_hello).unwrap();
        assert_eq!(v["n"], 1);
        let pt = open(&session_key, DIR_DAEMON_TO_PHONE, 1, &unb64(v["ct"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(pt, br#"{"channel":"flow","type":"flow.hello","sessions":[]}"#);
        assert!(guard.session_open());
        // Phone → engine, decrypted.
        let ct = seal(&session_key, DIR_PHONE_TO_DAEMON, 1, br#"{"channel":"flow","type":"flow.attach","id":"fs-1"}"#).unwrap();
        assert_eq!(
            guard.inbound(&json!({"channel":"flow","v":1,"n":1,"ct":b64(&ct)}).to_string()),
            vec![Action::ToEngine(r#"{"channel":"flow","type":"flow.attach","id":"fs-1"}"#.into())]
        );
        // Replay is refused.
        assert_eq!(
            error_code(&guard.inbound(&json!({"channel":"flow","v":1,"n":1,"ct":b64(&ct)}).to_string())[0]),
            "e2e_bad_frame"
        );
        // Engine → phone, sealed with n=2 now.
        let wire = guard.outbound(r#"{"channel":"flow","type":"flow.output","id":"fs-1","seq":1,"data_b64":"aGk="}"#);
        let v: Value = serde_json::from_str(&wire[0]).unwrap();
        assert_eq!(v["n"], 2);
        assert!(v.get("data_b64").is_none(), "nothing readable in the clear: {v}");
    }

    #[test]
    fn revoke_mid_session_refuses_the_next_frame_and_drops_output() {
        let (mut guard, st, pairing_key) = paired_guard();
        let actions = guard.inbound(&json!({"channel":"flow","v":1,"type":"flow.e2e.open","salt":b64(&PHONE_SALT)}).to_string());
        let Action::ToPhone(open_reply) = &actions[0] else { panic!() };
        let v: Value = serde_json::from_str(open_reply).unwrap();
        let daemon_salt: [u8; 16] = unb64(v["salt"].as_str().unwrap()).unwrap().try_into().unwrap();
        let session_key = derive_session_key(&pairing_key, &PHONE_SALT, &daemon_salt);
        assert!(st.revoke(PHONE_DEVICE).unwrap());
        let ct = seal(&session_key, DIR_PHONE_TO_DAEMON, 1, b"{}").unwrap();
        assert_eq!(
            error_code(&guard.inbound(&json!({"channel":"flow","v":1,"n":1,"ct":b64(&ct)}).to_string())[0]),
            "e2e_revoked"
        );
        assert!(!guard.session_open());
        // Now unpaired: engine output goes out in the clear again (pre-pairing behaviour),
        // and the phone can pair afresh.
        assert_eq!(guard.outbound("{}"), vec!["{}".to_string()]);
    }

    #[test]
    fn re_pairing_rotates_the_key_and_drops_the_old_session() {
        let (mut guard, st, _old_key) = paired_guard();
        drop(guard.inbound(&json!({"channel":"flow","v":1,"type":"flow.e2e.open","salt":b64(&PHONE_SALT)}).to_string()));
        assert!(guard.session_open());
        let qr = st.begin_with(DAEMON_SECRET, [9u8; 16], "deadbeef".into());
        let (frame, new_key) = phone_pair_frame(&qr, r#"{"type":"flow.pair.hello","label":"same phone","platform":"ios"}"#);
        let actions = guard.inbound(&frame);
        assert!(matches!(actions[0], Action::ToPhone(_)));
        assert!(!guard.session_open(), "old session is gone after a re-pair");
        assert_eq!(st.key_for(PHONE_DEVICE).unwrap(), new_key);
        assert_eq!(st.list().len(), 1);
        assert_eq!(st.list()[0].label, "same phone");
        // Old-key frames no longer open.
        let (mut fresh, _) = st.open_session_with(PHONE_DEVICE, &b64(&PHONE_SALT), DAEMON_SALT).unwrap();
        let old_session_key = derive_session_key(&_old_key, &PHONE_SALT, &DAEMON_SALT);
        let ct = seal(&old_session_key, DIR_PHONE_TO_DAEMON, 1, b"{}").unwrap();
        assert!(fresh.open_frame(1, &b64(&ct)).is_err());
    }

    #[test]
    fn pending_output_is_capped() {
        let (mut guard, _st, _k) = paired_guard();
        for i in 0..(PENDING_OUT_MAX + 10) {
            assert!(guard
                .outbound(&format!(r#"{{"channel":"flow","type":"flow.event","id":"fs-1","kind":"x","text":"{i}"}}"#))
                .is_empty());
        }
        let actions = guard.inbound(&json!({"channel":"flow","v":1,"type":"flow.e2e.open","salt":b64(&PHONE_SALT)}).to_string());
        assert_eq!(actions.len(), 1 + PENDING_OUT_MAX, "open reply + the capped buffer");
    }

    #[test]
    fn malformed_e2e_frames_get_a_visible_error() {
        let (mut guard, _st, _k) = paired_guard();
        assert_eq!(error_code(&guard.inbound(r#"{"channel":"flow","v":2,"n":1,"ct":"x"}"#)[0]), "e2e_bad_frame");
        assert_eq!(error_code(&guard.inbound(r#"{"channel":"flow","v":1,"type":"flow.pair"}"#)[0]), "e2e_bad_frame");
        assert_eq!(
            error_code(&guard.inbound(r#"{"channel":"flow","v":1,"type":"flow.pair","pair":"nope","pk":"x","n":1,"ct":"y"}"#)[0]),
            "pair_failed"
        );
        assert_eq!(error_code(&guard.inbound("garbage")[0]), "e2e_bad_frame");
    }

    /// The live bridge with a PAIRED phone: the engine's on-connect hello must
    /// come back sealed, never in the clear.
    #[tokio::test]
    async fn paired_phone_gets_the_engine_hello_sealed_over_the_real_flow_ws() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = smooth_flow::Engine::open(smooth_flow::EngineConfig {
            db_path: tmp.path().join("flow.db"),
            default_project: tmp.path().to_path_buf(),
            version: "t".into(),
            machine_label: "m".into(),
            home: tmp.path().join("home"),
            daemon_url: None,
        })
        .unwrap();
        let app = crate::flow_route::flow_router(engine.clone(), Some("tok".into()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let st = Arc::new(PairingState::new(engine, "daemon-t".into(), "t".into(), false));
        let qr = st.begin_with(DAEMON_SECRET, CODE, PAIRING_ID.into());
        let (pair_frame, pairing_key) = phone_pair_frame(&qr, r#"{"type":"flow.pair.hello","label":"x","platform":"ios"}"#);

        let (tx, rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let guard = FlowGuard::new(PHONE_DEVICE, st.clone());
        let _task = spawn_bridge_with(PHONE_DEVICE.to_string(), flow_ws_url(addr.port(), "tok"), rx, out_tx, Some(guard));
        tx.send(pair_frame).unwrap();
        tx.send(json!({"channel":"flow","v":1,"type":"flow.e2e.open","salt":b64(&PHONE_SALT)}).to_string())
            .unwrap();

        let mut session_key = None;
        let mut saw_sealed_hello = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !saw_sealed_hello {
            let envelope = tokio::time::timeout_at(deadline, out_rx.recv()).await.expect("frames within 5s").unwrap();
            let v: Value = serde_json::from_str(&envelope).unwrap();
            assert_eq!(v["to"], PHONE_DEVICE);
            let frame = &v["frame"];
            assert!(
                frame.get("sessions").is_none() && frame.get("daemon").is_none(),
                "hello leaked in the clear: {v}"
            );
            match frame["type"].as_str() {
                Some("flow.pair") => {}
                Some("flow.e2e.open") => {
                    let ds: [u8; 16] = unb64(frame["salt"].as_str().unwrap()).unwrap().try_into().unwrap();
                    session_key = Some(derive_session_key(&pairing_key, &PHONE_SALT, &ds));
                }
                Some(other) => panic!("unexpected clear frame {other}: {v}"),
                None => {
                    let key = session_key.expect("open reply precedes data");
                    let n = frame["n"].as_u64().unwrap();
                    let pt = open(&key, DIR_DAEMON_TO_PHONE, n, &unb64(frame["ct"].as_str().unwrap()).unwrap()).unwrap();
                    let inner: Value = serde_json::from_slice(&pt).unwrap();
                    if inner["type"] == "flow.hello" {
                        assert_eq!(inner["daemon"]["version"], "t");
                        saw_sealed_hello = true;
                    }
                }
            }
        }
    }
}

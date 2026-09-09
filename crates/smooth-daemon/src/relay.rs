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
//! Config: `SMOOTH_RELAY=0` disables; `SMOOTH_RELAY_URL` overrides the default
//! relay endpoint; `SMOOTH_RELAY_DEVICE_ID` / `SMOOTH_RELAY_LABEL` pin the
//! identity (the env-knob precedent of `config.rs`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use smooai_client_shared::auth::storage::CredentialsStore;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::flow_e2e::{self, E2eSession, Inbound, PairingState, PENDING_OUT_MAX};

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

/// This daemon's relay identity — resolved ONCE at boot so the pairing QR and
/// the relay connection agree on the device id across reconnects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayIdentity {
    pub device: String,
    pub label: String,
}

/// Resolve [`RelayIdentity`] from the environment + `~/.smooth`.
pub fn device_identity() -> RelayIdentity {
    let device = resolve_device_id_from(
        std::env::var("SMOOTH_RELAY_DEVICE_ID").ok().as_deref(),
        dirs_next::home_dir().map(|h| h.join(".smooth")).as_deref(),
    );
    let label = resolve_label_from(std::env::var("SMOOTH_RELAY_LABEL").ok().as_deref(), host_name().as_deref());
    RelayIdentity { device, label }
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

/// Read the freshest USABLE Smoo access token from the stored session,
/// refreshing it first when it's expired/near-expiry (or `force`d after a
/// 4401) and persisting the rotated tokens. `None` when signed out / no store —
/// the supervisor waits and retries. Best-effort: a failed refresh still
/// returns the existing token so the connect is attempted, never crashing the
/// daemon (th-c6a542).
async fn fresh_access_token(http: &reqwest::Client, force: bool) -> Option<String> {
    let store = CredentialsStore::default_user().ok()?;
    let creds = store.load().ok().flatten()?;
    if !force && !needs_refresh(creds.expires_at, Utc::now()) {
        return Some(creds.access_token).filter(|t| !t.is_empty());
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
    Some(creds.access_token).filter(|t| !t.is_empty())
}

/// Spawn the relay supervisor.
///
/// (Re)connects to the relay with a fresh token, forwards envelopes phone ⇄
/// operator via per-device loopback bridges, backs off exponentially on drops,
/// and waits patiently while signed out. Never fails the daemon — every error
/// is a log line and a retry.
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
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let http = reqwest::Client::default();
        let local_ws_url = format!("ws://127.0.0.1:{local_port}/ws?token={}", urlencode(&local_token));
        let flow_ws_url = flow_ws_url(local_port, &local_token);
        let RelayIdentity { device, label } = identity;
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
                    match run_connection(stream, &local_ws_url, &flow_ws_url, &pairing).await {
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
async fn run_connection(
    stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    local_ws_url: &str,
    flow_ws_url: &str,
    pairing: &Arc<PairingState>,
) -> ConnEnd {
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

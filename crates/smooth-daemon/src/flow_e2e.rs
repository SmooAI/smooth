//! End-to-end encryption of SmoothFlow relay frames (pearl th-d98fde).
//!
//! A phone reaches this daemon through the Smoo Relay, which forwards opaque
//! `{to, frame}` envelopes between one user's devices. Terminal bytes must not
//! be readable by the relay, so every `channel:"flow"` frame between a
//! **paired** phone and the daemon is sealed here and only the envelope stays
//! routable. Big Smooth chat frames (no `channel`) are untouched.
//!
//! ## Pairing (once per phone)
//!
//! 1. The daemon mints a pending pairing: a fresh X25519 keypair, a random
//!    128-bit one-time code, a short id. The macOS app / `th flow pair --qr`
//!    show it as a QR carrying `smoothflow://pair?v=1&p=<id>&d=<daemon device>
//!    &k=<daemon pub>&c=<code>&l=<label>`. The code never crosses the relay.
//! 2. The phone scans it, makes its own X25519 keypair and derives
//!    `pairing_key = HKDF-SHA256(salt = code, ikm = X25519(phone_sk, daemon_pk),
//!    info = "smoothflow-pair/v1" || pairing_id)`.
//! 3. The phone sends `{channel:"flow", v:1, type:"flow.pair", pair:<id>,
//!    pk:<phone pub>, n:1, ct}` where `ct` seals `{"type":"flow.pair.hello",
//!    label, platform}` under the pairing key. Proving it can seal under a key
//!    that needs the code is what authenticates the phone: the relay sees both
//!    public keys but never the code, so it cannot substitute its own.
//! 4. The daemon derives the same key, opens the hello, persists the pairing
//!    (`flow.db` `pairings`: device → pubkey, label, created, last seen) and
//!    answers `{channel:"flow", v:1, type:"flow.pair", n:1, ct}` sealing
//!    `{"type":"flow.pair.ok", device, label}`.
//!
//! ## Sessions (every connection)
//!
//! The pairing key is never used for data. Each time the phone connects it
//! sends `{channel:"flow", v:1, type:"flow.e2e.open", salt:<16 B>}`; the
//! daemon answers with its own 16-byte salt and both derive
//! `session_key = HKDF-SHA256(salt = phone_salt || daemon_salt, ikm =
//! pairing_key, info = "smoothflow-session/v1")`. The daemon's contribution
//! is what stops a whole recorded session from being replayed after a restart.
//!
//! Data frames are `{channel:"flow", v:1, n:<counter>, ct:<base64>}` — the
//! plaintext is the ordinary flow frame JSON. ChaCha20-Poly1305 with a
//! 12-byte nonce `[direction, 0, 0, 0, n as u64 BE]`; direction 0 is
//! phone→daemon, 1 is daemon→phone; counters start at 1 and each receiver
//! requires a strictly increasing `n` (replay rejection). AAD is the constant
//! `smoothflow-e2e/v1`. Base64: standard for `ct`/`salt`, URL-safe unpadded
//! for keys and the code (they ride in a URL).
//!
//! Once a phone is paired, its **unencrypted** flow frames are rejected with a
//! plaintext `flow.error` (`e2e_required`), so a downgrade is visible, not
//! silent. Unpaired phones keep working in plaintext until
//! `SMOOTH_FLOW_E2E_REQUIRED=1`, which rejects every plaintext flow frame.
//!
//! The cross-platform test vectors live in
//! `tests/fixtures/flow-e2e-v1.json`; the Swift and Kotlin relay packages
//! (smooai `apps/bigsmooth/relay`) run the same file, so a wire-format drift
//! fails a test on every platform.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use chrono::{DateTime, Utc};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use smooth_flow::engine::Engine;
use smooth_flow::store::Pairing;

/// Wire protocol version carried as `v`.
pub const VERSION: u64 = 1;
/// AEAD associated data — binds every ciphertext to this protocol version.
pub const AAD: &[u8] = b"smoothflow-e2e/v1";
/// HKDF info prefix for the pairing key (`|| pairing_id`).
pub const PAIR_INFO: &[u8] = b"smoothflow-pair/v1";
/// HKDF info for per-connection session keys.
pub const SESSION_INFO: &[u8] = b"smoothflow-session/v1";
/// Nonce direction byte: phone → daemon.
pub const DIR_PHONE_TO_DAEMON: u8 = 0;
/// Nonce direction byte: daemon → phone.
pub const DIR_DAEMON_TO_PHONE: u8 = 1;
/// QR deep link prefix.
pub const QR_SCHEME: &str = "smoothflow://pair";
/// A pending pairing (QR on screen) expires after this.
pub const PAIRING_TTL: Duration = Duration::from_mins(5);
/// How long a completed pairing id stays pollable (`GET /api/flow/pair/{id}`).
const COMPLETED_TTL: Duration = Duration::from_mins(10);
/// `last_seen_at` writes are throttled to one per device per this.
const TOUCH_EVERY: Duration = Duration::from_secs(60);
/// Frame the daemon buffers for a paired phone before its session is open.
pub const PENDING_OUT_MAX: usize = 64;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const B64URL: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

// ── primitives ────────────────────────────────────────────────────────────────

/// Standard base64 (for `ct` / `salt`).
pub fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

/// Decode standard base64.
///
/// # Errors
/// On malformed input.
pub fn unb64(s: &str) -> Result<Vec<u8>> {
    B64.decode(s).context("bad base64")
}

/// URL-safe unpadded base64 (for keys and the one-time code — they ride in a URL).
pub fn b64url(bytes: &[u8]) -> String {
    B64URL.encode(bytes)
}

/// Decode URL-safe unpadded base64.
///
/// # Errors
/// On malformed input.
pub fn unb64url(s: &str) -> Result<Vec<u8>> {
    B64URL.decode(s).context("bad base64url")
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Decode hex into exactly 32 bytes.
///
/// # Errors
/// On malformed input or a wrong length.
pub fn unhex32(s: &str) -> Result<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("expected 64 hex chars");
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk)?, 16)?;
    }
    Ok(out)
}

fn array32(v: &[u8], what: &str) -> Result<[u8; 32]> {
    <[u8; 32]>::try_from(v).map_err(|_| anyhow!("{what}: expected 32 bytes, got {}", v.len()))
}

fn array16(v: &[u8], what: &str) -> Result<[u8; 16]> {
    <[u8; 16]>::try_from(v).map_err(|_| anyhow!("{what}: expected 16 bytes, got {}", v.len()))
}

fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut b);
    b
}

fn random16() -> [u8; 16] {
    let mut b = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut b);
    b
}

/// The X25519 public key for a 32-byte secret.
pub fn public_key(secret: &[u8; 32]) -> [u8; 32] {
    x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*secret)).to_bytes()
}

/// X25519 key agreement with the contributory check (a low-order peer point
/// yields the all-zero secret and is refused).
///
/// # Errors
/// When the peer key is a low-order point.
pub fn x25519(secret: &[u8; 32], peer_public: &[u8; 32]) -> Result<[u8; 32]> {
    let ss = x25519_dalek::StaticSecret::from(*secret).diffie_hellman(&x25519_dalek::PublicKey::from(*peer_public));
    if !ss.was_contributory() {
        bail!("peer public key is a low-order point");
    }
    Ok(ss.to_bytes())
}

fn hkdf32(salt: &[u8], ikm: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    // 32 bytes is far under HKDF's 255·32 limit; expand cannot fail.
    if Hkdf::<Sha256>::new(Some(salt), ikm).expand(info, &mut out).is_err() {
        unreachable!("HKDF expand of 32 bytes cannot fail");
    }
    out
}

/// The per-pairing key both sides derive at QR time.
pub fn derive_pairing_key(shared_secret: &[u8; 32], code: &[u8; 16], pairing_id: &str) -> [u8; 32] {
    let mut info = PAIR_INFO.to_vec();
    info.extend_from_slice(pairing_id.as_bytes());
    hkdf32(code, shared_secret, &info)
}

/// The per-connection session key.
pub fn derive_session_key(pairing_key: &[u8; 32], phone_salt: &[u8; 16], daemon_salt: &[u8; 16]) -> [u8; 32] {
    let mut salt = phone_salt.to_vec();
    salt.extend_from_slice(daemon_salt);
    hkdf32(&salt, pairing_key, SESSION_INFO)
}

/// The 12-byte nonce for (direction, counter).
pub fn nonce(dir: u8, n: u64) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[0] = dir;
    out[4..].copy_from_slice(&n.to_be_bytes());
    out
}

/// Seal `plaintext` under `key` for (direction, counter). Output is
/// ciphertext || 16-byte tag.
///
/// # Errors
/// Only on an internal AEAD failure (never for well-formed input).
pub fn seal(key: &[u8; 32], dir: u8, n: u64, plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(key.into());
    cipher
        .encrypt(Nonce::from_slice(&nonce(dir, n)), Payload { msg: plaintext, aad: AAD })
        .map_err(|_| anyhow!("seal failed"))
}

/// Open ciphertext || tag under `key` for (direction, counter).
///
/// # Errors
/// When authentication fails (wrong key, nonce, AAD, or tampering).
pub fn open(key: &[u8; 32], dir: u8, n: u64, ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(Nonce::from_slice(&nonce(dir, n)), Payload { msg: ciphertext, aad: AAD })
        .map_err(|_| anyhow!("authentication failed"))
}

// ── QR payload ────────────────────────────────────────────────────────────────

/// What the QR carries. `to_url` / `parse` are the one wire spelling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QrPayload {
    pub version: u64,
    pub pairing_id: String,
    pub daemon_device: String,
    /// base64url
    pub daemon_public_key: String,
    /// base64url, 16 bytes
    pub code: String,
    pub label: String,
}

impl QrPayload {
    /// `smoothflow://pair?v=1&p=…&d=…&k=…&c=…&l=…`
    pub fn to_url(&self) -> String {
        format!(
            "{QR_SCHEME}?v={}&p={}&d={}&k={}&c={}&l={}",
            self.version,
            urlencode(&self.pairing_id),
            urlencode(&self.daemon_device),
            urlencode(&self.daemon_public_key),
            urlencode(&self.code),
            urlencode(&self.label)
        )
    }

    /// Parse a scanned URL.
    ///
    /// # Errors
    /// When the scheme, version, or a required field is off.
    pub fn parse(url: &str) -> Result<Self> {
        let url = url.trim();
        let Some(query) = url.strip_prefix(QR_SCHEME).and_then(|rest| rest.strip_prefix('?')) else {
            bail!("not a SmoothFlow pairing link");
        };
        let mut fields: HashMap<&str, String> = HashMap::new();
        for kv in query.split('&') {
            if let Some((k, v)) = kv.split_once('=') {
                fields.insert(k, urldecode(v));
            }
        }
        let version: u64 = fields.get("v").and_then(|v| v.parse().ok()).context("missing v")?;
        if version != VERSION {
            bail!("unsupported pairing version {version}");
        }
        let field = |k: &str| fields.get(k).filter(|v| !v.is_empty()).cloned().with_context(|| format!("missing {k}"));
        let out = Self {
            version,
            pairing_id: field("p")?,
            daemon_device: field("d")?,
            daemon_public_key: field("k")?,
            code: field("c")?,
            label: fields.get("l").cloned().unwrap_or_default(),
        };
        // Fail early on junk rather than at HKDF time.
        array32(&unb64url(&out.daemon_public_key)?, "daemon public key")?;
        array16(&unb64url(&out.code)?, "code")?;
        Ok(out)
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex2 = &s[i + 1..i + 3];
                if let Ok(b) = u8::from_str_radix(hex2, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ── wire frames ───────────────────────────────────────────────────────────────

/// A phone's inbound flow frame, classified by its e2e fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    /// No e2e fields — an ordinary plaintext flow frame.
    Plain(String),
    /// `flow.pair` — the pairing hello.
    Pair {
        pairing_id: String,
        phone_public_key: String,
        n: u64,
        ct: String,
    },
    /// `flow.e2e.open` — start a session; `salt` is the phone's 16 bytes (base64).
    Open { salt: String },
    /// A sealed data frame.
    Data { n: u64, ct: String },
    /// Carries `v` but is not a shape we know.
    Malformed(&'static str),
}

/// Classify one inbound flow frame (already known to be `channel:"flow"`).
pub fn classify(frame_text: &str) -> Inbound {
    let Ok(v) = serde_json::from_str::<Value>(frame_text) else {
        return Inbound::Malformed("not JSON");
    };
    if v.get("v").is_none() {
        return Inbound::Plain(frame_text.to_string());
    }
    if v.get("v").and_then(Value::as_u64) != Some(VERSION) {
        return Inbound::Malformed("unsupported e2e version");
    }
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    match s("type").as_deref() {
        Some("flow.pair") => match (s("pair"), s("pk"), v.get("n").and_then(Value::as_u64), s("ct")) {
            (Some(pairing_id), Some(phone_public_key), Some(n), Some(ct)) => Inbound::Pair {
                pairing_id,
                phone_public_key,
                n,
                ct,
            },
            _ => Inbound::Malformed("flow.pair needs pair, pk, n, ct"),
        },
        Some("flow.e2e.open") => s("salt").map_or(Inbound::Malformed("flow.e2e.open needs salt"), |salt| Inbound::Open { salt }),
        Some(_) => Inbound::Malformed("unknown e2e control frame"),
        None => match (v.get("n").and_then(Value::as_u64), s("ct")) {
            (Some(n), Some(ct)) => Inbound::Data { n, ct },
            _ => Inbound::Malformed("data frame needs n and ct"),
        },
    }
}

/// A plaintext `flow.error` the daemon sends when it cannot (or will not)
/// process a phone's frame.
pub fn error_frame(code: &str, message: &str) -> String {
    json!({ "channel": "flow", "type": "flow.error", "ref": Value::Null, "code": code, "message": message }).to_string()
}

/// One direction-pair of an open session: seals daemon→phone, opens phone→daemon.
#[derive(Debug)]
pub struct E2eSession {
    key: [u8; 32],
    send_n: u64,
    recv_last: u64,
}

impl E2eSession {
    pub const fn new(key: [u8; 32]) -> Self {
        Self { key, send_n: 0, recv_last: 0 }
    }

    /// Seal a plaintext flow frame into the wire data frame (daemon→phone).
    ///
    /// # Errors
    /// On an internal AEAD failure.
    pub fn seal_frame(&mut self, plaintext: &str) -> Result<String> {
        self.send_n += 1;
        let ct = seal(&self.key, DIR_DAEMON_TO_PHONE, self.send_n, plaintext.as_bytes())?;
        Ok(json!({ "channel": "flow", "v": VERSION, "n": self.send_n, "ct": b64(&ct) }).to_string())
    }

    /// Open a phone→daemon data frame. Rejects a counter that is not strictly
    /// greater than the last accepted one (replay / reorder), and never
    /// advances the counter on a failed authentication.
    ///
    /// # Errors
    /// On replay or authentication failure.
    pub fn open_frame(&mut self, n: u64, ct_b64: &str) -> Result<String> {
        if n == 0 || n <= self.recv_last {
            bail!("replayed or out-of-order frame (n={n}, last={})", self.recv_last);
        }
        let ct = unb64(ct_b64)?;
        let pt = open(&self.key, DIR_PHONE_TO_DAEMON, n, &ct)?;
        self.recv_last = n;
        String::from_utf8(pt).context("plaintext is not UTF-8")
    }

    /// Frames sealed so far (tests).
    #[must_use]
    pub const fn sent(&self) -> u64 {
        self.send_n
    }
}

// ── pairing state ─────────────────────────────────────────────────────────────

/// A QR that is on screen and has not been scanned yet.
#[derive(Debug, Clone)]
pub struct PendingPairing {
    pub id: String,
    pub code: [u8; 16],
    pub secret: [u8; 32],
    pub public: [u8; 32],
    pub created_at: Instant,
    pub expires_at: DateTime<Utc>,
}

/// What `GET /api/flow/pair/{id}` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PairStatus {
    Pending { expires_at: DateTime<Utc> },
    Paired { device: String, label: String, platform: String },
    Expired,
    Unknown,
}

/// The daemon's pairing authority: pending QRs, the paired-phone cache, the
/// `flow.db` rows behind it. Shared by the pair routes and every relay bridge.
pub struct PairingState {
    engine: Engine,
    daemon_device: String,
    daemon_label: String,
    required: bool,
    pending: Mutex<HashMap<String, PendingPairing>>,
    completed: Mutex<HashMap<String, (Pairing, Instant)>>,
    cache: RwLock<HashMap<String, Pairing>>,
    last_touch: Mutex<HashMap<String, Instant>>,
}

impl PairingState {
    /// Build from the engine (loads the pairing cache). `required` mirrors
    /// `SMOOTH_FLOW_E2E_REQUIRED`: reject plaintext from unpaired phones too.
    pub fn new(engine: Engine, daemon_device: String, daemon_label: String, required: bool) -> Self {
        let cache = engine.pairings().map_or_else(
            |e| {
                tracing::warn!(error = %e, "flow e2e: could not load pairings — treating every phone as unpaired");
                HashMap::new()
            },
            |rows| rows.into_iter().map(|p| (p.device.clone(), p)).collect(),
        );
        Self {
            engine,
            daemon_device,
            daemon_label,
            required,
            pending: Mutex::new(HashMap::new()),
            completed: Mutex::new(HashMap::new()),
            cache: RwLock::new(cache),
            last_touch: Mutex::new(HashMap::new()),
        }
    }

    /// `SMOOTH_FLOW_E2E_REQUIRED` truthiness.
    pub fn required_from_env() -> bool {
        std::env::var("SMOOTH_FLOW_E2E_REQUIRED").is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
    }

    pub fn daemon_device(&self) -> &str {
        &self.daemon_device
    }

    pub fn daemon_label(&self) -> &str {
        &self.daemon_label
    }

    pub const fn plaintext_required_to_fail(&self) -> bool {
        self.required
    }

    /// Mint a pending pairing and return what the QR carries.
    pub fn begin(&self) -> QrPayload {
        self.begin_with(random32(), random16(), uuid::Uuid::new_v4().simple().to_string()[..8].to_string())
    }

    /// [`begin`](Self::begin) with fixed material (tests / fixtures).
    pub fn begin_with(&self, secret: [u8; 32], code: [u8; 16], id: String) -> QrPayload {
        let public = public_key(&secret);
        let now = Utc::now();
        let pending = PendingPairing {
            id: id.clone(),
            code,
            secret,
            public,
            created_at: Instant::now(),
            expires_at: now + chrono::Duration::from_std(PAIRING_TTL).unwrap_or_else(|_| chrono::Duration::minutes(5)),
        };
        let mut map = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        map.retain(|_, p| p.created_at.elapsed() < PAIRING_TTL);
        map.insert(id.clone(), pending);
        QrPayload {
            version: VERSION,
            pairing_id: id,
            daemon_device: self.daemon_device.clone(),
            daemon_public_key: b64url(&public),
            code: b64url(&code),
            label: self.daemon_label.clone(),
        }
    }

    /// Poll a pairing id.
    pub fn status(&self, id: &str) -> PairStatus {
        {
            let done = self.completed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((p, _)) = done.get(id) {
                return PairStatus::Paired {
                    device: p.device.clone(),
                    label: p.label.clone(),
                    platform: p.platform.clone(),
                };
            }
        }
        let map = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match map.get(id) {
            Some(p) if p.created_at.elapsed() < PAIRING_TTL => PairStatus::Pending { expires_at: p.expires_at },
            Some(_) => PairStatus::Expired,
            None => PairStatus::Unknown,
        }
    }

    /// Finish a pairing from the phone's `flow.pair` frame. On success the
    /// pairing is persisted and cached, and the sealed `flow.pair` reply is
    /// returned as wire text.
    ///
    /// # Errors
    /// Unknown/expired id, bad key material, or a hello that does not open
    /// (wrong code) — all of which the caller reports as one generic failure.
    pub fn complete(&self, from_device: &str, pairing_id: &str, phone_public_b64: &str, n: u64, ct_b64: &str) -> Result<String> {
        let pending = {
            let mut map = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(p) = map.get(pairing_id) else { bail!("unknown pairing id") };
            if p.created_at.elapsed() >= PAIRING_TTL {
                map.remove(pairing_id);
                bail!("pairing expired");
            }
            p.clone()
        };
        if n != 1 {
            bail!("pairing hello must use n=1");
        }
        let phone_public = array32(&unb64url(phone_public_b64)?, "phone public key")?;
        let shared = x25519(&pending.secret, &phone_public)?;
        let key = derive_pairing_key(&shared, &pending.code, pairing_id);
        let hello = open(&key, DIR_PHONE_TO_DAEMON, 1, &unb64(ct_b64)?).context("pairing hello did not authenticate")?;
        let hello: Value = serde_json::from_slice(&hello).context("pairing hello is not JSON")?;
        if hello.get("type").and_then(Value::as_str) != Some("flow.pair.hello") {
            bail!("pairing hello has the wrong type");
        }
        let label = hello
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("phone")
            .chars()
            .take(120)
            .collect::<String>();
        let platform = hello.get("platform").and_then(Value::as_str).unwrap_or("").chars().take(32).collect::<String>();
        let pairing = Pairing {
            device: from_device.to_string(),
            label,
            platform,
            public_key: phone_public_b64.to_string(),
            key_hex: hex(&key),
            created_at: Utc::now(),
            last_seen_at: Some(Utc::now()),
        };
        self.engine.upsert_pairing(&pairing).context("persist pairing")?;
        self.cache
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(pairing.device.clone(), pairing.clone());
        {
            let mut map = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            map.remove(pairing_id);
        }
        {
            let mut done = self.completed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            done.retain(|_, (_, at)| at.elapsed() < COMPLETED_TTL);
            done.insert(pairing_id.to_string(), (pairing.clone(), Instant::now()));
        }
        tracing::info!(device = %pairing.device, label = %pairing.label, platform = %pairing.platform, "flow e2e: phone paired");
        let reply = json!({ "type": "flow.pair.ok", "device": self.daemon_device, "label": self.daemon_label, "protocol": VERSION }).to_string();
        let ct = seal(&key, DIR_DAEMON_TO_PHONE, 1, reply.as_bytes())?;
        Ok(json!({ "channel": "flow", "v": VERSION, "type": "flow.pair", "n": 1, "ct": b64(&ct) }).to_string())
    }

    /// The pairing key for a device, if paired.
    pub fn key_for(&self, device: &str) -> Option<[u8; 32]> {
        let cache = self.cache.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.get(device).and_then(|p| unhex32(&p.key_hex).ok())
    }

    /// Whether the device is paired.
    pub fn is_paired(&self, device: &str) -> bool {
        self.cache.read().unwrap_or_else(std::sync::PoisonError::into_inner).contains_key(device)
    }

    /// Open a session for a paired phone from its salt; returns the session and
    /// the daemon's `flow.e2e.open` reply as wire text.
    ///
    /// # Errors
    /// When the device is not paired or the salt is malformed.
    pub fn open_session(&self, device: &str, phone_salt_b64: &str) -> Result<(E2eSession, String)> {
        self.open_session_with(device, phone_salt_b64, random16())
    }

    /// [`open_session`](Self::open_session) with a fixed daemon salt (tests).
    ///
    /// # Errors
    /// When the device is not paired or the salt is malformed.
    pub fn open_session_with(&self, device: &str, phone_salt_b64: &str, daemon_salt: [u8; 16]) -> Result<(E2eSession, String)> {
        let Some(key) = self.key_for(device) else { bail!("device is not paired") };
        let phone_salt = array16(&unb64(phone_salt_b64)?, "phone salt")?;
        let session = E2eSession::new(derive_session_key(&key, &phone_salt, &daemon_salt));
        let reply = json!({ "channel": "flow", "v": VERSION, "type": "flow.e2e.open", "salt": b64(&daemon_salt) }).to_string();
        Ok((session, reply))
    }

    /// Every pairing (secrets stripped by serialization).
    pub fn list(&self) -> Vec<Pairing> {
        let cache = self.cache.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut v: Vec<Pairing> = cache.values().cloned().collect();
        v.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.device.cmp(&b.device)));
        v
    }

    /// Revoke a pairing. Live bridges notice on their next frame.
    ///
    /// # Errors
    /// On a store failure.
    pub fn revoke(&self, device: &str) -> Result<bool> {
        let removed = self.engine.remove_pairing(device)?;
        self.cache.write().unwrap_or_else(std::sync::PoisonError::into_inner).remove(device);
        if removed {
            tracing::info!(%device, "flow e2e: pairing revoked");
        }
        Ok(removed)
    }

    /// Note the phone was heard from (throttled write).
    pub fn touch(&self, device: &str) {
        let mut last = self.last_touch.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.get(device).is_some_and(|t| t.elapsed() < TOUCH_EVERY) {
            return;
        }
        last.insert(device.to_string(), Instant::now());
        drop(last);
        let now = Utc::now();
        if let Err(e) = self.engine.touch_pairing(device, now) {
            tracing::debug!(error = %e, %device, "flow e2e: touch failed");
        }
        if let Some(p) = self.cache.write().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(device) {
            p.last_seen_at = Some(now);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
pub(crate) mod tests {
    use super::*;

    /// Deterministic material used by the checked-in fixture. Changing any of
    /// it changes `tests/fixtures/flow-e2e-v1.json` — regenerate with
    /// `SMOOTH_E2E_WRITE_FIXTURE=1 cargo test -p smooai-smooth-daemon fixture`
    /// and copy the file to the smooai relay packages.
    pub const DAEMON_SECRET: [u8; 32] = [
        0x77, 0x07, 0x6d, 0x0a, 0x73, 0x18, 0xa5, 0x7d, 0x3c, 0x16, 0xc1, 0x72, 0x51, 0xb2, 0x66, 0x45, 0xdf, 0x4c, 0x2f, 0x87, 0xeb, 0xc0, 0x99, 0x2a, 0xb1,
        0x77, 0xfb, 0xa5, 0x1d, 0xb9, 0x2c, 0x2a,
    ];
    pub const PHONE_SECRET: [u8; 32] = [
        0x5d, 0xab, 0x08, 0x7e, 0x62, 0x4a, 0x8a, 0x4b, 0x79, 0xe1, 0x7f, 0x8b, 0x83, 0x80, 0x0e, 0xe6, 0x6f, 0x3b, 0xb1, 0x29, 0x26, 0x18, 0xb6, 0xfd, 0x1c,
        0x2f, 0x8b, 0x27, 0xff, 0x88, 0xe0, 0xeb,
    ];
    pub const CODE: [u8; 16] = [0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
    pub const PHONE_SALT: [u8; 16] = [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae, 0xaf];
    pub const DAEMON_SALT: [u8; 16] = [0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xbb, 0xbc, 0xbd, 0xbe, 0xbf];
    pub const PAIRING_ID: &str = "1a2b3c4d";
    pub const DAEMON_DEVICE: &str = "daemon-0123456789ab";
    pub const DAEMON_LABEL: &str = "smoo-hub";
    pub const PHONE_DEVICE: &str = "phone-fedcba987654";

    pub fn engine() -> Engine {
        let dir = tempfile::tempdir().unwrap();
        let cfg = smooth_flow::EngineConfig {
            db_path: dir.path().join("flow.db"),
            ..smooth_flow::EngineConfig::new(dir.path().to_path_buf())
        };
        // Keep the tempdir alive for the process: the store holds the file open.
        std::mem::forget(dir);
        Engine::open(cfg).unwrap()
    }

    pub fn state() -> PairingState {
        PairingState::new(engine(), DAEMON_DEVICE.into(), DAEMON_LABEL.into(), false)
    }

    /// Phone side of pairing, as the apps implement it.
    pub fn phone_pair_frame(qr: &QrPayload, hello: &str) -> (String, [u8; 32]) {
        let daemon_pub = array32(&unb64url(&qr.daemon_public_key).unwrap(), "k").unwrap();
        let code = array16(&unb64url(&qr.code).unwrap(), "c").unwrap();
        let shared = x25519(&PHONE_SECRET, &daemon_pub).unwrap();
        let key = derive_pairing_key(&shared, &code, &qr.pairing_id);
        let ct = seal(&key, DIR_PHONE_TO_DAEMON, 1, hello.as_bytes()).unwrap();
        let frame = json!({
            "channel": "flow", "v": VERSION, "type": "flow.pair",
            "pair": qr.pairing_id, "pk": b64url(&public_key(&PHONE_SECRET)), "n": 1, "ct": b64(&ct)
        })
        .to_string();
        (frame, key)
    }

    // ── primitives ──────────────────────────────────────────────────────────

    #[test]
    fn x25519_matches_rfc7748_vector() {
        // RFC 7748 §6.1: Alice's private / Bob's public → the shared secret K.
        let alice = DAEMON_SECRET;
        let bob_pub: [u8; 32] = [
            0xde, 0x9e, 0xdb, 0x7d, 0x7b, 0x7d, 0xc1, 0xb4, 0xd3, 0x5b, 0x61, 0xc2, 0xec, 0xe4, 0x35, 0x37, 0x3f, 0x83, 0x43, 0xc8, 0x5b, 0x78, 0x67, 0x4d,
            0xad, 0xfc, 0x7e, 0x14, 0x6f, 0x88, 0x2b, 0x4f,
        ];
        assert_eq!(hex(&public_key(&alice)), "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        assert_eq!(
            hex(&x25519(&alice, &bob_pub).unwrap()),
            "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742"
        );
        // Both directions agree.
        assert_eq!(
            x25519(&DAEMON_SECRET, &public_key(&PHONE_SECRET)).unwrap(),
            x25519(&PHONE_SECRET, &public_key(&DAEMON_SECRET)).unwrap()
        );
    }

    #[test]
    fn x25519_refuses_low_order_points() {
        assert!(x25519(&DAEMON_SECRET, &[0u8; 32]).is_err());
        let mut one = [0u8; 32];
        one[0] = 1;
        assert!(x25519(&DAEMON_SECRET, &one).is_err());
    }

    #[test]
    fn nonce_layout_is_direction_then_big_endian_counter() {
        assert_eq!(hex(&nonce(0, 1)), "000000000000000000000001");
        assert_eq!(hex(&nonce(1, 0x0102_0304_0506_0708)), "010000000102030405060708");
    }

    #[test]
    fn seal_open_round_trip_and_tamper_detection() {
        let key = [7u8; 32];
        let ct = seal(&key, 0, 1, b"hello").unwrap();
        assert_eq!(ct.len(), 5 + 16);
        assert_eq!(open(&key, 0, 1, &ct).unwrap(), b"hello");
        assert!(open(&key, 1, 1, &ct).is_err(), "direction is part of the nonce");
        assert!(open(&key, 0, 2, &ct).is_err(), "counter is part of the nonce");
        assert!(open(&[8u8; 32], 0, 1, &ct).is_err(), "wrong key");
        let mut bad = ct.clone();
        bad[0] ^= 1;
        assert!(open(&key, 0, 1, &bad).is_err(), "flipped ciphertext bit");
        let mut bad = ct;
        bad[20] ^= 1;
        assert!(open(&key, 0, 1, &bad).is_err(), "flipped tag bit");
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(unhex32(&hex(&DAEMON_SECRET)).unwrap(), DAEMON_SECRET);
        assert!(unhex32("zz").is_err());
        assert!(unhex32(&"0".repeat(63)).is_err());
    }

    // ── QR ──────────────────────────────────────────────────────────────────

    #[test]
    fn qr_url_round_trips_and_validates() {
        let st = state();
        let qr = st.begin_with(DAEMON_SECRET, CODE, PAIRING_ID.into());
        let url = qr.to_url();
        assert!(url.starts_with("smoothflow://pair?v=1&p=1a2b3c4d&d=daemon-0123456789ab&k="), "{url}");
        assert_eq!(QrPayload::parse(&url).unwrap(), qr);
        assert!(QrPayload::parse("https://example.com/?v=1").is_err());
        assert!(QrPayload::parse(&url.replace("v=1", "v=2")).is_err());
        assert!(QrPayload::parse(&url.replace("&c=", "&c=short")).is_err(), "code must be 16 bytes");
        // Labels with spaces survive.
        let st2 = PairingState::new(engine(), "daemon-x".into(), "Brent's Mac Studio".into(), false);
        let qr2 = st2.begin();
        assert_eq!(QrPayload::parse(&qr2.to_url()).unwrap().label, "Brent's Mac Studio");
    }

    // ── classify ────────────────────────────────────────────────────────────

    #[test]
    fn classify_recognises_every_shape() {
        assert_eq!(
            classify(r#"{"channel":"flow","type":"flow.hello"}"#),
            Inbound::Plain(r#"{"channel":"flow","type":"flow.hello"}"#.into())
        );
        assert_eq!(
            classify(r#"{"channel":"flow","v":1,"type":"flow.pair","pair":"p","pk":"k","n":1,"ct":"c"}"#),
            Inbound::Pair {
                pairing_id: "p".into(),
                phone_public_key: "k".into(),
                n: 1,
                ct: "c".into()
            }
        );
        assert_eq!(
            classify(r#"{"channel":"flow","v":1,"type":"flow.e2e.open","salt":"s"}"#),
            Inbound::Open { salt: "s".into() }
        );
        assert_eq!(classify(r#"{"channel":"flow","v":1,"n":7,"ct":"c"}"#), Inbound::Data { n: 7, ct: "c".into() });
        assert!(matches!(classify(r#"{"channel":"flow","v":2,"n":7,"ct":"c"}"#), Inbound::Malformed(_)));
        assert!(matches!(classify(r#"{"channel":"flow","v":1,"n":7}"#), Inbound::Malformed(_)));
        assert!(matches!(classify(r#"{"channel":"flow","v":1,"type":"flow.pair"}"#), Inbound::Malformed(_)));
        assert!(matches!(classify("nope"), Inbound::Malformed(_)));
    }

    // ── pairing end to end ──────────────────────────────────────────────────

    #[test]
    fn pairing_completes_persists_and_answers_sealed() {
        let st = state();
        let qr = st.begin_with(DAEMON_SECRET, CODE, PAIRING_ID.into());
        assert!(matches!(st.status(PAIRING_ID), PairStatus::Pending { .. }));
        let (frame, key) = phone_pair_frame(&qr, r#"{"type":"flow.pair.hello","label":"Brent's iPhone","platform":"ios"}"#);
        let Inbound::Pair {
            pairing_id,
            phone_public_key,
            n,
            ct,
        } = classify(&frame)
        else {
            panic!("not a pair frame")
        };
        let reply = st.complete(PHONE_DEVICE, &pairing_id, &phone_public_key, n, &ct).unwrap();
        // The reply opens under the same key, direction 1, n=1.
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["type"], "flow.pair");
        let pt = open(&key, DIR_DAEMON_TO_PHONE, 1, &unb64(v["ct"].as_str().unwrap()).unwrap()).unwrap();
        let pt: Value = serde_json::from_slice(&pt).unwrap();
        assert_eq!(pt["type"], "flow.pair.ok");
        assert_eq!(pt["device"], DAEMON_DEVICE);
        assert_eq!(pt["label"], DAEMON_LABEL);
        // Persisted + cached + pollable.
        assert_eq!(st.key_for(PHONE_DEVICE).unwrap(), key);
        assert!(st.is_paired(PHONE_DEVICE));
        let rows = st.engine.pairings().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Brent's iPhone");
        assert_eq!(rows[0].platform, "ios");
        assert_eq!(rows[0].key_hex, hex(&key));
        assert!(matches!(st.status(PAIRING_ID), PairStatus::Paired { ref device, .. } if device == PHONE_DEVICE));
        // The id is single-use.
        assert!(st.complete(PHONE_DEVICE, &pairing_id, &phone_public_key, n, &ct).is_err());
    }

    #[test]
    fn pairing_with_the_wrong_code_fails_and_stores_nothing() {
        let st = state();
        let qr = st.begin_with(DAEMON_SECRET, CODE, PAIRING_ID.into());
        let mut wrong = qr.clone();
        wrong.code = b64url(&[0u8; 16]);
        let (frame, _) = phone_pair_frame(&wrong, r#"{"type":"flow.pair.hello","label":"x","platform":"ios"}"#);
        let Inbound::Pair {
            pairing_id,
            phone_public_key,
            n,
            ct,
        } = classify(&frame)
        else {
            panic!()
        };
        assert!(st.complete(PHONE_DEVICE, &pairing_id, &phone_public_key, n, &ct).is_err());
        assert!(!st.is_paired(PHONE_DEVICE));
        assert!(st.engine.pairings().unwrap().is_empty());
        // Still pending — a wrong scan doesn't burn the QR.
        assert!(matches!(st.status(PAIRING_ID), PairStatus::Pending { .. }));
    }

    #[test]
    fn pairing_rejects_unknown_id_bad_key_and_wrong_counter() {
        let st = state();
        let qr = st.begin_with(DAEMON_SECRET, CODE, PAIRING_ID.into());
        let (frame, _) = phone_pair_frame(&qr, r#"{"type":"flow.pair.hello","label":"x","platform":"ios"}"#);
        let Inbound::Pair {
            pairing_id,
            phone_public_key,
            ct,
            ..
        } = classify(&frame)
        else {
            panic!()
        };
        assert!(st.complete(PHONE_DEVICE, "nope", &phone_public_key, 1, &ct).is_err());
        assert!(st.complete(PHONE_DEVICE, &pairing_id, "AAAA", 1, &ct).is_err(), "short key");
        assert!(st.complete(PHONE_DEVICE, &pairing_id, &b64url(&[0u8; 32]), 1, &ct).is_err(), "low-order point");
        assert!(st.complete(PHONE_DEVICE, &pairing_id, &phone_public_key, 2, &ct).is_err(), "n must be 1");
        assert!(matches!(st.status("nope"), PairStatus::Unknown));
    }

    fn paired_state() -> (PairingState, [u8; 32]) {
        let st = state();
        let qr = st.begin_with(DAEMON_SECRET, CODE, PAIRING_ID.into());
        let (frame, key) = phone_pair_frame(&qr, r#"{"type":"flow.pair.hello","label":"Brent's iPhone","platform":"ios"}"#);
        let Inbound::Pair {
            pairing_id,
            phone_public_key,
            n,
            ct,
        } = classify(&frame)
        else {
            panic!()
        };
        st.complete(PHONE_DEVICE, &pairing_id, &phone_public_key, n, &ct).unwrap();
        (st, key)
    }

    #[test]
    fn sessions_derive_the_same_key_on_both_sides_and_count_per_direction() {
        let (st, pairing_key) = paired_state();
        let (mut daemon, reply) = st.open_session_with(PHONE_DEVICE, &b64(&PHONE_SALT), DAEMON_SALT).unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["type"], "flow.e2e.open");
        let daemon_salt = array16(&unb64(v["salt"].as_str().unwrap()).unwrap(), "ds").unwrap();
        let phone_key = derive_session_key(&pairing_key, &PHONE_SALT, &daemon_salt);
        let mut phone_n = 0u64;
        // phone → daemon
        for text in [
            r#"{"channel":"flow","type":"flow.hello"}"#,
            r#"{"channel":"flow","type":"flow.attach","id":"fs-1","cols":80,"rows":24}"#,
        ] {
            phone_n += 1;
            let ct = seal(&phone_key, DIR_PHONE_TO_DAEMON, phone_n, text.as_bytes()).unwrap();
            assert_eq!(daemon.open_frame(phone_n, &b64(&ct)).unwrap(), text);
        }
        // daemon → phone
        let wire = daemon.seal_frame(r#"{"channel":"flow","type":"flow.hello","sessions":[]}"#).unwrap();
        let v: Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["n"], 1);
        assert!(v.get("type").is_none(), "data frames carry no type in the clear");
        let pt = open(&phone_key, DIR_DAEMON_TO_PHONE, 1, &unb64(v["ct"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(pt, br#"{"channel":"flow","type":"flow.hello","sessions":[]}"#);
        assert_eq!(daemon.seal_frame("{}").map(|_| daemon.sent()).unwrap(), 2);
    }

    #[test]
    fn replayed_and_reordered_frames_are_rejected() {
        let (st, pairing_key) = paired_state();
        let (mut daemon, _) = st.open_session_with(PHONE_DEVICE, &b64(&PHONE_SALT), DAEMON_SALT).unwrap();
        let phone_key = derive_session_key(&pairing_key, &PHONE_SALT, &DAEMON_SALT);
        let f = |n: u64| b64(&seal(&phone_key, DIR_PHONE_TO_DAEMON, n, b"{}").unwrap());
        assert!(daemon.open_frame(1, &f(1)).is_ok());
        assert!(daemon.open_frame(1, &f(1)).is_err(), "exact replay");
        assert!(daemon.open_frame(0, &f(0)).is_err(), "zero counter");
        assert!(daemon.open_frame(3, &f(3)).is_ok(), "gaps are fine (a dropped frame)");
        assert!(daemon.open_frame(2, &f(2)).is_err(), "reorder below the high-water mark");
        // A failed auth must not advance the counter.
        assert!(daemon.open_frame(5, &f(4)).is_err(), "nonce mismatch");
        assert!(daemon.open_frame(5, &f(5)).is_ok(), "5 is still available after the failed attempt");
        // An old session's frames don't open a new session (the daemon salt differs).
        let (mut fresh, _) = st.open_session_with(PHONE_DEVICE, &b64(&PHONE_SALT), [0xcc; 16]).unwrap();
        assert!(fresh.open_frame(1, &f(1)).is_err(), "cross-session replay");
    }

    #[test]
    fn revoke_drops_the_key_and_lists_shrink() {
        let (st, _) = paired_state();
        assert_eq!(st.list().len(), 1);
        assert!(st.revoke(PHONE_DEVICE).unwrap());
        assert!(!st.is_paired(PHONE_DEVICE));
        assert!(st.key_for(PHONE_DEVICE).is_none());
        assert!(st.list().is_empty());
        assert!(st.engine.pairings().unwrap().is_empty());
        assert!(!st.revoke(PHONE_DEVICE).unwrap());
        assert!(st.open_session(PHONE_DEVICE, &b64(&PHONE_SALT)).is_err());
    }

    #[test]
    fn pairings_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = || smooth_flow::EngineConfig {
            db_path: dir.path().join("flow.db"),
            ..smooth_flow::EngineConfig::new(dir.path().to_path_buf())
        };
        let st = PairingState::new(Engine::open(cfg()).unwrap(), DAEMON_DEVICE.into(), DAEMON_LABEL.into(), false);
        let qr = st.begin_with(DAEMON_SECRET, CODE, PAIRING_ID.into());
        let (frame, key) = phone_pair_frame(&qr, r#"{"type":"flow.pair.hello","label":"x","platform":"android"}"#);
        let Inbound::Pair {
            pairing_id,
            phone_public_key,
            n,
            ct,
        } = classify(&frame)
        else {
            panic!()
        };
        st.complete(PHONE_DEVICE, &pairing_id, &phone_public_key, n, &ct).unwrap();
        drop(st);
        let again = PairingState::new(Engine::open(cfg()).unwrap(), DAEMON_DEVICE.into(), DAEMON_LABEL.into(), false);
        assert_eq!(again.key_for(PHONE_DEVICE).unwrap(), key);
        assert_eq!(again.list()[0].platform, "android");
    }

    #[test]
    fn touch_is_throttled_but_records_last_seen() {
        let (st, _) = paired_state();
        st.touch(PHONE_DEVICE);
        let first = st.list()[0].last_seen_at.unwrap();
        st.touch(PHONE_DEVICE);
        assert_eq!(st.list()[0].last_seen_at.unwrap(), first, "second touch inside the window is a no-op");
        assert!(st.engine.pairing(PHONE_DEVICE).unwrap().unwrap().last_seen_at.is_some());
    }

    #[test]
    fn required_from_env_parses_truthy() {
        for v in ["1", "true", "yes", "on"] {
            std::env::set_var("SMOOTH_FLOW_E2E_REQUIRED", v);
            assert!(PairingState::required_from_env(), "{v}");
        }
        std::env::set_var("SMOOTH_FLOW_E2E_REQUIRED", "0");
        assert!(!PairingState::required_from_env());
        std::env::remove_var("SMOOTH_FLOW_E2E_REQUIRED");
        assert!(!PairingState::required_from_env());
    }

    // ── the shared fixture ──────────────────────────────────────────────────

    fn fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/flow-e2e-v1.json")
    }

    /// Build the fixture from the fixed material above, entirely through the
    /// public primitives (so the fixture documents the algorithm, not this
    /// crate's internals).
    fn build_fixture() -> Value {
        let daemon_pub = public_key(&DAEMON_SECRET);
        let phone_pub = public_key(&PHONE_SECRET);
        let shared = x25519(&PHONE_SECRET, &daemon_pub).unwrap();
        let pairing_key = derive_pairing_key(&shared, &CODE, PAIRING_ID);
        let qr = QrPayload {
            version: VERSION,
            pairing_id: PAIRING_ID.into(),
            daemon_device: DAEMON_DEVICE.into(),
            daemon_public_key: b64url(&daemon_pub),
            code: b64url(&CODE),
            label: DAEMON_LABEL.into(),
        };
        let hello = r#"{"type":"flow.pair.hello","label":"Brent's iPhone","platform":"ios"}"#;
        let hello_ct = seal(&pairing_key, DIR_PHONE_TO_DAEMON, 1, hello.as_bytes()).unwrap();
        let reply = format!(r#"{{"type":"flow.pair.ok","device":"{DAEMON_DEVICE}","label":"{DAEMON_LABEL}","protocol":1}}"#);
        let reply_ct = seal(&pairing_key, DIR_DAEMON_TO_PHONE, 1, reply.as_bytes()).unwrap();
        let session_key = derive_session_key(&pairing_key, &PHONE_SALT, &DAEMON_SALT);
        let frames = [
            (DIR_PHONE_TO_DAEMON, 1u64, r#"{"channel":"flow","type":"flow.hello"}"#),
            (
                DIR_DAEMON_TO_PHONE,
                1u64,
                r#"{"channel":"flow","type":"flow.hello","daemon":{"version":"0.44.0","machine_label":"smoo-hub"},"sessions":[]}"#,
            ),
            (
                DIR_PHONE_TO_DAEMON,
                2u64,
                r#"{"channel":"flow","type":"flow.input","id":"fs-1a2b3c4d","data_b64":"bHMgLWxhCg=="}"#,
            ),
            (
                DIR_DAEMON_TO_PHONE,
                2u64,
                r#"{"channel":"flow","type":"flow.output","id":"fs-1a2b3c4d","seq":1,"data_b64":"dG90YWwgMAo="}"#,
            ),
        ]
        .into_iter()
        .map(|(dir, n, pt)| {
            let ct = seal(&session_key, dir, n, pt.as_bytes()).unwrap();
            json!({ "dir": dir, "n": n, "nonce_hex": hex(&nonce(dir, n)), "plaintext": pt, "ct_b64": b64(&ct),
                    "wire": { "channel": "flow", "v": VERSION, "n": n, "ct": b64(&ct) } })
        })
        .collect::<Vec<_>>();
        json!({
            "_comment": "SmoothFlow relay E2E test vectors (th-d98fde). Generated by smooth-daemon's flow_e2e tests; the Swift and Kotlin relay packages assert the same values. Do not hand-edit.",
            "version": VERSION,
            "aad": String::from_utf8(AAD.to_vec()).unwrap(),
            "pair_info_prefix": String::from_utf8(PAIR_INFO.to_vec()).unwrap(),
            "session_info": String::from_utf8(SESSION_INFO.to_vec()).unwrap(),
            "pairing_id": PAIRING_ID,
            "daemon_device": DAEMON_DEVICE,
            "daemon_label": DAEMON_LABEL,
            "phone_device": PHONE_DEVICE,
            "daemon_secret_hex": hex(&DAEMON_SECRET),
            "daemon_public_b64url": b64url(&daemon_pub),
            "phone_secret_hex": hex(&PHONE_SECRET),
            "phone_public_b64url": b64url(&phone_pub),
            "code_b64url": b64url(&CODE),
            "qr_url": qr.to_url(),
            "shared_secret_hex": hex(&shared),
            "pairing_key_hex": hex(&pairing_key),
            "hello": { "plaintext": hello, "dir": DIR_PHONE_TO_DAEMON, "n": 1, "nonce_hex": hex(&nonce(DIR_PHONE_TO_DAEMON, 1)), "ct_b64": b64(&hello_ct),
                       "wire": { "channel": "flow", "v": VERSION, "type": "flow.pair", "pair": PAIRING_ID, "pk": b64url(&phone_pub), "n": 1, "ct": b64(&hello_ct) } },
            "reply": { "plaintext": reply, "dir": DIR_DAEMON_TO_PHONE, "n": 1, "ct_b64": b64(&reply_ct),
                       "wire": { "channel": "flow", "v": VERSION, "type": "flow.pair", "n": 1, "ct": b64(&reply_ct) } },
            "phone_salt_b64": b64(&PHONE_SALT),
            "daemon_salt_b64": b64(&DAEMON_SALT),
            "open": { "phone_wire": { "channel": "flow", "v": VERSION, "type": "flow.e2e.open", "salt": b64(&PHONE_SALT) },
                      "daemon_wire": { "channel": "flow", "v": VERSION, "type": "flow.e2e.open", "salt": b64(&DAEMON_SALT) } },
            "session_key_hex": hex(&session_key),
            "frames": frames,
            "error_frame": { "e2e_required": error_frame("e2e_required", "this phone is paired; send encrypted frames") },
        })
    }

    #[test]
    fn fixture_on_disk_matches_this_implementation() {
        let want = build_fixture();
        let path = fixture_path();
        if std::env::var("SMOOTH_E2E_WRITE_FIXTURE").is_ok() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, serde_json::to_string_pretty(&want).unwrap() + "\n").unwrap();
        }
        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap();
        assert_eq!(
            on_disk, want,
            "regenerate with SMOOTH_E2E_WRITE_FIXTURE=1 and copy to the smooai relay packages"
        );
    }

    /// The daemon side, driven purely by the fixture's phone-side values —
    /// exactly what a phone implementation must produce.
    #[test]
    fn daemon_accepts_the_fixture_phone_frames() {
        let fx: Value = serde_json::from_str(&std::fs::read_to_string(fixture_path()).unwrap()).unwrap();
        let st = state();
        let qr = st.begin_with(DAEMON_SECRET, CODE, PAIRING_ID.into());
        assert_eq!(qr.to_url(), fx["qr_url"]);
        let hello = &fx["hello"]["wire"];
        let reply = st
            .complete(
                PHONE_DEVICE,
                hello["pair"].as_str().unwrap(),
                hello["pk"].as_str().unwrap(),
                hello["n"].as_u64().unwrap(),
                hello["ct"].as_str().unwrap(),
            )
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap(), fx["reply"]["wire"]);
        let (mut session, open_reply) = st
            .open_session_with(PHONE_DEVICE, fx["open"]["phone_wire"]["salt"].as_str().unwrap(), DAEMON_SALT)
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&open_reply).unwrap(), fx["open"]["daemon_wire"]);
        for f in fx["frames"].as_array().unwrap() {
            let n = f["n"].as_u64().unwrap();
            if f["dir"].as_u64().unwrap() == u64::from(DIR_PHONE_TO_DAEMON) {
                assert_eq!(session.open_frame(n, f["ct_b64"].as_str().unwrap()).unwrap(), f["plaintext"]);
            } else {
                assert_eq!(
                    serde_json::from_str::<Value>(&session.seal_frame(f["plaintext"].as_str().unwrap()).unwrap()).unwrap(),
                    f["wire"]
                );
            }
        }
    }
}

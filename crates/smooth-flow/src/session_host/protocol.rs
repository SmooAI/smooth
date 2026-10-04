//! The daemon ⇄ `flow-host` wire format (docs/Architecture/SmoothFlow-Session-Host.md).
//!
//! A frame is `u32 BE total | u32 BE header length | JSON header | body`.
//! The header is a JSON object with a `type`; bulk bytes (PTY output, input,
//! snapshots, screen text) travel in the body, never base64 inside JSON.
//! Both size limits are checked before anything is allocated, so a hostile
//! length can't make a peer reserve 4 GiB.
//!
//! Message types are split by direction ([`ClientMsg`] daemon → host,
//! [`HostMsg`] host → daemon). Unknown header fields are ignored (serde's
//! default), which is what makes additive changes free. An unknown `type`
//! is told apart from a malformed known one by [`classify`], because the spec
//! answers the two differently.

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

/// The highest IPC version this build speaks.
pub const PROTOCOL: u32 = 1;
/// The lowest IPC version this build speaks. The compatibility window: a
/// daemon must speak every version a host from the previous two minor
/// releases may speak.
pub const PROTOCOL_MIN: u32 = 1;
/// The host record's schema version (`v`).
pub const RECORD_V: u32 = 1;
/// The most a frame may carry after its length prefix.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
/// The most a frame's JSON header may be.
pub const MAX_HEADER: usize = 64 * 1024;
/// The largest snapshot body that still fits a frame with a full header.
pub const MAX_BODY: usize = MAX_FRAME - MAX_HEADER - 4;

/// Error codes an `error` message carries.
pub mod code {
    pub const AUTH: &str = "auth";
    pub const VERSION: &str = "version";
    pub const SUPERSEDED: &str = "superseded";
    pub const FRAME_TOO_LARGE: &str = "frame_too_large";
    pub const UNKNOWN_TYPE: &str = "unknown_type";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const NOT_EXITED: &str = "not_exited";
    pub const UNKNOWN_KEY: &str = "unknown_key";
}

/// One decoded frame: the header object and the body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub header: Value,
    pub body: Vec<u8>,
}

impl Frame {
    /// The header's `type`, when it has a string one.
    #[must_use]
    pub fn kind(&self) -> Option<&str> {
        self.header.get("type").and_then(Value::as_str)
    }

    /// The header's `req`, when it has a numeric one.
    #[must_use]
    pub fn req(&self) -> Option<u64> {
        self.header.get("req").and_then(Value::as_u64)
    }
}

/// Why a frame could not be read.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The stream failed or ended mid-frame.
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    /// A length over [`MAX_FRAME`] or [`MAX_HEADER`], or a header length
    /// larger than the frame. The peer is answered `frame_too_large`.
    #[error("frame too large: {0}")]
    TooLarge(String),
    /// The header is not a JSON object with a string `type`.
    #[error("bad header: {0}")]
    BadHeader(String),
}

impl FrameError {
    /// The `error` code the receiver answers this with.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooLarge(_) => code::FRAME_TOO_LARGE,
            Self::Io(_) | Self::BadHeader(_) => code::BAD_REQUEST,
        }
    }
}

/// Encode a frame. `header` must serialize to a JSON object.
///
/// # Errors
/// When the header does not serialize, or the frame is over a limit.
pub fn encode<H: Serialize + ?Sized>(header: &H, body: &[u8]) -> io::Result<Vec<u8>> {
    let h = serde_json::to_vec(header).map_err(io::Error::other)?;
    if h.len() > MAX_HEADER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("header of {} bytes is over {MAX_HEADER}", h.len()),
        ));
    }
    let total = 4 + h.len() + body.len();
    if total > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("frame of {total} bytes is over {MAX_FRAME}"),
        ));
    }
    let mut out = Vec::with_capacity(4 + total);
    // Both fit u32: checked against the limits above.
    out.extend_from_slice(&u32::try_from(total).map_err(io::Error::other)?.to_be_bytes());
    out.extend_from_slice(&u32::try_from(h.len()).map_err(io::Error::other)?.to_be_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(body);
    Ok(out)
}

/// Encode and write a frame in one `write_all`.
///
/// # Errors
/// As [`encode`], or when the write fails.
pub fn write_frame<W: Write + ?Sized, H: Serialize + ?Sized>(w: &mut W, header: &H, body: &[u8]) -> io::Result<()> {
    let bytes = encode(header, body)?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Read one frame. `Ok(None)` is a clean end of stream at a frame boundary.
///
/// # Errors
/// [`FrameError::Io`] for a failed read or an end of stream mid-frame,
/// [`FrameError::TooLarge`] / [`FrameError::BadHeader`] for a protocol
/// violation (the stream is then unusable: close it).
pub fn read_frame<R: Read + ?Sized>(r: &mut R) -> Result<Option<Frame>, FrameError> {
    let mut len = [0u8; 4];
    // A clean EOF is only allowed before the first byte of a frame.
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let total = u32::from_be_bytes(len) as usize;
    if total > MAX_FRAME {
        return Err(FrameError::TooLarge(format!("frame length {total} is over {MAX_FRAME}")));
    }
    if total < 4 {
        return Err(FrameError::BadHeader(format!("frame length {total} has no room for a header length")));
    }
    let mut hlen = [0u8; 4];
    r.read_exact(&mut hlen)?;
    let hlen = u32::from_be_bytes(hlen) as usize;
    if hlen > MAX_HEADER {
        return Err(FrameError::TooLarge(format!("header length {hlen} is over {MAX_HEADER}")));
    }
    if hlen > total - 4 {
        return Err(FrameError::TooLarge(format!("header length {hlen} is over the frame's {}", total - 4)));
    }
    let mut header = vec![0u8; hlen];
    r.read_exact(&mut header)?;
    let mut body = vec![0u8; total - 4 - hlen];
    r.read_exact(&mut body)?;
    let header: Value = serde_json::from_slice(&header).map_err(|e| FrameError::BadHeader(e.to_string()))?;
    if !header.get("type").is_some_and(Value::is_string) {
        return Err(FrameError::BadHeader("header is not an object with a string \"type\"".into()));
    }
    Ok(Some(Frame { header, body }))
}

/// What a frame's header is, for the receiver's dispatch.
#[derive(Debug)]
pub enum Classified<T> {
    Known(T),
    /// A `type` this side does not know: answer `unknown_type`, keep the
    /// connection.
    Unknown(String),
    /// A known `type` whose fields don't parse: answer `bad_request`.
    Bad(String),
}

/// Parse a frame's header as `T` (a `#[serde(tag = "type")]` enum), telling
/// an unknown `type` apart from a malformed known one. `known` lists the
/// type names `T` has.
#[must_use]
pub fn classify<T: DeserializeOwned>(frame: &Frame, known: &[&str]) -> Classified<T> {
    let kind = frame.kind().unwrap_or_default();
    if !known.contains(&kind) {
        return Classified::Unknown(kind.to_string());
    }
    match serde_json::from_value(frame.header.clone()) {
        Ok(m) => Classified::Known(m),
        Err(e) => Classified::Bad(format!("{kind}: {e}")),
    }
}

/// The highest version both sides speak: `offer` is the daemon's inclusive
/// range, `host` the host's.
#[must_use]
pub fn negotiate(offer: (u32, u32), host: (u32, u32)) -> Option<u32> {
    let lo = offer.0.max(host.0);
    let hi = offer.1.min(host.1);
    (lo <= hi).then_some(hi)
}

/// Compare two secrets in time that depends only on their lengths.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// How the child ended. In the record (with `at`) and in `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitInfo {
    /// The exit status, or `None` when a signal ended it.
    pub code: Option<i32>,
    /// The terminating signal, or `None`.
    pub signal: Option<i32>,
    /// RFC 3339, when the host saw it.
    pub at: String,
}

/// `kill`'s first signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KillSignal {
    #[default]
    Term,
    Kill,
}

/// Input modes a `screen` answer reports, all read from the host's VT.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Modes {
    pub bracketed_paste: bool,
    pub cursor_keys_app: bool,
    /// `x10` (9), `normal` (1000), `button` (1002), `any` (1003), or `None`.
    pub mouse_tracking: Option<String>,
    /// `x10` (the default), `utf8` (1005), `sgr` (1006), `urxvt` (1015),
    /// `sgr_pixels` (1016).
    pub mouse_format: String,
    /// Alternate scroll (1007).
    pub alt_scroll: bool,
    /// Kitty keyboard flags; 0 is the legacy encoding.
    pub kitty_keyboard: u8,
}

/// daemon → host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        /// The inclusive range of versions the daemon speaks.
        protocol: (u32, u32),
        token: String,
        #[serde(default)]
        client: String,
    },
    /// Body: bytes for the PTY, as-is.
    Input,
    /// Body: UTF-8 text, encoded by `Vt::encode_paste`.
    Paste,
    Key {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repeat: Option<u32>,
        /// Additive (th-e4aef9): with a `req` the host answers `ok{req}` or
        /// `error{req, unknown_key}`; without one an unknown key answers a
        /// bare `error`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        req: Option<u64>,
    },
    Resize {
        req: u64,
        cols: u16,
        rows: u16,
    },
    Snapshot {
        req: u64,
        max_bytes: usize,
    },
    Screen {
        req: u64,
    },
    Kill {
        req: u64,
        #[serde(default)]
        signal: KillSignal,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grace_ms: Option<u64>,
    },
    Release,
    Ping {
        req: u64,
    },
}

/// The `type` names of [`ClientMsg`], for [`classify`].
pub const CLIENT_TYPES: &[&str] = &["hello", "input", "paste", "key", "resize", "snapshot", "screen", "kill", "release", "ping"];

/// host → daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMsg {
    Hello {
        protocol: u32,
        host_version: String,
        id: String,
        pid: u32,
        child_pid: u32,
        cols: u16,
        rows: u16,
        /// The last output `seq` the host produced (or `seq_start` before
        /// the first); every later `output` has a greater one.
        seq: u64,
        running: bool,
        exit: Option<ExitInfo>,
    },
    /// Body: PTY bytes, already fed to the VT.
    Output {
        seq: u64,
    },
    Overrun {
        through_seq: u64,
    },
    Exit {
        code: Option<i32>,
        signal: Option<i32>,
        seq: u64,
    },
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        req: Option<u64>,
        code: String,
        message: String,
    },
    Resized {
        req: u64,
        seq: u64,
        changed: bool,
    },
    /// Body: the VT snapshot.
    Snapshot {
        req: u64,
        seq: u64,
        cols: u16,
        rows: u16,
        alternate: bool,
        /// `full` | `history` | `plain` | `empty`.
        fidelity: String,
    },
    /// Body: the visible screen as plain text.
    Screen {
        req: u64,
        seq: u64,
        cols: u16,
        rows: u16,
        alternate_on: bool,
        cursor_x: u16,
        cursor_y: u16,
        title: Option<String>,
        modes: Modes,
    },
    Ok {
        req: u64,
    },
    Pong {
        req: u64,
    },
}

/// The `type` names of [`HostMsg`], for [`classify`].
pub const HOST_TYPES: &[&str] = &["hello", "output", "overrun", "exit", "error", "resized", "snapshot", "screen", "ok", "pong"];

impl HostMsg {
    /// An `error` answer.
    #[must_use]
    pub fn error(req: Option<u64>, code: &str, message: impl Into<String>) -> Self {
        Self::Error {
            req,
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// The one JSON object a daemon writes to `flow-host`'s stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// 64 lowercase hex. On stdin, never in argv or the environment.
    pub token: String,
    /// The host dir (`record::hosts_dir()`); must be absolute.
    pub dir: std::path::PathBuf,
    pub argv: Vec<String>,
    /// Must be absolute.
    pub cwd: std::path::PathBuf,
    /// The child's COMPLETE environment.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
    /// The `seq` the host reports before its first output; that output is
    /// `seq_start + 1`. A relaunch passes the previous host's final `seq` + 1.
    #[serde(default)]
    pub seq_start: u64,
    #[serde(default = "default_scrollback")]
    pub scrollback_rows: usize,
    #[serde(default = "default_linger")]
    pub linger_secs: u64,
    pub owner: String,
    /// The output queue bound before `overrun` (default 8 MiB). For tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_queue_bytes: Option<usize>,
}

const fn default_scrollback() -> usize {
    10_000
}

const fn default_linger() -> u64 {
    86_400
}

/// The line `flow-host` prints on stdout once it is serving (or failed to).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ready {
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket: Option<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn spawn_request_defaults_and_ready_lines() {
        let r: SpawnRequest = serde_json::from_str(r#"{"token":"t","dir":"/d","argv":["sh"],"cwd":"/","cols":80,"rows":24,"owner":"o"}"#).unwrap();
        assert_eq!((r.seq_start, r.scrollback_rows, r.linger_secs, r.max_queue_bytes), (0, 10_000, 86_400, None));
        assert!(r.env.is_empty());
        let ok: Ready = serde_json::from_str(r#"{"ready":true,"socket":"/s","pid":5}"#).unwrap();
        assert_eq!(ok.pid, Some(5));
        let bad: Ready = serde_json::from_str(r#"{"ready":false,"error":"no"}"#).unwrap();
        assert_eq!(bad.error.as_deref(), Some("no"));
    }

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(msg: &T, body: &[u8], known: &[&str]) {
        let bytes = encode(msg, body).unwrap();
        let frame = read_frame(&mut Cursor::new(bytes)).unwrap().unwrap();
        assert_eq!(frame.body, body);
        match classify::<T>(&frame, known) {
            Classified::Known(m) => assert_eq!(&m, msg),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_client_message_round_trips() {
        let msgs = [
            ClientMsg::Hello {
                protocol: (1, 1),
                token: "ab".repeat(32),
                client: "smooth-daemon/0.52.0".into(),
            },
            ClientMsg::Input,
            ClientMsg::Paste,
            ClientMsg::Key {
                name: "C-c".into(),
                repeat: Some(3),
                req: Some(9),
            },
            ClientMsg::Key {
                name: "Enter".into(),
                repeat: None,
                req: None,
            },
            ClientMsg::Resize { req: 1, cols: 120, rows: 40 },
            ClientMsg::Snapshot { req: 2, max_bytes: 1 << 20 },
            ClientMsg::Screen { req: 3 },
            ClientMsg::Kill {
                req: 4,
                signal: KillSignal::Kill,
                grace_ms: Some(10),
            },
            ClientMsg::Release,
            ClientMsg::Ping { req: 5 },
        ];
        let kinds: std::collections::BTreeSet<_> = msgs
            .iter()
            .map(|m| serde_json::to_value(m).unwrap()["type"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds.len(), CLIENT_TYPES.len(), "every type is covered");
        for m in &msgs {
            round_trip(m, b"\x1b[A body \x00\xff", CLIENT_TYPES);
        }
    }

    #[test]
    fn every_host_message_round_trips() {
        let exit = ExitInfo {
            code: Some(7),
            signal: None,
            at: "2026-10-03T17:40:12Z".into(),
        };
        let msgs = [
            HostMsg::Hello {
                protocol: 1,
                host_version: "0.52.0".into(),
                id: "fs-1a2b3c4d".into(),
                pid: 1,
                child_pid: 2,
                cols: 120,
                rows: 40,
                seq: 812,
                running: false,
                exit: Some(exit),
            },
            HostMsg::Output { seq: 813 },
            HostMsg::Overrun { through_seq: 900 },
            HostMsg::Exit {
                code: None,
                signal: Some(15),
                seq: 901,
            },
            HostMsg::error(Some(3), code::UNKNOWN_KEY, "no such key"),
            HostMsg::error(None, code::AUTH, "bad token"),
            HostMsg::Resized { req: 1, seq: 2, changed: true },
            HostMsg::Snapshot {
                req: 2,
                seq: 3,
                cols: 80,
                rows: 24,
                alternate: true,
                fidelity: "history".into(),
            },
            HostMsg::Screen {
                req: 3,
                seq: 4,
                cols: 80,
                rows: 24,
                alternate_on: false,
                cursor_x: 1,
                cursor_y: 2,
                title: Some("t".into()),
                modes: Modes {
                    mouse_tracking: Some("any".into()),
                    mouse_format: "sgr".into(),
                    kitty_keyboard: 1,
                    ..Modes::default()
                },
            },
            HostMsg::Ok { req: 5 },
            HostMsg::Pong { req: 6 },
        ];
        for m in &msgs {
            round_trip(m, b"", HOST_TYPES);
            round_trip(m, &vec![0u8; 70_000], HOST_TYPES);
        }
    }

    /// The spec's literal hello parses, and unknown fields are ignored.
    #[test]
    fn spec_hello_parses_and_extra_fields_are_ignored() {
        let h = serde_json::json!({"type":"hello","protocol":[1,1],"token":"t","client":"smooth-daemon/0.52.0","future":{"x":1}});
        let f = Frame { header: h, body: vec![] };
        match classify::<ClientMsg>(&f, CLIENT_TYPES) {
            Classified::Known(ClientMsg::Hello { protocol, token, .. }) => {
                assert_eq!(protocol, (1, 1));
                assert_eq!(token, "t");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_type_and_bad_fields_are_told_apart() {
        let f = Frame {
            header: serde_json::json!({"type":"teleport"}),
            body: vec![],
        };
        assert!(matches!(classify::<ClientMsg>(&f, CLIENT_TYPES), Classified::Unknown(t) if t == "teleport"));
        let f = Frame {
            header: serde_json::json!({"type":"resize","req":1,"cols":"wide"}),
            body: vec![],
        };
        assert!(matches!(classify::<ClientMsg>(&f, CLIENT_TYPES), Classified::Bad(_)));
        // A negative req is a bad request, not a panic.
        let f = Frame {
            header: serde_json::json!({"type":"ping","req":-1}),
            body: vec![],
        };
        assert!(matches!(classify::<ClientMsg>(&f, CLIENT_TYPES), Classified::Bad(_)));
    }

    #[test]
    fn a_body_split_across_reads_reassembles() {
        /// Hands out one byte per read.
        struct Trickle(Cursor<Vec<u8>>);
        impl Read for Trickle {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = buf.len().min(1);
                self.0.read(&mut buf[..n])
            }
        }
        let mut bytes = encode(&HostMsg::Output { seq: 1 }, b"hello world").unwrap();
        bytes.extend(encode(&HostMsg::Pong { req: 2 }, b"").unwrap());
        let mut r = Trickle(Cursor::new(bytes));
        let a = read_frame(&mut r).unwrap().unwrap();
        assert_eq!(a.body, b"hello world");
        let b = read_frame(&mut r).unwrap().unwrap();
        assert_eq!(b.kind(), Some("pong"));
        assert_eq!(b.req(), Some(2));
        assert!(read_frame(&mut r).unwrap().is_none(), "clean EOF at a boundary");
    }

    #[test]
    fn oversized_lengths_are_rejected_before_allocating() {
        let mut huge = Vec::new();
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        let e = read_frame(&mut Cursor::new(huge)).unwrap_err();
        assert!(matches!(e, FrameError::TooLarge(_)), "{e}");
        assert_eq!(e.code(), code::FRAME_TOO_LARGE);

        let over = u32::try_from(MAX_FRAME + 1).unwrap();
        let e = read_frame(&mut Cursor::new(over.to_be_bytes().to_vec())).unwrap_err();
        assert!(matches!(e, FrameError::TooLarge(_)));

        // A header length over 64 KiB, inside an otherwise legal frame.
        let mut b = 1_000_000u32.to_be_bytes().to_vec();
        b.extend_from_slice(&u32::try_from(MAX_HEADER + 1).unwrap().to_be_bytes());
        assert!(matches!(read_frame(&mut Cursor::new(b)).unwrap_err(), FrameError::TooLarge(_)));

        // A header length bigger than the frame it is in.
        let mut b = 10u32.to_be_bytes().to_vec();
        b.extend_from_slice(&100u32.to_be_bytes());
        assert!(matches!(read_frame(&mut Cursor::new(b)).unwrap_err(), FrameError::TooLarge(_)));

        // A frame too short to hold the header length.
        let b = 2u32.to_be_bytes().to_vec();
        assert!(matches!(read_frame(&mut Cursor::new(b)).unwrap_err(), FrameError::BadHeader(_)));
    }

    #[test]
    fn encoding_refuses_frames_over_the_limits() {
        let body = vec![0u8; MAX_FRAME];
        assert!(encode(&HostMsg::Output { seq: 1 }, &body).is_err());
        let big = "x".repeat(MAX_HEADER);
        assert!(encode(&HostMsg::error(None, code::BAD_REQUEST, big), b"").is_err());
        // The largest snapshot body we ever send fits.
        let body = vec![0u8; MAX_BODY - 1024];
        assert!(encode(
            &HostMsg::Snapshot {
                req: 1,
                seq: 1,
                cols: 1,
                rows: 1,
                alternate: false,
                fidelity: "full".into()
            },
            &body
        )
        .is_ok());
    }

    #[test]
    fn bad_json_headers_are_rejected() {
        for header in [&b"{not json"[..], b"[1,2]", b"{\"type\":7}", b"{}", b"\"hello\""] {
            let mut b = u32::try_from(4 + header.len()).unwrap().to_be_bytes().to_vec();
            b.extend_from_slice(&u32::try_from(header.len()).unwrap().to_be_bytes());
            b.extend_from_slice(header);
            let e = read_frame(&mut Cursor::new(b)).unwrap_err();
            assert!(matches!(e, FrameError::BadHeader(_)), "{header:?}: {e}");
            assert_eq!(e.code(), code::BAD_REQUEST);
        }
    }

    #[test]
    fn truncated_frames_are_io_errors() {
        let full = encode(&HostMsg::Output { seq: 1 }, b"0123456789").unwrap();
        for cut in [1, 3, 5, 9, full.len() - 1] {
            let e = read_frame(&mut Cursor::new(full[..cut].to_vec())).unwrap_err();
            assert!(matches!(e, FrameError::Io(_)), "cut at {cut}: {e}");
        }
    }

    #[test]
    fn version_negotiation_picks_the_highest_overlap() {
        assert_eq!(negotiate((1, 1), (1, 1)), Some(1));
        assert_eq!(negotiate((1, 3), (2, 5)), Some(3));
        assert_eq!(negotiate((2, 5), (1, 3)), Some(3));
        assert_eq!(negotiate((4, 4), (1, 3)), None, "an old host");
        assert_eq!(negotiate((1, 1), (2, 2)), None, "a new host");
        assert_eq!(negotiate((3, 1), (1, 3)), None, "an inverted offer overlaps nothing");
    }

    #[test]
    fn constant_time_eq_compares() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}

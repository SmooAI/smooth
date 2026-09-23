//! The local daemon's side of driving ANOTHER computer's Big Smooth over the
//! Smoo Relay (pearl th-a49e21).
//!
//! A Big Smooth window picks "smoo-hub" in its computer switcher; from then on
//! its operator WebSocket and its handful of REST calls go to
//! `/api/relay/peers/<device>/…` on its OWN daemon ([`crate::relay_peers_route`]),
//! and this module carries them to the remote daemon:
//!
//! - **One relay socket per window WebSocket** ([`Dialer::open`]). It dials the
//!   relay with this daemon's Smoo session as a separate device
//!   (`<this daemon's id>-w<slot>`, kind `phone`: to the remote daemon it is a
//!   client exactly like a phone), so the remote bridges it to its operator
//!   like any phone. A socket of its own — never the daemon's main relay
//!   socket — means every frame that comes back on it is a reply for this
//!   window, and a frame arriving on the main socket is always a request:
//!   two daemons driving each other can't be confused into a loop.
//! - **One shared relay socket per remote computer for REST** ([`HttpLinks`]),
//!   multiplexing `"channel":"http"` request/response frames by id, closed once
//!   idle.
//!
//! Slots are reused lowest-first, so a window that reconnects comes back as the
//! same device id and the remote daemon reuses its bridge rather than growing
//! one per reconnect.
//!
//! The Supabase session never leaves this daemon (it signs the relay dial), and
//! the window's local token never reaches the relay at all.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::relay::{classify_relay_msg, connect_url_as, fresh_access_token, wrap_out, RelayMsg, TokenOutcome};
use crate::relay_http::{parse_response, request_frame, Response};

/// Something that yields the Smoo access token to dial the relay with, or
/// `None` when signed out. Injectable so tests need no credentials file.
pub type TokenSource = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// The daemon's stored Smoo session — the same one its main relay link uses,
/// refreshed under the shared credential lock when it is near expiry.
#[must_use]
pub fn session_token_source() -> TokenSource {
    let http = reqwest::Client::default();
    Arc::new(move || {
        let http = http.clone();
        Box::pin(async move {
            match fresh_access_token(&http, false).await {
                TokenOutcome::Dial { token, .. } => Some(token),
                TokenOutcome::NoSession(_) => None,
            }
        })
    })
}

/// The relay `kind` a tunnel announces. The relay knows `daemon`, `flow`, and
/// everything else as `phone` — a tunnel is a client, so it says so, and no
/// picker ever offers it as a computer to drive.
const TUNNEL_KIND: &str = "phone";
/// How long a tunnel waits for the relay to open + authenticate it.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// A REST call's total budget across the relay and the remote daemon.
const HTTP_TIMEOUT: Duration = Duration::from_secs(25);
/// An idle REST link is closed after this long.
const HTTP_IDLE: Duration = Duration::from_secs(90);
/// Room for the `-w<slot>` suffix (up to `-w` + 10 digits) inside the relay's
/// 64-char device-id cap.
const BASE_DEVICE_MAX: usize = 52;

/// Why a tunnel could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelError {
    /// No Smoo session to dial the relay with.
    SignedOut,
    /// The relay could not be reached or refused the socket.
    Unreachable(String),
    /// The socket opened but the relay never authenticated it.
    NotAuthenticated,
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignedOut => f.write_str("this computer is not signed in to Smoo, so it cannot reach your other computers"),
            Self::Unreachable(why) => write!(f, "the Smoo Relay could not be reached: {why}"),
            Self::NotAuthenticated => f.write_str("the Smoo Relay did not accept this computer's session"),
        }
    }
}

/// What a tunnel hands back to its consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelEvent {
    /// A frame from the remote daemon, as wire text.
    Frame(String),
    /// The tunnel is over, and why.
    Ended(String),
}

/// One live tunnel to a remote daemon: send frames with [`Tunnel::send`],
/// read replies from `events`. Dropping it closes the relay socket.
pub struct Tunnel {
    to_remote: mpsc::UnboundedSender<String>,
    pub events: mpsc::UnboundedReceiver<TunnelEvent>,
    /// The relay device id this tunnel dialled as.
    pub device: String,
}

impl Tunnel {
    /// Queue one frame (JSON text) for the remote daemon. `false` once the
    /// tunnel is gone. Non-JSON is dropped by the envelope wrapper.
    pub fn send(&self, frame: String) -> bool {
        self.to_remote.send(frame).is_ok()
    }
}

/// Lowest-free slot numbers for tunnel device ids.
#[derive(Clone, Default)]
struct Slots(Arc<std::sync::Mutex<BTreeSet<u32>>>);

/// A claimed slot; released on drop.
struct SlotGuard {
    slots: Slots,
    slot: u32,
}

impl Slots {
    fn acquire(&self) -> SlotGuard {
        let mut taken = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = (0..).find(|n| !taken.contains(n)).unwrap_or(u32::MAX);
        taken.insert(slot);
        SlotGuard { slots: self.clone(), slot }
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.slots.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.slot);
    }
}

/// The relay-safe stem for tunnel device ids: this daemon's own id, reduced to
/// the relay grammar and shortened so `-w<slot>` still fits in 64 chars.
fn base_device(device: &str) -> String {
    let safe: String = device
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(BASE_DEVICE_MAX)
        .collect();
    if safe.is_empty() {
        "daemon".to_string()
    } else {
        safe
    }
}

/// Opens tunnels. Cheap to clone.
#[derive(Clone)]
pub struct Dialer {
    relay_url: Arc<String>,
    base: Arc<String>,
    label: Arc<String>,
    token: TokenSource,
    slots: Slots,
    silence_timeout: Duration,
}

impl Dialer {
    /// `device` / `label` are THIS daemon's relay identity; tunnels derive
    /// theirs from it.
    #[must_use]
    pub fn new(relay_url: impl Into<String>, device: &str, label: &str, token: TokenSource) -> Self {
        Self {
            relay_url: Arc::new(relay_url.into()),
            base: Arc::new(base_device(device)),
            label: Arc::new(format!("{label} (window)")),
            token,
            slots: Slots::default(),
            silence_timeout: crate::relay::RELAY_SILENCE_TIMEOUT,
        }
    }

    #[cfg(test)]
    const fn with_silence_timeout(mut self, t: Duration) -> Self {
        self.silence_timeout = t;
        self
    }

    /// The device id slot `slot` dials as.
    fn device_for(&self, slot: u32) -> String {
        format!("{}-w{slot}", self.base)
    }

    /// Open a tunnel to `target` (a relay device id the caller has already
    /// checked is one of this user's online daemons).
    ///
    /// # Errors
    /// [`TunnelError`] when there is no session, the relay is unreachable, or
    /// it does not authenticate the socket in time.
    pub async fn open(&self, target: &str) -> Result<Tunnel, TunnelError> {
        let token = (self.token)().await.ok_or(TunnelError::SignedOut)?;
        let slot = self.slots.acquire();
        let device = self.device_for(slot.slot);
        let url = connect_url_as(&self.relay_url, &token, &device, &self.label, TUNNEL_KIND);
        let (stream, _) = match tokio::time::timeout(DIAL_TIMEOUT, tokio_tungstenite::connect_async(&url)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(TunnelError::Unreachable(e.to_string())),
            Err(_) => return Err(TunnelError::Unreachable("timed out".into())),
        };
        let (mut sink, mut source) = stream.split();
        // The upgrade is not authentication: wait for the relay's ack before
        // calling the tunnel open (answering its pings meanwhile).
        let acked = tokio::time::timeout(DIAL_TIMEOUT, async {
            while let Some(Ok(msg)) = source.next().await {
                let Message::Text(text) = msg else { continue };
                match classify_relay_msg(&text) {
                    RelayMsg::Connected => return true,
                    RelayMsg::Ping => {
                        let _ = sink.send(Message::Text(r#"{"type":"pong"}"#.into())).await;
                    }
                    _ => {}
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        if !acked {
            let _ = sink.send(Message::Close(None)).await;
            return Err(TunnelError::NotAuthenticated);
        }
        let (to_remote, mut outbound) = mpsc::unbounded_channel::<String>();
        let (events_tx, events) = mpsc::unbounded_channel::<TunnelEvent>();
        let target = target.to_string();
        let silence_timeout = self.silence_timeout;
        let tunnel_device = device.clone();
        tokio::spawn(async move {
            let _slot = slot; // held for the life of the socket
            let silence = tokio::time::sleep(silence_timeout);
            tokio::pin!(silence);
            let why = loop {
                tokio::select! {
                    frame = outbound.recv() => {
                        let Some(frame) = frame else { break String::new() };
                        let Some(envelope) = wrap_out(&target, &frame) else { continue };
                        if sink.send(Message::Text(envelope.into())).await.is_err() {
                            break "the relay connection dropped".to_string();
                        }
                    }
                    () = &mut silence => break "the relay went silent".to_string(),
                    msg = source.next() => {
                        if matches!(msg, Some(Ok(_))) {
                            silence.as_mut().reset(tokio::time::Instant::now() + silence_timeout);
                        }
                        let text = match msg {
                            Some(Ok(Message::Text(t))) => t,
                            Some(Ok(Message::Close(_))) | None => break "the relay closed the connection".to_string(),
                            Some(Ok(_)) => continue,
                            Some(Err(e)) => break format!("the relay connection failed: {e}"),
                        };
                        let frame = match classify_relay_msg(&text) {
                            RelayMsg::Ping => {
                                if sink.send(Message::Text(r#"{"type":"pong"}"#.into())).await.is_err() {
                                    break "the relay connection dropped".to_string();
                                }
                                continue;
                            }
                            RelayMsg::PeerOffline(d) if d == target => break "that computer went offline".to_string(),
                            // Only the computer this tunnel drives may talk on it.
                            RelayMsg::Frame(from, f) | RelayMsg::FlowFrame(from, f) if from == target => f,
                            RelayMsg::HttpFrame(from, f) if from == target => f.to_string(),
                            _ => continue,
                        };
                        if events_tx.send(TunnelEvent::Frame(frame)).is_err() {
                            break String::new(); // the consumer is gone
                        }
                    }
                }
            };
            let _ = sink.send(Message::Close(None)).await;
            if !why.is_empty() {
                tracing::debug!(device = %tunnel_device, %target, %why, "relay tunnel ended");
                let _ = events_tx.send(TunnelEvent::Ended(why));
            }
        });
        Ok(Tunnel { to_remote, events, device })
    }
}

/// Why a REST call over the relay produced no response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// The tunnel could not be opened.
    Tunnel(TunnelError),
    /// No answer in time — typically a remote daemon too old to speak HTTP
    /// over the relay, or one that went away mid-call.
    Timeout,
    /// The link dropped with the call in flight.
    Dropped,
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tunnel(e) => e.fmt(f),
            Self::Timeout => f.write_str("that computer did not answer over the relay in time — its Big Smooth may need an update"),
            Self::Dropped => f.write_str("the relay connection to that computer dropped"),
        }
    }
}

type Pending = Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<Response>>>>;

/// One shared REST tunnel to one remote computer.
struct HttpLink {
    to_remote: mpsc::UnboundedSender<String>,
    pending: Pending,
    last_used: Arc<std::sync::Mutex<Instant>>,
}

/// REST calls to remote computers, one multiplexed tunnel per computer.
#[derive(Clone)]
pub struct HttpLinks {
    dialer: Dialer,
    links: Arc<tokio::sync::Mutex<HashMap<String, Arc<HttpLink>>>>,
    idle: Duration,
    timeout: Duration,
}

impl HttpLinks {
    #[must_use]
    pub fn new(dialer: Dialer) -> Self {
        Self {
            dialer,
            links: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            idle: HTTP_IDLE,
            timeout: HTTP_TIMEOUT,
        }
    }

    #[cfg(test)]
    const fn with_timing(mut self, idle: Duration, timeout: Duration) -> Self {
        self.idle = idle;
        self.timeout = timeout;
        self
    }

    /// The live link to `target`, dialling one if there is none.
    async fn link(&self, target: &str) -> Result<Arc<HttpLink>, LinkError> {
        let mut links = self.links.lock().await;
        if let Some(link) = links.get(target).filter(|l| !l.to_remote.is_closed()) {
            return Ok(link.clone());
        }
        let tunnel = self.dialer.open(target).await.map_err(LinkError::Tunnel)?;
        let Tunnel { to_remote, mut events, .. } = tunnel;
        let link = Arc::new(HttpLink {
            to_remote,
            pending: Arc::new(std::sync::Mutex::new(HashMap::new())),
            last_used: Arc::new(std::sync::Mutex::new(Instant::now())),
        });
        links.insert(target.to_string(), link.clone());
        drop(links);

        // The reader: settle calls by id; close the link once it has sat idle.
        let reader_link = Arc::downgrade(&link);
        let all_links = self.links.clone();
        let target = target.to_string();
        let idle = self.idle;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(idle.min(Duration::from_secs(10)).max(Duration::from_millis(20)));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    ev = events.recv() => match ev {
                        Some(TunnelEvent::Frame(text)) => {
                            let Some(link) = reader_link.upgrade() else { break };
                            let Some((id, resp)) = serde_json::from_str::<Value>(&text).ok().as_ref().and_then(parse_response) else { continue };
                            let waiter = link.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);
                            if let Some(w) = waiter {
                                let _ = w.send(resp);
                            }
                        }
                        Some(TunnelEvent::Ended(_)) | None => break,
                    },
                    _ = tick.tick() => {
                        let Some(link) = reader_link.upgrade() else { break };
                        let quiet = link.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty();
                        let idle_for = link.last_used.lock().unwrap_or_else(std::sync::PoisonError::into_inner).elapsed();
                        if quiet && idle_for >= idle {
                            break;
                        }
                    }
                }
            }
            // Forget this link (if it is still the registered one) — dropping
            // the last handle closes its tunnel — and fail anything in flight.
            let mut links = all_links.lock().await;
            if links
                .get(&target)
                .is_some_and(|l| reader_link.upgrade().is_some_and(|mine| Arc::ptr_eq(l, &mine)))
            {
                links.remove(&target);
            }
            drop(links);
            if let Some(link) = reader_link.upgrade() {
                link.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
            }
        });
        Ok(link)
    }

    /// One REST call to `target`. `path` is the path + query, already
    /// allowlisted and stripped of the local token by the caller.
    ///
    /// # Errors
    /// [`LinkError`] when there is no tunnel or no answer.
    pub async fn request(&self, target: &str, method: &str, path: &str, body: Option<&str>) -> Result<Response, LinkError> {
        let link = self.link(target).await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let (tx, rx) = oneshot::channel();
        link.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id.clone(), tx);
        *link.last_used.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        if link.to_remote.send(request_frame(&id, method, path, body).to_string()).is_err() {
            link.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);
            return Err(LinkError::Dropped);
        }
        let outcome = tokio::time::timeout(self.timeout, rx).await;
        link.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);
        *link.last_used.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        match outcome {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => Err(LinkError::Dropped),
            Err(_) => Err(LinkError::Timeout),
        }
    }

    #[cfg(test)]
    async fn open_links(&self) -> usize {
        self.links.lock().await.values().filter(|l| !l.to_remote.is_closed()).count()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
pub(crate) mod tests {
    use super::*;

    use axum::extract::ws::{Message as AxMsg, WebSocket, WebSocketUpgrade};
    use axum::extract::{Query, State};
    use axum::routing::get;
    use axum::Router;
    use serde_json::json;

    /// A fixed-token source.
    pub fn token(t: &'static str) -> TokenSource {
        Arc::new(move || Box::pin(async move { Some(t.to_string()) }))
    }

    /// A signed-out source.
    pub fn no_token() -> TokenSource {
        Arc::new(|| Box::pin(async { None }))
    }

    /// An in-memory relay with the real relay's routing rules: every socket is
    /// `(user from ?token=, device from ?device=)`; `{to, frame}` goes to the
    /// SAME user's `to` device as `{from, frame}`, or comes back as
    /// `peer_offline`; `list_peers` answers with the user's other devices.
    #[derive(Clone, Default)]
    pub struct FakeRelay {
        #[allow(clippy::type_complexity, reason = "test fixture")]
        peers: Arc<std::sync::Mutex<HashMap<(String, String), (String, String, mpsc::UnboundedSender<String>)>>>,
        /// Every `?device=` that connected, in order.
        pub connects: Arc<std::sync::Mutex<Vec<String>>>,
        /// Refuse to ack (auth never completes).
        pub no_ack: bool,
    }

    impl FakeRelay {
        pub async fn serve(self) -> std::net::SocketAddr {
            let app = Router::new().route("/ws", get(Self::upgrade)).with_state(self);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            addr
        }

        async fn upgrade(State(relay): State<Self>, Query(q): Query<HashMap<String, String>>, u: WebSocketUpgrade) -> axum::response::Response {
            let user = q.get("token").cloned().unwrap_or_default();
            let device = q.get("device").cloned().unwrap_or_default();
            let label = q.get("label").cloned().unwrap_or_default();
            let kind = match q.get("kind").map(String::as_str) {
                Some("daemon") => "daemon",
                Some("flow") => "flow",
                _ => "phone",
            }
            .to_string();
            relay.connects.lock().unwrap().push(device.clone());
            u.on_upgrade(move |ws| relay.session(ws, user, device, label, kind))
        }

        async fn session(self, ws: WebSocket, user: String, device: String, label: String, kind: String) {
            let (mut sink, mut source) = ws.split();
            let (tx, mut rx) = mpsc::unbounded_channel::<String>();
            self.peers.lock().unwrap().insert((user.clone(), device.clone()), (label, kind, tx.clone()));
            if !self.no_ack {
                let _ = tx.send(r#"{"type":"connected"}"#.to_string());
            }
            let writer = tokio::spawn(async move {
                while let Some(t) = rx.recv().await {
                    if sink.send(AxMsg::Text(t.into())).await.is_err() {
                        break;
                    }
                }
            });
            while let Some(Ok(msg)) = source.next().await {
                let AxMsg::Text(text) = msg else {
                    if matches!(msg, AxMsg::Close(_)) {
                        break;
                    }
                    continue;
                };
                let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                if v.get("type").and_then(Value::as_str) == Some("list_peers") {
                    let peers: Vec<Value> = self
                        .peers
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|((u, d), _)| *u == user && *d != device)
                        .map(|((_, d), (l, k, _))| json!({"device": d, "label": l, "kind": k}))
                        .collect();
                    let _ = tx.send(json!({"type":"peers","peers":peers}).to_string());
                    continue;
                }
                let (Some(to), Some(frame)) = (v.get("to").and_then(Value::as_str), v.get("frame")) else {
                    continue;
                };
                let target = self.peers.lock().unwrap().get(&(user.clone(), to.to_string())).map(|(_, _, t)| t.clone());
                match target {
                    Some(t) => {
                        let _ = t.send(json!({"from": device, "frame": frame}).to_string());
                    }
                    None => {
                        let _ = tx.send(json!({"type":"peer_offline","to": to}).to_string());
                    }
                }
            }
            self.peers.lock().unwrap().remove(&(user, device));
            writer.abort();
        }

        /// Connect a raw peer (a fake remote daemon, another user's device…).
        pub async fn connect(addr: std::net::SocketAddr, user: &str, device: &str, kind: &str) -> FakePeer {
            let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={user}&device={device}&label={device}&kind={kind}"))
                .await
                .unwrap();
            let (sink, mut source) = ws.split();
            // Swallow the ack.
            let first = source.next().await.unwrap().unwrap();
            assert!(first.to_text().unwrap().contains("connected"));
            FakePeer { sink, source }
        }
    }

    type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    pub struct FakePeer {
        pub sink: futures_util::stream::SplitSink<WsStream, Message>,
        pub source: futures_util::stream::SplitStream<WsStream>,
    }

    impl FakePeer {
        pub async fn send(&mut self, v: Value) {
            self.sink.send(Message::Text(v.to_string().into())).await.unwrap();
        }
        pub async fn recv(&mut self) -> Value {
            let msg = tokio::time::timeout(Duration::from_secs(5), self.source.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            serde_json::from_str(msg.to_text().unwrap()).unwrap()
        }
    }

    fn dialer(addr: std::net::SocketAddr, user: &'static str) -> Dialer {
        Dialer::new(format!("ws://{addr}/ws"), "daemon-local", "marvin", token(user))
    }

    #[test]
    fn tunnel_device_ids_fit_the_relay_grammar() {
        let d = Dialer::new("ws://x", "daemon-abc", "marvin", no_token());
        assert_eq!(d.device_for(0), "daemon-abc-w0");
        let long = Dialer::new("ws://x", &"x".repeat(200), "m", no_token());
        assert!(crate::relay::valid_device(&long.device_for(u32::MAX)));
        let junk = Dialer::new("ws://x", "we ird:id/!", "m", no_token());
        assert_eq!(junk.device_for(3), "weirdid-w3");
        let empty = Dialer::new("ws://x", "::", "m", no_token());
        assert_eq!(empty.device_for(0), "daemon-w0");
    }

    #[test]
    fn slots_are_reused_lowest_first() {
        let slots = Slots::default();
        let a = slots.acquire();
        let b = slots.acquire();
        assert_eq!((a.slot, b.slot), (0, 1));
        drop(a);
        let c = slots.acquire();
        assert_eq!(c.slot, 0, "a reconnect comes back as the same device");
        let d = slots.acquire();
        assert_eq!(d.slot, 2);
    }

    #[tokio::test]
    async fn signed_out_dials_nothing() {
        let relay = FakeRelay::default();
        let connects = relay.connects.clone();
        let addr = relay.serve().await;
        let d = Dialer::new(format!("ws://{addr}/ws"), "daemon-local", "marvin", no_token());
        assert_eq!(d.open("daemon-remote").await.err(), Some(TunnelError::SignedOut));
        assert!(connects.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unreachable_or_unacking_relay_is_an_error_not_a_hang() {
        let d = Dialer::new("ws://127.0.0.1:1/ws", "daemon-local", "marvin", token("u1"));
        assert!(matches!(d.open("daemon-remote").await, Err(TunnelError::Unreachable(_))));

        let relay = FakeRelay {
            no_ack: true,
            ..FakeRelay::default()
        };
        let addr = relay.serve().await;
        let started = Instant::now();
        let err = dialer(addr, "u1").open("daemon-remote").await.err();
        assert_eq!(err, Some(TunnelError::NotAuthenticated));
        assert!(started.elapsed() < DIAL_TIMEOUT + Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_tunnel_carries_frames_to_its_target_and_only_its_targets_replies_back() {
        let relay = FakeRelay::default();
        let connects = relay.connects.clone();
        let addr = relay.serve().await;
        let mut remote = FakeRelay::connect(addr, "u1", "daemon-remote", "daemon").await;
        let mut other = FakeRelay::connect(addr, "u1", "daemon-other", "daemon").await;

        let mut tunnel = dialer(addr, "u1").open("daemon-remote").await.unwrap();
        assert_eq!(tunnel.device, "daemon-local-w0");
        assert_eq!(connects.lock().unwrap().last().map(String::as_str), Some("daemon-local-w0"));

        assert!(tunnel.send(r#"{"action":"list_conversations","requestId":"lc-1"}"#.to_string()));
        assert!(tunnel.send("not json".to_string()), "noise is dropped, the tunnel stays up");
        let got = remote.recv().await;
        assert_eq!(got["from"], "daemon-local-w0");
        assert_eq!(got["frame"]["action"], "list_conversations");

        // A stranger on the same account writing to the tunnel is ignored…
        other.send(json!({"to":"daemon-local-w0","frame":{"type":"spoof"}})).await;
        // …the target's reply is delivered.
        remote.send(json!({"to":"daemon-local-w0","frame":{"type":"conversations","items":[]}})).await;
        let ev = tokio::time::timeout(Duration::from_secs(5), tunnel.events.recv()).await.unwrap().unwrap();
        let TunnelEvent::Frame(text) = ev else {
            panic!("expected a frame, got {ev:?}")
        };
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["type"], "conversations");
    }

    #[tokio::test]
    async fn another_users_device_is_unreachable() {
        let relay = FakeRelay::default();
        let addr = relay.serve().await;
        // Same device id, different Smoo user.
        let mut stranger = FakeRelay::connect(addr, "u2", "daemon-remote", "daemon").await;
        let mut tunnel = dialer(addr, "u1").open("daemon-remote").await.unwrap();
        tunnel.send(r#"{"action":"send_message","message":"hi"}"#.to_string());
        let ev = tokio::time::timeout(Duration::from_secs(5), tunnel.events.recv()).await.unwrap().unwrap();
        assert_eq!(ev, TunnelEvent::Ended("that computer went offline".into()));
        assert!(
            tokio::time::timeout(Duration::from_millis(300), stranger.source.next()).await.is_err(),
            "nothing reaches another user's device"
        );
    }

    #[tokio::test]
    async fn a_silent_relay_ends_the_tunnel() {
        let relay = FakeRelay::default();
        let addr = relay.serve().await;
        let _remote = FakeRelay::connect(addr, "u1", "daemon-remote", "daemon").await;
        let mut tunnel = dialer(addr, "u1")
            .with_silence_timeout(Duration::from_millis(200))
            .open("daemon-remote")
            .await
            .unwrap();
        let ev = tokio::time::timeout(Duration::from_secs(5), tunnel.events.recv()).await.unwrap().unwrap();
        assert_eq!(ev, TunnelEvent::Ended("the relay went silent".into()));
    }

    /// A remote daemon that answers `http.request` frames the way the real one
    /// does, from a canned table.
    async fn fake_remote_http(mut remote: FakePeer) {
        loop {
            let got = remote.recv().await;
            let from = got["from"].as_str().unwrap().to_string();
            let req = crate::relay_http::parse_request(&got["frame"]).unwrap();
            let reply = crate::relay_http::response_frame(Some(&req.id), 200, "application/json", &json!({"path": req.path, "body": req.body}).to_string());
            remote.send(json!({"to": from, "frame": reply})).await;
        }
    }

    #[tokio::test]
    async fn rest_calls_multiplex_over_one_link_and_close_when_idle() {
        let relay = FakeRelay::default();
        let connects = relay.connects.clone();
        let addr = relay.serve().await;
        let remote = FakeRelay::connect(addr, "u1", "daemon-remote", "daemon").await;
        tokio::spawn(fake_remote_http(remote));

        let links = HttpLinks::new(dialer(addr, "u1")).with_timing(Duration::from_millis(300), Duration::from_secs(5));
        let (a, b) = tokio::join!(
            links.request("daemon-remote", "GET", "/api/stats", None),
            links.request("daemon-remote", "POST", "/api/session/cwd", Some(r#"{"path":"/x"}"#)),
        );
        let a: Value = serde_json::from_str(&a.unwrap().body).unwrap();
        let b: Value = serde_json::from_str(&b.unwrap().body).unwrap();
        assert_eq!(a["path"], "/api/stats");
        assert_eq!(b["path"], "/api/session/cwd");
        assert_eq!(b["body"], r#"{"path":"/x"}"#);
        let dials = connects.lock().unwrap().iter().filter(|d| d.starts_with("daemon-local")).count();
        assert_eq!(dials, 1, "both calls shared one tunnel");
        assert_eq!(links.open_links().await, 1);

        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(links.open_links().await, 0, "an idle link is closed");
        // …and the next call simply dials again.
        let again = links.request("daemon-remote", "GET", "/api/stats", None).await.unwrap();
        assert_eq!(again.status, 200);
    }

    #[tokio::test]
    async fn an_old_remote_that_never_answers_times_out_cleanly() {
        let relay = FakeRelay::default();
        let addr = relay.serve().await;
        // Swallows everything, answers nothing (what a pre-th-a49e21 daemon's
        // operator bridge amounts to for an http frame).
        let _remote = FakeRelay::connect(addr, "u1", "daemon-remote", "daemon").await;
        let links = HttpLinks::new(dialer(addr, "u1")).with_timing(Duration::from_secs(60), Duration::from_millis(300));
        let err = links.request("daemon-remote", "GET", "/api/stats", None).await.unwrap_err();
        assert_eq!(err, LinkError::Timeout);
        assert!(err.to_string().contains("update"));
    }

    #[tokio::test]
    async fn a_remote_that_leaves_fails_the_call_instead_of_hanging() {
        let relay = FakeRelay::default();
        let addr = relay.serve().await;
        let links = HttpLinks::new(dialer(addr, "u1")).with_timing(Duration::from_secs(60), Duration::from_secs(5));
        let started = Instant::now();
        let err = links.request("daemon-gone", "GET", "/api/stats", None).await.unwrap_err();
        assert_eq!(err, LinkError::Dropped);
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}

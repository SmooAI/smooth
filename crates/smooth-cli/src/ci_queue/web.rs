//! `th ci-queue web` — the check queue as a live web page (SMOODEV-3371).
//!
//! One command that always works. When Big Smooth is running and serves the
//! Queue tab, this opens that (`http://<daemon>/#queue`). Otherwise — or with
//! `--serve` — it serves the same page itself (`queue.html`, embedded in `th`
//! through smooth-web) and streams the queue to it as Server-Sent Events. Put
//! it on a screen.
//!
//! - `GET /api/events`: an SSE stream of `snapshot` events, one a second.
//!   The first carries the whole ten-minute sample window; later ones carry
//!   only the sample taken since, if any. The payload is the same
//!   `{snapshot, samples, error?}` smooth-daemon's `/api/ci-queue/status`
//!   returns (`smooth_web::queue`).
//! - `GET /api/status` and `GET /api/ci-queue/status` (the daemon's route, same
//!   contract, `?since_ms=`): the same payload once, for polling and curl.
//!
//! One sampler serves every viewer, and it only reads the queue while
//! someone is connected: an open tab costs one `snapshot` a second, however
//! many tabs there are, and a closed one costs nothing.
//!
//! Binds loopback by default. The page shows worktree paths and job labels,
//! so `--host 0.0.0.0` is an explicit choice.

use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::{Json, Router};
use clap::Args;
use futures_util::Stream;
use smooth_web::queue::{Samples, StatusResponse};
use tokio::sync::broadcast;

use super::queue::{Queue, SNAPSHOT_SCHEMA};

#[derive(Args, Debug)]
pub struct WebArgs {
    /// Port to listen on. If it is taken, any free port is used instead.
    #[arg(long, default_value_t = 4380)]
    pub port: u16,

    /// Address to bind. The page shows worktree paths and job labels, so this
    /// stays loopback unless you mean to share it (e.g. `0.0.0.0` for a TV).
    #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    pub host: IpAddr,

    /// Open the page in the default browser.
    #[arg(long)]
    pub open: bool,

    /// Open the replay of a busy night (35 agents, load ~1,000) instead of
    /// the live queue. Nice when the machine is idle and you want to show it.
    #[arg(long)]
    pub demo: bool,

    /// Serve the page from this `th` even when Big Smooth is running.
    #[arg(long)]
    pub serve: bool,
}

/// How often the sampler reads the queue while someone is watching.
const EVERY: Duration = Duration::from_secs(1);

/// Finished jobs per snapshot: enough for per-label p50s and cost profiles.
const HISTORY: usize = 200;

/// What the sampler reads. A trait so the tests feed it without a real queue.
pub trait Source: Send + Sync + 'static {
    fn read(&self) -> Result<serde_json::Value, String>;
}

/// The machine's queue.
pub struct QueueSource {
    queue: Queue,
    cwd: PathBuf,
}

impl Source for QueueSource {
    fn read(&self) -> Result<serde_json::Value, String> {
        if !cfg!(unix) {
            return Ok(serde_json::json!({
                "schema": SNAPSHOT_SCHEMA,
                "unsupported": "the queue is Unix-only (it relies on flock and process groups); on this OS jobs run directly and nothing is queued",
            }));
        }
        let snap = self
            .queue
            .snapshot(HISTORY, &self.cwd)
            .map_err(|e| format!("could not read the queue: {e:#}"))?;
        serde_json::to_value(&snap).map_err(|e| format!("could not encode the snapshot: {e}"))
    }
}

#[derive(Default)]
struct Latest {
    snapshot: Option<serde_json::Value>,
    error: Option<String>,
    samples: Samples,
    read_at: Option<std::time::Instant>,
}

/// Shared between the sampler and every connection.
#[derive(Clone)]
pub struct Hub {
    latest: Arc<Mutex<Latest>>,
    tx: broadcast::Sender<Arc<String>>,
    source: Arc<dyn Source>,
}

impl Hub {
    pub fn new(source: Arc<dyn Source>) -> Self {
        let (tx, _) = broadcast::channel(16);
        Self {
            latest: Arc::default(),
            tx,
            source,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Latest> {
        self.latest.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Read the queue once and publish it. Returns the payload sent to
    /// connected viewers (only the new sample, if one was due).
    pub async fn tick(&self) -> Arc<String> {
        let source = Arc::clone(&self.source);
        let read = tokio::task::spawn_blocking(move || source.read())
            .await
            .unwrap_or_else(|e| Err(format!("the queue reader crashed: {e}")));
        let payload = {
            let mut l = self.lock();
            l.read_at = Some(std::time::Instant::now());
            let fresh = match read {
                Ok(snap) => {
                    let sample = l.samples.record(&snap);
                    l.snapshot = Some(snap);
                    l.error = None;
                    sample.into_iter().collect()
                }
                // Keep the last good snapshot on screen, and say why it is stale.
                Err(e) => {
                    l.error = Some(e);
                    Vec::new()
                }
            };
            Arc::new(encode(&StatusResponse {
                snapshot: l.snapshot.clone(),
                samples: fresh,
                error: l.error.clone(),
            }))
        };
        let _ = self.tx.send(Arc::clone(&payload));
        payload
    }

    /// Everything a new viewer needs: the latest snapshot and the whole window.
    fn full(&self) -> Option<String> {
        let l = self.lock();
        (l.snapshot.is_some() || l.error.is_some()).then(|| {
            encode(&StatusResponse {
                snapshot: l.snapshot.clone(),
                samples: l.samples.since(0),
                error: l.error.clone(),
            })
        })
    }

    /// Read the queue once a second, but only while someone is connected.
    pub async fn run_sampler(self) {
        let mut every = tokio::time::interval(EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            if self.tx.receiver_count() > 0 {
                self.tick().await;
            }
        }
    }
}

fn encode(r: &StatusResponse) -> String {
    serde_json::to_string(r).unwrap_or_else(|e| format!(r#"{{"snapshot":null,"samples":[],"error":"could not encode: {e}"}}"#))
}

/// `/api/*` in front of the page.
pub fn router(hub: Hub) -> Router {
    Router::new()
        .route("/api/events", get(events))
        .route("/api/status", get(status))
        .route("/api/ci-queue/status", get(status))
        .with_state(hub)
        .merge(smooth_web::queue::queue_router())
}

#[derive(Debug, serde::Deserialize)]
struct StatusQuery {
    #[serde(default)]
    since_ms: u64,
}

/// One read, for polling and curl. A snapshot younger than a sampler tick is
/// reused, so pollers share the sampler's reads like SSE viewers do.
async fn status(State(hub): State<Hub>, axum::extract::Query(q): axum::extract::Query<StatusQuery>) -> Json<StatusResponse> {
    let stale = hub.lock().read_at.is_none_or(|t| t.elapsed() >= EVERY);
    if stale {
        hub.tick().await;
    }
    let l = hub.lock();
    Json(StatusResponse {
        snapshot: l.snapshot.clone(),
        samples: l.samples.since(q.since_ms),
        error: l.error.clone(),
    })
}

async fn events(State(hub): State<Hub>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    if hub.full().is_none() {
        // First viewer: read now rather than making them wait a tick.
        hub.tick().await;
    }
    let opener = hub.full().unwrap_or_default();
    // A tick that lands between the opener and this subscribe is not lost
    // for long: every tick is a whole snapshot, and the next is a second away.
    let rx = hub.tx.subscribe();
    let stream = futures_util::stream::unfold((Some(opener), rx), |(first, mut rx)| async move {
        if let Some(body) = first {
            return Some((Ok(Event::default().event("snapshot").data(body)), (None, rx)));
        }
        loop {
            match rx.recv().await {
                Ok(body) => return Some((Ok(Event::default().event("snapshot").data(body.as_str())), (None, rx))),
                // A slow viewer skips ahead; the next snapshot is complete on its own.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Bind `host:port`, falling back to any free port when it is taken.
async fn bind(host: IpAddr, port: u16) -> Result<tokio::net::TcpListener> {
    match tokio::net::TcpListener::bind(SocketAddr::new(host, port)).await {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && port != 0 => tokio::net::TcpListener::bind(SocketAddr::new(host, 0))
            .await
            .with_context(|| format!("binding {host} (port {port} was taken)")),
        Err(e) => Err(e).with_context(|| format!("binding {host}:{port}")),
    }
}

/// The page's URL for a bound address.
#[must_use]
pub fn page_url(addr: SocketAddr, demo: bool) -> String {
    let host = if addr.ip().is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        addr.ip()
    };
    let base = format!("http://{}/", SocketAddr::new(host, addr.port()));
    if demo {
        format!("{base}?demo")
    } else {
        base
    }
}

/// The daemon Big Smooth advertises in `~/.smooth/daemon.addr`, if any.
fn advertised_daemon() -> Option<String> {
    let addr = std::fs::read_to_string(dirs_next::home_dir()?.join(".smooth").join("daemon.addr")).ok()?;
    Some(addr.trim().to_string()).filter(|a| !a.is_empty())
}

/// Does the daemon at `addr` serve the Queue tab with a live queue? A daemon
/// older than the tab (404), one whose `th` has no queue (no snapshot), or no
/// daemon at all (refused, or slower than a second) means "serve it here".
pub async fn daemon_serves_queue(addr: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_millis(1500)).build() else {
        return false;
    };
    let Ok(res) = client.get(format!("http://{addr}/api/ci-queue/status")).send().await else {
        return false;
    };
    if !res.status().is_success() {
        return false;
    }
    res.json::<serde_json::Value>()
        .await
        .is_ok_and(|v| v.get("snapshot").is_some_and(smooth_web::queue::is_snapshot))
}

/// Big Smooth's Queue tab on the daemon at `addr`.
#[must_use]
pub fn daemon_queue_url(addr: &str, demo: bool) -> String {
    format!("http://{addr}/{}#queue", if demo { "?demo" } else { "" })
}

fn open_in_browser(url: &str) {
    if let Err(e) = open::that(url) {
        eprintln!("  could not open a browser ({e}); open the URL above.");
    }
}

/// Open Big Smooth's Queue tab if it is up, else serve until Ctrl-C.
///
/// # Errors
/// When the address cannot be bound.
pub fn run(queue: Queue, a: &WebArgs) -> Result<i32> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let hub = Hub::new(Arc::new(QueueSource { queue, cwd }));
    let (host, port, open, demo, serve) = (a.host, a.port, a.open, a.demo, a.serve);
    // `th`'s main is already async; the server runs on that runtime.
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async move {
            // Sharing on the network is a serving decision, so --host serves too.
            if !serve && host.is_loopback() {
                if let Some(daemon) = advertised_daemon() {
                    if daemon_serves_queue(&daemon).await {
                        let url = daemon_queue_url(&daemon, demo);
                        eprintln!("th ci-queue web: Big Smooth is running — {url}");
                        eprintln!("  (`--serve` serves the page from this th instead)");
                        if open {
                            open_in_browser(&url);
                        }
                        return Ok(0);
                    }
                }
            }
            let listener = bind(host, port).await?;
            let addr = listener.local_addr()?;
            let url = page_url(addr, demo);
            eprintln!("th ci-queue web: {url}  (Ctrl-C to stop)");
            if !host.is_loopback() {
                eprintln!("  listening on {host}: anyone who can reach this machine can see the queue's paths and job labels.");
            }
            if open {
                open_in_browser(&url);
            }
            tokio::spawn(hub.clone().run_sampler());
            // Open SSE connections never end on their own, so a graceful
            // shutdown would wait forever: Ctrl-C just stops serving.
            tokio::select! {
                r = axum::serve(listener, router(hub)) => r.context("serving")?,
                _ = tokio::signal::ctrl_c() => eprintln!(),
            }
            Ok::<_, anyhow::Error>(0)
        })
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use futures_util::StreamExt as _;
    use tower::ServiceExt as _;

    use super::*;

    /// A fake queue whose clock advances 5s per read, so every read is due a sample.
    struct Fake {
        reads: AtomicU64,
        fail: bool,
    }

    impl Source for Fake {
        fn read(&self) -> Result<serde_json::Value, String> {
            let n = self.reads.fetch_add(1, Ordering::SeqCst);
            if self.fail && n > 0 {
                return Err("boom".into());
            }
            Ok(
                serde_json::json!({ "schema": 2, "now_ms": 1_000 + n * 5_000, "running": [], "waiting": [], "readings": { "cores": 12, "load1": 3.0 }, "budget": { "scale": 0.5 } }),
            )
        }
    }

    fn hub(fail: bool) -> (Hub, Arc<Fake>) {
        let fake = Arc::new(Fake {
            reads: AtomicU64::new(0),
            fail,
        });
        (Hub::new(fake.clone()), fake)
    }

    async fn get_json(app: Router, uri: &str) -> serde_json::Value {
        let res = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn status_reads_once_and_relays_the_snapshot_untouched() {
        let (hub, fake) = hub(false);
        let json = get_json(router(hub.clone()), "/api/status").await;
        assert_eq!(json["snapshot"]["schema"], 2, "unknown fields and newer schemas pass through");
        assert_eq!(json["snapshot"]["budget"]["scale"], 0.5);
        assert_eq!(json["samples"][0]["budget"]["scale"], 0.5, "samples carry the budget for the sawtooth");
        get_json(router(hub), "/api/status").await;
        assert_eq!(
            fake.reads.load(Ordering::SeqCst),
            1,
            "a second status with a snapshot in hand does not read again"
        );
    }

    #[tokio::test]
    async fn later_ticks_carry_only_the_new_sample_and_the_opener_carries_the_window() {
        let (hub, _) = hub(false);
        hub.tick().await;
        hub.tick().await;
        let third: serde_json::Value = serde_json::from_str(&hub.tick().await).unwrap();
        assert_eq!(third["samples"].as_array().unwrap().len(), 1);
        let full: serde_json::Value = serde_json::from_str(&hub.full().unwrap()).unwrap();
        assert_eq!(full["samples"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_failed_read_keeps_the_last_snapshot_and_says_why() {
        let (hub, _) = hub(true);
        hub.tick().await;
        let after: serde_json::Value = serde_json::from_str(&hub.tick().await).unwrap();
        assert_eq!(after["snapshot"]["now_ms"], 1_000);
        assert_eq!(after["error"], "boom");
    }

    #[tokio::test]
    async fn events_open_with_a_full_snapshot_then_stream_ticks() {
        let (hub, _) = hub(false);
        let res = router(hub.clone())
            .oneshot(Request::builder().uri("/api/events").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
        let mut body = res.into_body().into_data_stream();
        let first = String::from_utf8(body.next().await.unwrap().unwrap().to_vec()).unwrap();
        assert!(first.starts_with("event: snapshot\ndata: {"), "{first}");
        assert!(first.contains("\"schema\":2"));
        hub.tick().await;
        let next = tokio::time::timeout(Duration::from_secs(2), body.next()).await.unwrap().unwrap().unwrap();
        assert!(String::from_utf8(next.to_vec()).unwrap().contains("\"now_ms\":6000"));
    }

    #[tokio::test]
    async fn the_sampler_reads_nothing_while_nobody_watches() {
        let (hub, fake) = hub(false);
        let task = tokio::spawn(hub.clone().run_sampler());
        tokio::time::sleep(Duration::from_millis(1_300)).await;
        assert_eq!(fake.reads.load(Ordering::SeqCst), 0);
        let _rx = hub.tx.subscribe();
        tokio::time::sleep(Duration::from_millis(1_300)).await;
        assert!(fake.reads.load(Ordering::SeqCst) >= 1);
        task.abort();
    }

    #[tokio::test]
    async fn the_daemons_route_answers_here_too_with_since_ms() {
        let (hub, fake) = hub(false);
        let json = get_json(router(hub.clone()), "/api/ci-queue/status").await;
        assert_eq!(json["snapshot"]["schema"], 2);
        assert_eq!(json["samples"].as_array().unwrap().len(), 1);
        let t = json["samples"][0]["t_ms"].as_u64().unwrap();
        let again = get_json(router(hub), &format!("/api/ci-queue/status?since_ms={t}")).await;
        assert!(again["samples"].as_array().unwrap().is_empty(), "nothing newer than since_ms");
        assert_eq!(fake.reads.load(Ordering::SeqCst), 1, "a poll inside a tick reuses the read");
    }

    /// A stand-in daemon on a free port answering `/api/ci-queue/status`.
    async fn fake_daemon(status: StatusCode, body: serde_json::Value) -> String {
        let app = Router::new().route(
            "/api/ci-queue/status",
            get(move || {
                let body = body.clone();
                async move { (status, Json(body)) }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        addr
    }

    #[tokio::test]
    async fn a_daemon_with_a_live_queue_tab_is_used() {
        let addr = fake_daemon(StatusCode::OK, serde_json::json!({ "snapshot": { "running": [] }, "samples": [] })).await;
        assert!(daemon_serves_queue(&addr).await);
        assert_eq!(daemon_queue_url("127.0.0.1:8787", false), "http://127.0.0.1:8787/#queue");
        assert_eq!(daemon_queue_url("127.0.0.1:8787", true), "http://127.0.0.1:8787/?demo#queue");
    }

    #[tokio::test]
    async fn an_old_daemon_a_daemon_without_a_queue_or_no_daemon_means_serve_here() {
        let old = fake_daemon(StatusCode::NOT_FOUND, serde_json::json!({})).await;
        assert!(!daemon_serves_queue(&old).await, "older than the Queue tab");
        let no_queue = fake_daemon(StatusCode::OK, serde_json::json!({ "snapshot": null, "samples": [], "error": "no ci-queue" })).await;
        assert!(!daemon_serves_queue(&no_queue).await, "its th has no queue");
        let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gone = free.local_addr().unwrap().to_string();
        drop(free);
        assert!(!daemon_serves_queue(&gone).await, "nothing listening");
    }

    #[tokio::test]
    async fn the_page_is_served_at_root() {
        let (hub, _) = hub(false);
        let res = router(hub).oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn urls_point_at_a_reachable_host() {
        let any: SocketAddr = "0.0.0.0:4380".parse().unwrap();
        assert_eq!(page_url(any, false), "http://127.0.0.1:4380/");
        let lo: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        assert_eq!(page_url(lo, true), "http://127.0.0.1:5000/?demo");
    }

    #[tokio::test]
    async fn a_taken_port_falls_back_to_a_free_one() {
        let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = held.local_addr().unwrap().port();
        let l = bind(IpAddr::V4(Ipv4Addr::LOCALHOST), port).await.unwrap();
        assert_ne!(l.local_addr().unwrap().port(), port);
    }
}

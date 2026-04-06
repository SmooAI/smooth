//! Scribe → Archivist forwarder.
//!
//! Runs as a background tokio task inside the Scribe process. Every
//! `LogEntry` posted to `/log` is cloned into a bounded mpsc channel; the
//! forwarder batches entries (by size or time) and POSTs them to the
//! Archivist's `/ingest` endpoint as JSON.
//!
//! Invariants:
//!
//! * **Non-blocking for the request path**: `/log` never waits on network
//!   I/O. If the mpsc is full, the oldest entry is dropped.
//! * **Fire-and-forget**: a POST failure is logged and swallowed. Archivist
//!   downtime cannot wedge the agent.
//! * **No shared state with Big Smooth's Scribe instance**: even when
//!   Scribe and Archivist run in the same process (Boardroom mode), the
//!   forwarder uses its own reqwest client and its own mpsc channel.

use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc;

use crate::log_entry::LogEntry;

/// JSON wire envelope matching `smooth_archivist::IngestBatch`. We don't
/// depend on smooth-archivist (would create a cycle: archivist → scribe
/// → archivist), so the struct is defined locally with serde attributes
/// that produce the same shape.
#[derive(Debug, Serialize)]
struct IngestEnvelope<'a> {
    entries: &'a [LogEntry],
    source_vm: &'a str,
}

/// How many log entries may sit in the forwarder channel before the
/// oldest is dropped. Chosen to comfortably cover a burst from a busy
/// agent while bounding memory at a few MB in the worst case.
const CHANNEL_CAPACITY: usize = 1024;

/// Flush the buffer when it reaches this many entries.
const MAX_BATCH_SIZE: usize = 50;

/// Flush the buffer at least this often regardless of size.
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// Handle to a running forwarder. Holds the sender half of the mpsc
/// channel; cloning is cheap (it's an Arc inside).
#[derive(Debug, Clone)]
pub struct ForwarderHandle {
    tx: mpsc::Sender<LogEntry>,
}

impl ForwarderHandle {
    /// Enqueue an entry. If the channel is full, the entry is **dropped**
    /// silently — this is the price of a non-blocking hot path.
    pub fn try_forward(&self, entry: LogEntry) {
        match self.tx.try_send(entry) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("scribe forwarder channel full; dropping log entry");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!("scribe forwarder channel closed; dropping log entry");
            }
        }
    }
}

/// Spawn the forwarder. Returns a handle the Scribe server clones into
/// its `AppState` and uses from `POST /log`.
///
/// `archivist_url` should be the base URL, e.g. `http://host.containers.internal:4401`.
/// The forwarder appends `/ingest` before POSTing.
#[must_use]
pub fn spawn(archivist_url: String, source_vm: String) -> ForwarderHandle {
    let (tx, mut rx) = mpsc::channel::<LogEntry>(CHANNEL_CAPACITY);
    let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap_or_else(|_| reqwest::Client::new());
    let url = format!("{}/ingest", archivist_url.trim_end_matches('/'));

    tokio::spawn(async move {
        let mut buffer: Vec<LogEntry> = Vec::with_capacity(MAX_BATCH_SIZE);
        let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                maybe_entry = rx.recv() => {
                    match maybe_entry {
                        Some(entry) => {
                            buffer.push(entry);
                            if buffer.len() >= MAX_BATCH_SIZE {
                                flush_once(&client, &url, &source_vm, &mut buffer).await;
                            }
                        }
                        None => {
                            // Sender dropped — drain and exit.
                            if !buffer.is_empty() {
                                flush_once(&client, &url, &source_vm, &mut buffer).await;
                            }
                            tracing::debug!("scribe forwarder: channel closed; exiting");
                            return;
                        }
                    }
                }
                _ = ticker.tick() => {
                    if !buffer.is_empty() {
                        flush_once(&client, &url, &source_vm, &mut buffer).await;
                    }
                }
            }
        }
    });

    ForwarderHandle { tx }
}

async fn flush_once(client: &reqwest::Client, url: &str, source_vm: &str, buffer: &mut Vec<LogEntry>) {
    let envelope = IngestEnvelope {
        entries: buffer.as_slice(),
        source_vm,
    };
    match client.post(url).json(&envelope).send().await {
        Ok(resp) if resp.status().is_success() => {
            tracing::trace!(batch = buffer.len(), "scribe forwarder: flushed batch");
        }
        Ok(resp) => {
            tracing::warn!(status = %resp.status(), "scribe forwarder: non-success from archivist");
        }
        Err(e) => {
            tracing::warn!(error = %e, "scribe forwarder: archivist POST failed");
        }
    }
    buffer.clear();
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::routing::post;
    use axum::{Json, Router};

    use super::*;
    use crate::log_entry::{LogEntry, LogLevel};

    /// Spawn a stub archivist and return (base_url, received-batches handle).
    async fn stub_archivist() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
        let received: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        let app = Router::new().route(
            "/ingest",
            post(move |Json(batch): Json<serde_json::Value>| {
                let received = Arc::clone(&received_clone);
                async move {
                    received.lock().expect("lock").push(batch);
                    axum::http::StatusCode::CREATED
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        // Give axum a tick to accept.
        tokio::time::sleep(Duration::from_millis(20)).await;
        (format!("http://{addr}"), received)
    }

    #[tokio::test]
    async fn forwarder_flushes_on_interval() {
        let (url, received) = stub_archivist().await;
        let handle = spawn(url, "vm-under-test".into());

        // Push one entry (less than MAX_BATCH_SIZE) and wait past the tick.
        handle.try_forward(LogEntry::new("svc", LogLevel::Info, "tick-test"));
        tokio::time::sleep(Duration::from_millis(700)).await;

        let batches = received.lock().expect("lock").clone();
        assert_eq!(batches.len(), 1, "expected exactly one flushed batch, got {batches:?}");
        let b = &batches[0];
        assert_eq!(b["source_vm"], "vm-under-test");
        assert_eq!(b["entries"].as_array().expect("entries array").len(), 1);
    }

    #[tokio::test]
    async fn forwarder_flushes_on_batch_size() {
        let (url, received) = stub_archivist().await;
        let handle = spawn(url, "vm-batch".into());

        for i in 0..MAX_BATCH_SIZE {
            handle.try_forward(LogEntry::new("svc", LogLevel::Info, format!("msg-{i}")));
        }
        // No sleep past the tick — the size trigger should fire first.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let batches = received.lock().expect("lock").clone();
        assert!(!batches.is_empty(), "expected at least one batch");
        let total: usize = batches.iter().map(|b| b["entries"].as_array().expect("arr").len()).sum();
        assert_eq!(total, MAX_BATCH_SIZE);
    }

    #[tokio::test]
    async fn forwarder_survives_archivist_down() {
        // Point at an unused port. POSTs should fail but the hot path
        // (try_forward) must remain instant.
        let handle = spawn("http://127.0.0.1:1".into(), "vm-down".into());
        let start = std::time::Instant::now();
        for _ in 0..5 {
            handle.try_forward(LogEntry::new("svc", LogLevel::Warn, "lost in the void"));
        }
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_millis(50), "try_forward must be non-blocking, took {elapsed:?}");
    }
}

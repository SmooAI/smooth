//! The flow WebSocket, on its own thread with a tokio runtime. Frames come
//! out as [`Event`]s on a futures channel the UI awaits; frames go in through
//! [`Outbox`]. Reconnects with backoff, re-reading the address each time (the
//! SmoothFlow daemon may have restarted on a new port).

use std::path::PathBuf;
use std::time::Duration;

use futures::channel::mpsc as fmpsc;
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::discovery;
use crate::frames::{self, Inbound};

/// What the UI hears about the connection.
#[derive(Debug, Clone)]
pub enum Event {
    Connecting,
    /// Connected to this endpoint (its address and token also serve HTTP).
    Connected(discovery::Endpoint),
    Offline(String),
    Frame(Inbound),
}

/// Sends a text frame to the engine; dropped while offline.
#[derive(Clone)]
pub struct Outbox(mpsc::UnboundedSender<String>);

impl Outbox {
    pub fn send(&self, frame: String) {
        let _ = self.0.send(frame);
    }
}

/// Start the connection thread.
#[must_use]
pub fn start(smooth_dir: PathBuf) -> (Outbox, fmpsc::UnboundedReceiver<Event>) {
    let (out_tx, out_rx) = mpsc::unbounded_channel::<String>();
    let (ev_tx, ev_rx) = fmpsc::unbounded::<Event>();
    std::thread::Builder::new()
        .name("smoothflow-net".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                let _ = ev_tx.unbounded_send(Event::Offline("could not start the network runtime".into()));
                return;
            };
            rt.block_on(run(smooth_dir, out_rx, ev_tx));
        })
        .ok();
    (Outbox(out_tx), ev_rx)
}

async fn run(smooth_dir: PathBuf, mut out_rx: mpsc::UnboundedReceiver<String>, ev_tx: fmpsc::UnboundedSender<Event>) {
    let mut backoff = Duration::from_millis(500);
    loop {
        let _ = ev_tx.unbounded_send(Event::Connecting);
        let Some(endpoint) = discovery::discover(&smooth_dir) else {
            let _ = ev_tx.unbounded_send(Event::Offline("no flow engine advertised — start SmoothFlow's daemon or `th up`".into()));
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(10));
            continue;
        };
        match tokio_tungstenite::connect_async(endpoint.ws_url()).await {
            Ok((ws, _)) => {
                backoff = Duration::from_millis(500);
                let _ = ev_tx.unbounded_send(Event::Connected(endpoint.clone()));
                let (mut sink, mut source) = ws.split();
                loop {
                    tokio::select! {
                        msg = source.next() => match msg {
                            Some(Ok(Message::Text(t))) => {
                                if let Some(f) = frames::parse(&t) {
                                    let _ = ev_tx.unbounded_send(Event::Frame(f));
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => {}
                            Some(Err(e)) => {
                                let _ = ev_tx.unbounded_send(Event::Offline(e.to_string()));
                                break;
                            }
                        },
                        out = out_rx.recv() => match out {
                            Some(frame) => {
                                if sink.send(Message::Text(frame.into())).await.is_err() {
                                    break;
                                }
                            }
                            None => return,
                        },
                    }
                }
                let _ = ev_tx.unbounded_send(Event::Offline(format!("disconnected from {}", endpoint.addr)));
            }
            Err(e) => {
                let _ = ev_tx.unbounded_send(Event::Offline(format!("{} — {e}", endpoint.addr)));
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(10));
    }
}

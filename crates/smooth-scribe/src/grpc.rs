//! Tonic gRPC server adapter for the Scribe service.
//!
//! Two streaming RPCs:
//! - `Log` (client-streaming): the runner + cast push log entries
//!   to the server. Server acks with a count after the client
//!   half-closes.
//! - `Query` (server-streaming): caller filters; matches stream as
//!   the server walks the store.
//!
//! Pearl th-893801 iter-2.

use crate::pb;
use async_trait::async_trait;
use futures_util::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

/// What the gRPC adapter needs from the underlying log store.
#[async_trait]
pub trait Logger: Send + Sync + 'static {
    /// Append one entry. Returns true if the entry was persisted,
    /// false if it was dropped (back-pressure).
    async fn append(&self, entry: pb::LogEntry) -> bool;

    /// Walk the store yielding matches. The implementation streams
    /// into the channel; the gRPC server forwards them.
    async fn query(&self, request: pb::QueryRequest, tx: mpsc::Sender<pb::LogEntry>);

    /// Counts + bytes-on-disk for the GetStats RPC. Defaults to zero.
    async fn stats(&self) -> pb::Stats {
        pb::Stats::default()
    }
}

/// Tonic-facing wrapper around a `Logger` implementation.
pub struct ScribeGrpcServer<L: Logger> {
    logger: Arc<L>,
}

impl<L: Logger> ScribeGrpcServer<L> {
    pub fn new(logger: Arc<L>) -> Self {
        Self { logger }
    }
}

#[async_trait]
impl<L: Logger> pb::scribe_server::Scribe for ScribeGrpcServer<L> {
    async fn log(&self, request: tonic::Request<tonic::Streaming<pb::LogEntry>>) -> Result<tonic::Response<pb::LogAck>, tonic::Status> {
        let mut stream = request.into_inner();
        let mut received = 0u64;
        let mut persisted = 0u64;
        let mut dropped_reason = String::new();
        while let Some(entry_res) = futures_util::StreamExt::next(&mut stream).await {
            let entry = match entry_res {
                Ok(e) => e,
                Err(status) => {
                    // Client-side error (deserialize, dropped conn).
                    // Don't abort the stream — return what we have.
                    dropped_reason = format!("stream error: {status}");
                    break;
                }
            };
            received += 1;
            if self.logger.append(entry).await {
                persisted += 1;
            } else if dropped_reason.is_empty() {
                dropped_reason = "logger dropped entry under back-pressure".into();
            }
        }
        Ok(tonic::Response::new(pb::LogAck {
            entries_received: received,
            entries_persisted: persisted,
            dropped_reason,
        }))
    }

    type QueryStream = Pin<Box<dyn Stream<Item = Result<pb::LogEntry, tonic::Status>> + Send + 'static>>;

    async fn query(&self, request: tonic::Request<pb::QueryRequest>) -> Result<tonic::Response<Self::QueryStream>, tonic::Status> {
        let req = request.into_inner();
        // Bound the channel so a slow consumer back-pressures the
        // store walker rather than blowing up memory.
        let (tx, rx) = mpsc::channel(64);
        let logger = self.logger.clone();
        tokio::spawn(async move {
            logger.query(req, tx).await;
        });
        let stream: Self::QueryStream = Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok));
        Ok(tonic::Response::new(stream))
    }

    async fn get_stats(&self, _request: tonic::Request<pb::GetStatsRequest>) -> Result<tonic::Response<pb::Stats>, tonic::Status> {
        Ok(tonic::Response::new(self.logger.stats().await))
    }
}

// `.map()` is on tokio_stream's StreamExt; bring it into scope.
use tokio_stream::StreamExt;

/// Spawn a Scribe gRPC server on a UDS. Sync — spawns the server
/// task and returns its JoinHandle.
///
/// # Errors
///
/// Returns the underlying io::Error if binding the UDS fails.
pub fn serve_uds<L: Logger>(logger: Arc<L>, uds_path: std::path::PathBuf) -> std::io::Result<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>> {
    let _ = std::fs::remove_file(&uds_path);
    let uds = tokio::net::UnixListener::bind(uds_path)?;
    let svc = pb::scribe_server::ScribeServer::new(ScribeGrpcServer::new(logger));
    let handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(uds))
            .await
    });
    Ok(handle)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;
    use tempfile::TempDir;
    use tower::service_fn;

    /// In-memory stub Logger — keeps a Vec of received entries and
    /// streams them back on Query (no filtering, just for the
    /// round-trip).
    #[derive(Default)]
    struct VecLogger {
        entries: Mutex<Vec<pb::LogEntry>>,
    }

    #[async_trait]
    impl Logger for VecLogger {
        async fn append(&self, entry: pb::LogEntry) -> bool {
            self.entries.lock().unwrap().push(entry);
            true
        }
        async fn query(&self, _request: pb::QueryRequest, tx: mpsc::Sender<pb::LogEntry>) {
            let entries = self.entries.lock().unwrap().clone();
            for e in entries {
                if tx.send(e).await.is_err() {
                    break;
                }
            }
        }
        async fn stats(&self) -> pb::Stats {
            let entries = self.entries.lock().unwrap();
            pb::Stats {
                total_entries: entries.len() as u64,
                bytes_on_disk: 0,
                oldest_entry_at: None,
                newest_entry_at: None,
                counts_by_source: std::collections::HashMap::new(),
            }
        }
    }

    async fn build_uds_client(uds_path: std::path::PathBuf) -> pb::scribe_client::ScribeClient<tonic::transport::Channel> {
        let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")
            .unwrap()
            .connect_with_connector(service_fn(move |_: tonic::transport::Uri| {
                let path = uds_path.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(path).await?;
                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                }
            }))
            .await
            .expect("connect UDS");
        pb::scribe_client::ScribeClient::new(channel)
    }

    fn make_entry(message: &str) -> pb::LogEntry {
        pb::LogEntry {
            timestamp: None,
            source: "test".into(),
            operator_id: String::new(),
            bead_id: String::new(),
            level: pb::Level::Info as i32,
            message: message.into(),
            fields: std::collections::HashMap::new(),
            trace_id: String::new(),
            span_id: String::new(),
        }
    }

    #[tokio::test]
    async fn log_client_streams_round_trip() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("scribe.sock");
        let logger = Arc::new(VecLogger::default());
        let _server = serve_uds(logger.clone(), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let entries = vec![make_entry("first"), make_entry("second"), make_entry("third")];

        let in_stream = tokio_stream::iter(entries.clone());
        let ack = client.log(tonic::Request::new(in_stream)).await.unwrap().into_inner();
        assert_eq!(ack.entries_received, 3);
        assert_eq!(ack.entries_persisted, 3);
        assert_eq!(ack.dropped_reason, "");
        assert_eq!(logger.entries.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn query_server_streams_round_trip() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("scribe.sock");
        let logger = Arc::new(VecLogger::default());
        logger.entries.lock().unwrap().push(make_entry("alpha"));
        logger.entries.lock().unwrap().push(make_entry("beta"));
        let _server = serve_uds(logger.clone(), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let mut stream = client.query(tonic::Request::new(pb::QueryRequest::default())).await.unwrap().into_inner();
        let mut collected = Vec::new();
        while let Some(entry) = StreamExt::next(&mut stream).await {
            collected.push(entry.unwrap());
        }
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].message, "alpha");
        assert_eq!(collected[1].message, "beta");
    }

    #[tokio::test]
    async fn get_stats_returns_logger_count() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("scribe.sock");
        let logger = Arc::new(VecLogger::default());
        logger.entries.lock().unwrap().push(make_entry("one"));
        let _server = serve_uds(logger.clone(), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let stats = client
            .get_stats(tonic::Request::new(pb::GetStatsRequest::default()))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(stats.total_entries, 1);
    }

    #[tokio::test]
    async fn log_persisted_count_can_be_less_than_received() {
        // Drop everything to verify back-pressure flow.
        struct DroppingLogger;
        #[async_trait]
        impl Logger for DroppingLogger {
            async fn append(&self, _entry: pb::LogEntry) -> bool {
                false
            }
            async fn query(&self, _req: pb::QueryRequest, _tx: mpsc::Sender<pb::LogEntry>) {}
        }
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("scribe.sock");
        let _server = serve_uds(Arc::new(DroppingLogger), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let in_stream = tokio_stream::iter(vec![make_entry("a"), make_entry("b")]);
        let ack = client.log(tonic::Request::new(in_stream)).await.unwrap().into_inner();
        assert_eq!(ack.entries_received, 2);
        assert_eq!(ack.entries_persisted, 0);
        assert!(ack.dropped_reason.contains("back-pressure"));
    }
}

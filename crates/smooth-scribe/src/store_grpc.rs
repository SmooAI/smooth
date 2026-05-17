//! Wire `MemoryLogStore` into `grpc::Logger` so the production
//! store serves the new gRPC Scribe surface unchanged.
//!
//! Pearl th-893801 iter-3c. The domain `LogEntry` predates the
//! proto contract by a wide margin, so this module also owns the
//! proto<->domain conversion. Lossy in two well-defined ways:
//!
//! - `pb::Level::Trace` and `pb::Level::Unspecified` fold to
//!   domain `Debug` / `Info` respectively (domain has no Trace).
//! - Domain `id` (uuid) has no proto equivalent — generated on
//!   append, dropped on emit. Queries match on the rest.
//!
//! When iter-3f swaps the runner's HTTP forwarder for a gRPC
//! client, this is the only translation layer it crosses.

use async_trait::async_trait;
use chrono::TimeZone;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::grpc::Logger;
use crate::log_entry::{LogEntry, LogLevel};
use crate::pb;
use crate::store::{LogStore, MemoryLogStore, Query};

/// Convert a proto Level into the four-level domain enum. Trace
/// folds to Debug; Unspecified defaults to Info (the most common
/// emitted level — matches how the HTTP path treats missing
/// levels).
fn level_from_pb(level: i32) -> LogLevel {
    match pb::Level::try_from(level).unwrap_or(pb::Level::Unspecified) {
        pb::Level::Trace | pb::Level::Debug => LogLevel::Debug,
        pb::Level::Warn => LogLevel::Warn,
        pb::Level::Error => LogLevel::Error,
        pb::Level::Info | pb::Level::Unspecified => LogLevel::Info,
    }
}

fn level_to_pb(level: LogLevel) -> pb::Level {
    match level {
        LogLevel::Debug => pb::Level::Debug,
        LogLevel::Info => pb::Level::Info,
        LogLevel::Warn => pb::Level::Warn,
        LogLevel::Error => pb::Level::Error,
    }
}

fn ts_from_pb(ts: Option<prost_types::Timestamp>) -> chrono::DateTime<chrono::Utc> {
    ts.and_then(|t| chrono::Utc.timestamp_opt(t.seconds, u32::try_from(t.nanos.max(0)).unwrap_or(0)).single())
        .unwrap_or_else(chrono::Utc::now)
}

fn ts_to_pb(ts: chrono::DateTime<chrono::Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: ts.timestamp(),
        nanos: ts.timestamp_subsec_nanos().min(i32::MAX as u32).try_into().unwrap_or(0),
    }
}

/// Build a domain LogEntry from a proto LogEntry. Generates a
/// fresh uuid; preserves all other fields with the level fold
/// described above.
pub fn entry_from_pb(pb: pb::LogEntry) -> LogEntry {
    let mut entry = LogEntry::new(&pb.source, level_from_pb(pb.level), &pb.message);
    entry.timestamp = ts_from_pb(pb.timestamp);
    entry.fields = pb.fields;
    if !pb.operator_id.is_empty() {
        entry.operator_id = Some(pb.operator_id);
    }
    if !pb.bead_id.is_empty() {
        entry.bead_id = Some(pb.bead_id);
    }
    if !pb.trace_id.is_empty() {
        entry.trace_id = Some(pb.trace_id);
    }
    if !pb.span_id.is_empty() {
        entry.span_id = Some(pb.span_id);
    }
    entry
}

/// Emit a proto LogEntry from a domain entry. Drops the uuid
/// (proto doesn't carry one).
pub fn entry_to_pb(entry: &LogEntry) -> pb::LogEntry {
    pb::LogEntry {
        timestamp: Some(ts_to_pb(entry.timestamp)),
        source: entry.service.clone(),
        operator_id: entry.operator_id.clone().unwrap_or_default(),
        bead_id: entry.bead_id.clone().unwrap_or_default(),
        level: level_to_pb(entry.level) as i32,
        message: entry.message.clone(),
        fields: entry.fields.clone(),
        trace_id: entry.trace_id.clone().unwrap_or_default(),
        span_id: entry.span_id.clone().unwrap_or_default(),
    }
}

/// Translate a proto QueryRequest into the in-store filter the
/// domain Query understands. The proto surface is richer than the
/// domain query (since/until/operator_id/bead_id/trace_id/
/// message_contains/source) — for iter-3c we honor the subset the
/// store handles natively (service/min_level/limit) and apply the
/// remaining filters in-process during the walk.
fn build_domain_query(req: &pb::QueryRequest) -> Query {
    let limit = if req.limit == 0 { usize::MAX } else { req.limit as usize };
    let min_level = if req.min_level == pb::Level::Unspecified as i32 {
        None
    } else {
        Some(level_from_pb(req.min_level))
    };
    let service = if req.source.is_empty() { None } else { Some(req.source.clone()) };
    Query { service, min_level, limit }
}

fn matches_extra_filters(entry: &LogEntry, req: &pb::QueryRequest) -> bool {
    if !req.operator_id.is_empty() && entry.operator_id.as_deref() != Some(req.operator_id.as_str()) {
        return false;
    }
    if !req.bead_id.is_empty() && entry.bead_id.as_deref() != Some(req.bead_id.as_str()) {
        return false;
    }
    if !req.trace_id.is_empty() && entry.trace_id.as_deref() != Some(req.trace_id.as_str()) {
        return false;
    }
    if !req.message_contains.is_empty() {
        let needle = req.message_contains.to_lowercase();
        if !entry.message.to_lowercase().contains(&needle) {
            return false;
        }
    }
    if let Some(since) = req.since {
        let since_dt = ts_from_pb(Some(since));
        if entry.timestamp < since_dt {
            return false;
        }
    }
    if let Some(until) = req.until {
        let until_dt = ts_from_pb(Some(until));
        if entry.timestamp > until_dt {
            return false;
        }
    }
    true
}

/// Wrap any `Arc<LogStore>` (production: `MemoryLogStore`) so it
/// can be served as the gRPC `Logger`. The wrapper exists so
/// MemoryLogStore stays a pure domain type — the proto
/// dependency lives here.
pub struct GrpcLogStoreAdapter<S: LogStore + 'static> {
    inner: Arc<S>,
}

impl<S: LogStore + 'static> GrpcLogStoreAdapter<S> {
    pub fn new(inner: Arc<S>) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &Arc<S> {
        &self.inner
    }
}

#[async_trait]
impl<S: LogStore + Send + Sync + 'static> Logger for GrpcLogStoreAdapter<S> {
    async fn append(&self, entry: pb::LogEntry) -> bool {
        self.inner.append(entry_from_pb(entry));
        true
    }

    async fn query(&self, request: pb::QueryRequest, tx: mpsc::Sender<pb::LogEntry>) {
        // Pull the cheap-filter set first; honor the extra proto
        // filters in-process. The store already returns newest-
        // first; we keep that ordering for the stream.
        let domain_query = build_domain_query(&request);
        let entries = self.inner.query(&domain_query);
        for entry in entries {
            if !matches_extra_filters(&entry, &request) {
                continue;
            }
            if tx.send(entry_to_pb(&entry)).await.is_err() {
                // Client cancelled — stop walking.
                return;
            }
        }
    }

    async fn stats(&self) -> pb::Stats {
        pb::Stats {
            total_entries: self.inner.count() as u64,
            ..pb::Stats::default()
        }
    }
}

/// Convenience: build an adapter directly from a fresh
/// MemoryLogStore (the common production wiring).
pub fn adapter_for_memory_store() -> GrpcLogStoreAdapter<MemoryLogStore> {
    GrpcLogStoreAdapter::new(Arc::new(MemoryLogStore::new()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::grpc::serve_uds;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio_stream::StreamExt;
    use tower::service_fn;

    async fn build_client(uds_path: PathBuf) -> pb::scribe_client::ScribeClient<tonic::transport::Channel> {
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

    fn make_pb_entry(source: &str, level: pb::Level, msg: &str) -> pb::LogEntry {
        pb::LogEntry {
            timestamp: Some(prost_types::Timestamp { seconds: 1_000_000, nanos: 0 }),
            source: source.into(),
            operator_id: String::new(),
            bead_id: String::new(),
            level: level as i32,
            message: msg.into(),
            fields: std::collections::HashMap::new(),
            trace_id: String::new(),
            span_id: String::new(),
        }
    }

    #[test]
    fn level_roundtrip_preserves_four_canonical_levels() {
        for level in [LogLevel::Debug, LogLevel::Info, LogLevel::Warn, LogLevel::Error] {
            let pb_level = level_to_pb(level) as i32;
            assert_eq!(level_from_pb(pb_level), level);
        }
    }

    #[test]
    fn level_trace_folds_to_debug() {
        assert_eq!(level_from_pb(pb::Level::Trace as i32), LogLevel::Debug);
    }

    #[test]
    fn level_unspecified_folds_to_info() {
        assert_eq!(level_from_pb(pb::Level::Unspecified as i32), LogLevel::Info);
    }

    #[test]
    fn entry_roundtrip_preserves_fields() {
        let mut original = LogEntry::new("svc", LogLevel::Warn, "hello")
            .with_operator("op-1")
            .with_bead("bd-1")
            .with_trace("t-1", "s-1");
        original.fields.insert("k".into(), "v".into());
        let pb_entry = entry_to_pb(&original);
        let restored = entry_from_pb(pb_entry);
        assert_eq!(restored.service, original.service);
        assert_eq!(restored.message, original.message);
        assert_eq!(restored.level, original.level);
        assert_eq!(restored.operator_id, original.operator_id);
        assert_eq!(restored.bead_id, original.bead_id);
        assert_eq!(restored.trace_id, original.trace_id);
        assert_eq!(restored.span_id, original.span_id);
        assert_eq!(restored.fields, original.fields);
        // id is regenerated on the return trip — confirm it's
        // populated but don't assert equality.
        assert!(!restored.id.is_empty());
    }

    #[tokio::test]
    async fn append_via_grpc_lands_in_store() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("scribe.sock");
        let store = Arc::new(MemoryLogStore::new());
        let adapter = Arc::new(GrpcLogStoreAdapter::new(store.clone()));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let entries = vec![
            make_pb_entry("operator-runner", pb::Level::Info, "hello"),
            make_pb_entry("operator-runner", pb::Level::Warn, "warning"),
        ];
        let stream = tokio_stream::iter(entries);
        let resp = client.log(tonic::Request::new(stream)).await.unwrap().into_inner();
        assert_eq!(resp.entries_received, 2);
        assert_eq!(resp.entries_persisted, 2);
        assert_eq!(store.count(), 2);
    }

    #[tokio::test]
    async fn query_streams_back_pb_entries() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("scribe.sock");
        let store = Arc::new(MemoryLogStore::new());
        store.append(LogEntry::new("alpha", LogLevel::Info, "a"));
        store.append(LogEntry::new("beta", LogLevel::Info, "b"));
        store.append(LogEntry::new("alpha", LogLevel::Warn, "c"));

        let adapter = Arc::new(GrpcLogStoreAdapter::new(store.clone()));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client
            .query(tonic::Request::new(pb::QueryRequest {
                source: "alpha".into(),
                limit: 0,
                ..Default::default()
            }))
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        let mut collected = Vec::new();
        while let Some(item) = stream.next().await {
            collected.push(item.unwrap());
        }
        assert_eq!(collected.len(), 2);
        assert!(collected.iter().all(|e| e.source == "alpha"));
    }

    #[tokio::test]
    async fn query_respects_min_level_filter() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("scribe.sock");
        let store = Arc::new(MemoryLogStore::new());
        store.append(LogEntry::new("svc", LogLevel::Debug, "a"));
        store.append(LogEntry::new("svc", LogLevel::Info, "b"));
        store.append(LogEntry::new("svc", LogLevel::Warn, "c"));
        store.append(LogEntry::new("svc", LogLevel::Error, "d"));

        let adapter = Arc::new(GrpcLogStoreAdapter::new(store));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client
            .query(tonic::Request::new(pb::QueryRequest {
                min_level: pb::Level::Warn as i32,
                limit: 0,
                ..Default::default()
            }))
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        let mut collected = Vec::new();
        while let Some(item) = stream.next().await {
            collected.push(item.unwrap());
        }
        assert_eq!(collected.len(), 2);
    }

    #[tokio::test]
    async fn query_message_contains_is_case_insensitive() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("scribe.sock");
        let store = Arc::new(MemoryLogStore::new());
        store.append(LogEntry::new("svc", LogLevel::Info, "Connection refused"));
        store.append(LogEntry::new("svc", LogLevel::Info, "deploy succeeded"));

        let adapter = Arc::new(GrpcLogStoreAdapter::new(store));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client
            .query(tonic::Request::new(pb::QueryRequest {
                message_contains: "CONNECTION".into(),
                limit: 0,
                ..Default::default()
            }))
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        let mut collected = Vec::new();
        while let Some(item) = stream.next().await {
            collected.push(item.unwrap());
        }
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].message, "Connection refused");
    }

    #[tokio::test]
    async fn get_stats_reports_total_entries() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("scribe.sock");
        let store = Arc::new(MemoryLogStore::new());
        for i in 0..7 {
            store.append(LogEntry::new("svc", LogLevel::Info, format!("msg-{i}")));
        }
        let adapter = Arc::new(GrpcLogStoreAdapter::new(store));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let stats = client.get_stats(tonic::Request::new(pb::GetStatsRequest {})).await.unwrap().into_inner();
        assert_eq!(stats.total_entries, 7);
    }

    #[tokio::test]
    async fn adapter_for_memory_store_helper_works() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("scribe.sock");
        let adapter = Arc::new(adapter_for_memory_store());
        let store = adapter.inner().clone();

        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let stream = tokio_stream::iter(vec![make_pb_entry("svc", pb::Level::Info, "hi")]);
        let resp = client.log(tonic::Request::new(stream)).await.unwrap().into_inner();
        assert_eq!(resp.entries_persisted, 1);
        assert_eq!(store.count(), 1);
    }
}

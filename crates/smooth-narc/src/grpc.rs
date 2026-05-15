//! Thin server adapter that turns a `Judge` trait into a tonic gRPC
//! service. The Judge trait abstracts the actual decision engine —
//! `smooth_bigsmooth::boardroom_narc::BoardroomNarc` implements it,
//! and tests can implement it with a stub.
//!
//! Pearl th-893801 spike (iter-1).

// The `From`/`TryFrom` impls in crate::convert are picked up
// automatically because they live in this crate; no `use` needed.
use crate::judge::{JudgeDecision, JudgeRequest};
use crate::pb;
use async_trait::async_trait;
use std::sync::Arc;

/// What the proto-generated `Narc` server trait needs from the host
/// crate to actually decide. Keeping this as a thin trait lets the
/// grpc adapter live in smooth-narc (next to the proto types) while
/// the production implementation lives in smooth-bigsmooth (next to
/// AccessStore + SharedWonkGrants).
#[async_trait]
pub trait Judge: Send + Sync + 'static {
    async fn judge(&self, request: JudgeRequest) -> JudgeDecision;
    fn cache_len(&self) -> usize {
        0
    }
}

/// Tonic-facing wrapper around a `Judge` implementation.
///
/// `BoardroomNarc::judge` is already `async fn judge(&self, JudgeRequest)
/// -> JudgeDecision` — wrapping it as `Judge` is a one-line impl on
/// the smooth-bigsmooth side.
pub struct NarcGrpcServer<J: Judge> {
    judge: Arc<J>,
}

impl<J: Judge> NarcGrpcServer<J> {
    pub fn new(judge: Arc<J>) -> Self {
        Self { judge }
    }
}

#[async_trait]
impl<J: Judge> pb::narc_server::Narc for NarcGrpcServer<J> {
    async fn judge(&self, request: tonic::Request<pb::JudgeRequest>) -> Result<tonic::Response<pb::JudgeDecision>, tonic::Status> {
        let pb_req = request.into_inner();
        let domain_req: JudgeRequest = pb_req.try_into().map_err(|e: String| tonic::Status::invalid_argument(e))?;
        let decision = self.judge.judge(domain_req).await;
        let pb_dec: pb::JudgeDecision = decision.into();
        Ok(tonic::Response::new(pb_dec))
    }

    async fn get_cache_stats(&self, _request: tonic::Request<pb::GetCacheStatsRequest>) -> Result<tonic::Response<pb::CacheStats>, tonic::Status> {
        let len = self.judge.cache_len() as u64;
        Ok(tonic::Response::new(pb::CacheStats {
            entries: len,
            oldest_entry_at: None,
            hit_count: 0,
            miss_count: 0,
        }))
    }
}

/// Convenience: spin a Narc server on a UDS path. Returns the task
/// handle so the caller can abort it; the server runs until the
/// handle is dropped or aborted.
///
/// Used by tests and by the iter-2+ BS startup glue.
///
/// # Errors
///
/// Returns the underlying tonic / tokio error if binding or serving
/// fails.
pub fn serve_uds<J: Judge>(judge: Arc<J>, uds_path: std::path::PathBuf) -> std::io::Result<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>> {
    // Remove any stale socket from a previous run. Common pattern
    // for UDS servers; fail-silently on NotFound.
    let _ = std::fs::remove_file(&uds_path);
    let uds = tokio::net::UnixListener::bind(&uds_path)?;

    let server = NarcGrpcServer::new(judge);
    let svc = pb::narc_server::NarcServer::new(server);

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
    use std::time::Duration;
    use tempfile::TempDir;
    use tower::service_fn;

    /// Minimal in-memory Judge for the grpc round-trip test. Returns
    /// Approve for OBVIOUSLY_SAFE_DOMAIN_SUFFIXES, Deny otherwise.
    struct StubJudge;

    #[async_trait]
    impl Judge for StubJudge {
        async fn judge(&self, request: JudgeRequest) -> JudgeDecision {
            if crate::judge::domain_matches_suffix_list(&request.resource, crate::judge::OBVIOUSLY_SAFE_DOMAIN_SUFFIXES) {
                JudgeDecision::approve(format!("stub: {} is on obviously-safe list", request.resource))
            } else {
                JudgeDecision::deny(format!("stub: {} is not on obviously-safe list", request.resource))
            }
        }
    }

    /// Used by the get_cache_stats test. Counts judge invocations and
    /// exposes the count via `cache_len()` so the test can assert on
    /// the stats RPC's `entries` field round-tripping.
    struct CountingJudge {
        seen: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl Judge for CountingJudge {
        async fn judge(&self, _r: JudgeRequest) -> JudgeDecision {
            self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            JudgeDecision::approve("counted")
        }
        fn cache_len(&self) -> usize {
            self.seen.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    async fn build_uds_client(uds_path: std::path::PathBuf) -> pb::narc_client::NarcClient<tonic::transport::Channel> {
        // The "http://[::]:50051" URI is a placeholder — tonic's
        // UDS connector ignores it and dials the actual socket
        // path via the service_fn closure below.
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
        pb::narc_client::NarcClient::new(channel)
    }

    #[tokio::test]
    async fn judge_round_trips_over_uds_for_safe_domain() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");

        let _server = serve_uds(Arc::new(StubJudge), sock.clone()).unwrap();

        // Tiny wait for tonic to bind — UDS bind is fast but the
        // serve_with_incoming poll hasn't run yet on first iteration.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock.clone()).await;
        let req = tonic::Request::new(pb::JudgeRequest {
            kind: pb::JudgeKind::Network as i32,
            operator_id: "op-1".into(),
            bead_id: "pearl-1".into(),
            phase: "execute".into(),
            resource: "registry.npmjs.org".into(),
            detail: "/".into(),
            task_summary: String::new(),
            agent_reason: String::new(),
        });

        let resp = client.judge(req).await.expect("judge ok").into_inner();
        assert_eq!(resp.decision, pb::Decision::Approve as i32);
        assert!(resp.reason.contains("npmjs.org"));
    }

    #[tokio::test]
    async fn judge_round_trips_for_unknown_domain_yields_deny_from_stub() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");

        let _server = serve_uds(Arc::new(StubJudge), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock.clone()).await;
        let req = tonic::Request::new(pb::JudgeRequest {
            kind: pb::JudgeKind::Network as i32,
            operator_id: "op-1".into(),
            bead_id: "pearl-1".into(),
            phase: "execute".into(),
            resource: "totally-made-up.example".into(),
            detail: "/".into(),
            task_summary: String::new(),
            agent_reason: String::new(),
        });

        let resp = client.judge(req).await.expect("judge ok").into_inner();
        assert_eq!(resp.decision, pb::Decision::Deny as i32);
    }

    #[tokio::test]
    async fn judge_rejects_unspecified_kind_with_invalid_argument() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");

        let _server = serve_uds(Arc::new(StubJudge), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock.clone()).await;
        let req = tonic::Request::new(pb::JudgeRequest {
            kind: pb::JudgeKind::Unspecified as i32,
            operator_id: "op-1".into(),
            bead_id: "pearl-1".into(),
            phase: "execute".into(),
            resource: "doesnt-matter".into(),
            detail: String::new(),
            task_summary: String::new(),
            agent_reason: String::new(),
        });

        let err = client.judge(req).await.expect_err("expected status error");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn get_cache_stats_returns_judge_cache_len() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");

        let judge = Arc::new(CountingJudge {
            seen: std::sync::atomic::AtomicUsize::new(0),
        });
        let _server = serve_uds(judge.clone(), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock.clone()).await;

        // Drive a few Judges, then read the stats.
        for _ in 0..3 {
            let _ = client
                .judge(tonic::Request::new(pb::JudgeRequest {
                    kind: pb::JudgeKind::Network as i32,
                    resource: "anything.example".into(),
                    ..Default::default()
                }))
                .await
                .unwrap();
        }

        let stats = client
            .get_cache_stats(tonic::Request::new(pb::GetCacheStatsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(stats.entries, 3);
        // smoke check that the proto round-trip preserved the value.
        assert_eq!(judge.cache_len(), 3);

        // Sanity check that we haven't accidentally returned the
        // current-time sentinel for oldest_entry_at — stub returns
        // None, proto carries it as the zero proto Timestamp, which
        // the wire format serializes to None on the Rust side.
        assert!(stats.oldest_entry_at.is_none());
    }
}

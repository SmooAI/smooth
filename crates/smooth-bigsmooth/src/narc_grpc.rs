//! Production wiring: SafehouseNarc serves as the gRPC Narc judge.
//!
//! Pearl th-893801 iter-3a. The Judge trait from smooth-narc::grpc
//! matches SafehouseNarc::judge's signature already — this module
//! just implements the trait and exposes a `serve_uds` helper that
//! Big Smooth's startup code can call to bring the gRPC server up
//! at a known socket path (e.g. `$XDG_RUNTIME_DIR/smooth/narc.sock`).

use async_trait::async_trait;
use smooth_narc::grpc::Judge;
use smooth_narc::judge::{JudgeDecision, JudgeRequest};
use std::path::PathBuf;
use std::sync::Arc;

use crate::safehouse_narc::SafehouseNarc;

#[async_trait]
impl Judge for SafehouseNarc {
    async fn judge(&self, request: JudgeRequest) -> JudgeDecision {
        SafehouseNarc::judge(self, request).await
    }

    fn cache_len(&self) -> usize {
        SafehouseNarc::cache_len(self)
    }
}

/// Spawn a tonic Narc server on a UDS, backed by the production
/// SafehouseNarc. Thin wrapper over `smooth_narc::grpc::serve_uds`
/// — kept here so callers don't need to thread the Judge trait
/// import through.
///
/// # Errors
///
/// Returns the underlying io::Error if binding the UDS fails.
pub fn serve_uds(narc: Arc<SafehouseNarc>, uds_path: PathBuf) -> std::io::Result<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>> {
    smooth_narc::grpc::serve_uds(narc, uds_path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::access::AccessStore;
    use smooth_narc::judge::{Decision, JudgeKind};
    use smooth_narc::pb;
    use std::time::Duration;
    use tempfile::TempDir;
    use tower::service_fn;

    async fn build_client(uds_path: PathBuf) -> pb::narc_client::NarcClient<tonic::transport::Channel> {
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

    fn make_req(domain: &str) -> pb::JudgeRequest {
        pb::JudgeRequest {
            kind: pb::JudgeKind::Network as i32,
            operator_id: "op-1".into(),
            bead_id: "pearl-1".into(),
            phase: "execute".into(),
            resource: domain.into(),
            detail: "/".into(),
            task_summary: String::new(),
            agent_reason: String::new(),
        }
    }

    /// Production SafehouseNarc's rule engine short-circuits
    /// OBVIOUSLY_SAFE domains without touching the LLM. The
    /// `without_llm()` constructor is exactly what we want for an
    /// integration test that doesn't need network egress to
    /// providers.json.
    #[tokio::test]
    async fn serve_uds_rule_engine_approves_safe_domain() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");

        let narc = Arc::new(SafehouseNarc::without_llm());
        let _server = serve_uds(narc, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client.judge(tonic::Request::new(make_req("registry.npmjs.org"))).await.unwrap().into_inner();
        assert_eq!(resp.decision, pb::Decision::Approve as i32);
        // Rule-engine decisions have confidence 1.0.
        assert!((resp.confidence - 1.0).abs() < 1e-6);
        // Approve sets a cache TTL.
        assert!(resp.cache_ttl_seconds > 0);
    }

    #[tokio::test]
    async fn serve_uds_rule_engine_denies_dangerous_domain() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");

        let narc = Arc::new(SafehouseNarc::without_llm());
        let _server = serve_uds(narc, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client.judge(tonic::Request::new(make_req("pastebin.com"))).await.unwrap().into_inner();
        assert_eq!(resp.decision, pb::Decision::Deny as i32);
    }

    #[tokio::test]
    async fn serve_uds_unknown_domain_escalates_without_llm() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");

        // No LLM + unknown domain → SafehouseNarc returns
        // EscalateToHuman (the legacy fail-closed shape). The Ask
        // path is only entered when an LLM is actually configured
        // and returns low confidence.
        let narc = Arc::new(SafehouseNarc::without_llm());
        let _server = serve_uds(narc, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client
            .judge(tonic::Request::new(make_req("totally-made-up.example")))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.decision, pb::Decision::EscalateToHuman as i32);
    }

    #[tokio::test]
    async fn serve_uds_with_persistent_grant_short_circuits() {
        // SafehouseNarc::with_grants makes the persistent-grants
        // path live. A grant for `custom.example` should
        // short-circuit to Approve over the gRPC.
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");

        let mut grants = crate::wonk_grants::WonkGrants::new();
        grants.add_host("custom.example");
        let shared = crate::wonk_grants::SharedWonkGrants::new(grants);

        let narc = Arc::new(SafehouseNarc::without_llm().with_grants(shared));
        let _server = serve_uds(narc, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client.judge(tonic::Request::new(make_req("custom.example"))).await.unwrap().into_inner();
        assert_eq!(resp.decision, pb::Decision::Approve as i32);
        assert!(resp.reason.contains("wonk-allow"));
    }

    /// Sanity check: the Judge impl exposed via the gRPC matches
    /// the trait's signature exactly. If SafehouseNarc::judge ever
    /// gains a parameter, this test stops compiling.
    #[tokio::test]
    async fn judge_trait_routes_to_inherent_method() {
        let narc = SafehouseNarc::without_llm();
        let req = JudgeRequest {
            kind: JudgeKind::Network,
            operator_id: "op".into(),
            bead_id: "pearl".into(),
            phase: "execute".into(),
            resource: "registry.npmjs.org".into(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        };
        // Call through the trait — same answer as calling the
        // inherent method directly.
        let via_trait = Judge::judge(&narc, req.clone()).await;
        let via_inherent = SafehouseNarc::judge(&narc, req).await;
        assert_eq!(via_trait.decision, via_inherent.decision);
        assert_eq!(via_trait.decision, Decision::Approve);
    }

    /// Cache len round-trips through GetCacheStats. After a
    /// rule-engine hit the entry lands in the local cache; the
    /// second call should hit the cache and the size reflects it.
    #[tokio::test]
    async fn get_cache_stats_reflects_judge_calls() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");

        let narc = Arc::new(SafehouseNarc::without_llm());
        let _server = serve_uds(narc.clone(), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let initial = client
            .get_cache_stats(tonic::Request::new(pb::GetCacheStatsRequest {}))
            .await
            .unwrap()
            .into_inner();
        let initial_entries = initial.entries;

        let _ = client.judge(tonic::Request::new(make_req("registry.npmjs.org"))).await.unwrap();
        let _ = client.judge(tonic::Request::new(make_req("pypi.org"))).await.unwrap();

        let after = client
            .get_cache_stats(tonic::Request::new(pb::GetCacheStatsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert!(after.entries > initial_entries, "cache should have grown");
    }

    /// AccessStore wired in through with_grants doesn't change the
    /// no-LLM path (we just verify the Narc constructor accepts it
    /// without crashing). The actual Ask flow requires an LLM that
    /// can return low-confidence approvals — exercised in the
    /// existing iter-2 tests at the safehouse_narc unit-test layer.
    #[tokio::test]
    async fn serve_uds_with_access_store_does_not_change_no_llm_path() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");

        let access = AccessStore::new();
        let narc = Arc::new(SafehouseNarc::new(None, access));
        let _server = serve_uds(narc, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client.judge(tonic::Request::new(make_req("pypi.org"))).await.unwrap().into_inner();
        assert_eq!(resp.decision, pb::Decision::Approve as i32);
    }
}

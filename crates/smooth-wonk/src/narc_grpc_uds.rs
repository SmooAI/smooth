//! UDS-dialing Narc client.
//!
//! Pearl th-893801 Phase 4 iter-6c. Counterpart to the
//! legacy HTTP [`crate::narc_client::NarcClient`] — same
//! `NarcEscalator` contract, gRPC-over-UDS transport. Used
//! by the in-VM Wonk when `SMOOTH_SINGLE_PROCESS=1` is set
//! and Narc is reachable on a local UDS socket
//! (`$XDG_RUNTIME_DIR/smooth/narc.sock` by default).
//!
//! The client folds any transport error into
//! `Decision::EscalateToHuman` so Wonk fails closed — same
//! shape as the HTTP client.

use std::path::PathBuf;

use anyhow::{Context, Result};
use async_trait::async_trait;
use smooth_narc::judge::{JudgeDecision, JudgeRequest};

use crate::narc_client::NarcEscalator;

/// UDS-dialing Narc client implementing
/// [`NarcEscalator`].
#[derive(Clone)]
pub struct NarcGrpcUds {
    socket: PathBuf,
    channel: tonic::transport::Channel,
}

impl NarcGrpcUds {
    /// Build a client dialing `socket`. The connection is
    /// established eagerly so callers learn about a missing
    /// socket at construction rather than first-judge.
    ///
    /// # Errors
    ///
    /// Bubbles up the transport-layer error.
    pub async fn connect(socket: PathBuf) -> Result<Self> {
        let endpoint = tonic::transport::Endpoint::try_from("http://[::]:50051").context("build endpoint")?;
        let path_for_conn = socket.clone();
        let channel = endpoint
            .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                let path = path_for_conn.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(path).await?;
                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                }
            }))
            .await
            .with_context(|| format!("connect UDS at {}", socket.display()))?;
        Ok(Self { socket, channel })
    }

    /// Socket path the client is dialing. Useful for
    /// diagnostics.
    #[must_use]
    pub fn socket(&self) -> &PathBuf {
        &self.socket
    }
}

#[async_trait]
impl NarcEscalator for NarcGrpcUds {
    async fn judge(&self, request: &JudgeRequest) -> JudgeDecision {
        let pb_req: smooth_narc::pb::JudgeRequest = request.clone().into();
        let mut client = smooth_narc::pb::narc_client::NarcClient::new(self.channel.clone());
        let resp = match client.judge(tonic::Request::new(pb_req)).await {
            Ok(resp) => resp.into_inner(),
            Err(status) => {
                return JudgeDecision::escalate(format!("Narc gRPC at {} unreachable: {status}", self.socket.display()));
            }
        };
        smooth_narc::judge::JudgeDecision::try_from(resp).unwrap_or_else(|e| JudgeDecision::escalate(format!("failed to decode Narc response: {e}")))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use smooth_narc::judge::{Decision, JudgeKind};
    use std::time::Duration;
    use tempfile::TempDir;

    fn req(domain: &str) -> JudgeRequest {
        JudgeRequest {
            kind: JudgeKind::Network,
            operator_id: "op-1".into(),
            bead_id: "pearl".into(),
            phase: "execute".into(),
            resource: domain.into(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        }
    }

    /// Spin a real smooth_narc gRPC server backed by a
    /// stub Judge so we exercise the full UDS dial path.
    struct ApprovingJudge;

    #[async_trait]
    impl smooth_narc::grpc::Judge for ApprovingJudge {
        async fn judge(&self, _request: JudgeRequest) -> JudgeDecision {
            JudgeDecision::approve("test approval")
        }
    }

    #[tokio::test]
    async fn judge_round_trips_over_uds() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");
        let _server = smooth_narc::grpc::serve_uds(std::sync::Arc::new(ApprovingJudge), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;

        let client = NarcGrpcUds::connect(sock.clone()).await.unwrap();
        assert_eq!(client.socket(), &sock);
        let decision = client.judge(&req("registry.npmjs.org")).await;
        assert_eq!(decision.decision, Decision::Approve);
    }

    #[tokio::test]
    async fn dead_socket_after_connect_folds_to_escalate() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("narc.sock");
        let server = smooth_narc::grpc::serve_uds(std::sync::Arc::new(ApprovingJudge), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;

        let client = NarcGrpcUds::connect(sock.clone()).await.unwrap();
        server.abort();
        let _ = std::fs::remove_file(&sock);
        tokio::time::sleep(Duration::from_millis(30)).await;

        let decision = client.judge(&req("x.example")).await;
        assert_eq!(decision.decision, Decision::EscalateToHuman);
        assert!(decision.reason.contains("unreachable"));
    }

    #[tokio::test]
    async fn connect_to_missing_socket_errors() {
        let tmp = TempDir::new().unwrap();
        let result = NarcGrpcUds::connect(tmp.path().join("missing.sock")).await;
        assert!(result.is_err(), "missing socket should error");
        let err = result.err().unwrap();
        assert!(err.to_string().contains("connect UDS at"));
    }
}

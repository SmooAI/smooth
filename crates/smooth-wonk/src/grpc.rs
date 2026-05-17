//! Tonic gRPC server adapter for the Wonk service.
//!
//! Wraps a `Checker` trait implementation as the proto-generated
//! Wonk service. Production Checker is the existing AppState (this
//! lands in iter-3 when we wire the server up at startup); iter-2
//! ships the wire layer + a stub Checker for round-trip tests.
//!
//! Pearl th-893801 iter-2.

use crate::pb;
use async_trait::async_trait;
use std::sync::Arc;

/// What the gRPC adapter needs from the underlying Wonk logic. Each
/// method returns the local-policy verdict; escalation to Narc
/// happens upstream (the Checker impl can call NarcClient itself).
///
/// `was_escalated` and `resolved_scope` in the response let the
/// caller distinguish "policy auto-approved" from "Narc judged this
/// and a human responded at scope X" — important for log + UI.
#[async_trait]
pub trait Checker: Send + Sync + 'static {
    async fn check_network(&self, req: NetworkReq) -> Verdict;
    async fn check_tool(&self, req: ToolReq) -> Verdict;
    async fn check_cli(&self, req: CliReq) -> Verdict;
    async fn check_file(&self, req: FileReq) -> Verdict;
    async fn reload_policy(&self) -> ReloadResult {
        // Default impl: not supported.
        ReloadResult {
            reloaded: false,
            error: "reload not implemented for this Checker".into(),
            network_allow_hosts: 0,
            tools_allow: 0,
            bash_allow_patterns: 0,
        }
    }
    async fn policy_summary(&self) -> PolicySummary {
        PolicySummary::default()
    }
}

/// Domain-level request shapes — kept independent of the proto types
/// so the trait can be implemented by code that doesn't depend on
/// the proto crate.
pub struct NetworkReq {
    pub domain: String,
    pub path: String,
    pub method: String,
    pub operator_id: String,
    pub bead_id: String,
}

pub struct ToolReq {
    pub tool_name: String,
    pub operator_id: String,
    pub bead_id: String,
}

pub struct CliReq {
    pub command: String,
    pub cwd: String,
    pub operator_id: String,
    pub bead_id: String,
}

pub struct FileReq {
    pub path: String,
    pub access: FileAccess,
    pub operator_id: String,
    pub bead_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccess {
    Read,
    Write,
    Execute,
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub allowed: bool,
    pub reason: String,
    pub was_escalated: bool,
    /// Set when the verdict came from a human resolution (Narc Ask
    /// path). `None` for direct policy decisions.
    pub resolved_scope: Option<smooth_narc::judge::Scope>,
}

#[derive(Debug, Clone, Default)]
pub struct ReloadResult {
    pub reloaded: bool,
    pub error: String,
    pub network_allow_hosts: u32,
    pub tools_allow: u32,
    pub bash_allow_patterns: u32,
}

#[derive(Debug, Clone, Default)]
pub struct PolicySummary {
    pub allow_hosts: Vec<String>,
    pub allow_tools: Vec<String>,
    pub allow_bash_patterns: Vec<String>,
    pub runtime_allowlist_size: u32,
    pub user_grants_path: String,
    pub project_grants_path: String,
}

/// Tonic-facing wrapper around a `Checker` implementation.
pub struct WonkGrpcServer<C: Checker> {
    checker: Arc<C>,
}

impl<C: Checker> WonkGrpcServer<C> {
    pub fn new(checker: Arc<C>) -> Self {
        Self { checker }
    }
}

/// Encode a `Verdict` into the proto response. Centralises the
/// `resolved_scope` mapping so all four check RPCs handle it the
/// same way.
fn verdict_to_pb(v: Verdict) -> pb::CheckResponse {
    let resolved_scope = v.resolved_scope.map_or(smooth_narc::pb::Scope::Unspecified as i32, |s| {
        let pb_s: smooth_narc::pb::Scope = s.into();
        pb_s as i32
    });
    pb::CheckResponse {
        allowed: v.allowed,
        reason: v.reason,
        was_escalated: v.was_escalated,
        resolved_scope,
    }
}

#[async_trait]
impl<C: Checker> pb::wonk_server::Wonk for WonkGrpcServer<C> {
    async fn check_network(&self, request: tonic::Request<pb::CheckNetworkRequest>) -> Result<tonic::Response<pb::CheckResponse>, tonic::Status> {
        let r = request.into_inner();
        let v = self
            .checker
            .check_network(NetworkReq {
                domain: r.domain,
                path: r.path,
                method: r.method,
                operator_id: r.operator_id,
                bead_id: r.bead_id,
            })
            .await;
        Ok(tonic::Response::new(verdict_to_pb(v)))
    }

    async fn check_tool(&self, request: tonic::Request<pb::CheckToolRequest>) -> Result<tonic::Response<pb::CheckResponse>, tonic::Status> {
        let r = request.into_inner();
        let v = self
            .checker
            .check_tool(ToolReq {
                tool_name: r.tool_name,
                operator_id: r.operator_id,
                bead_id: r.bead_id,
            })
            .await;
        Ok(tonic::Response::new(verdict_to_pb(v)))
    }

    async fn check_cli(&self, request: tonic::Request<pb::CheckCliRequest>) -> Result<tonic::Response<pb::CheckResponse>, tonic::Status> {
        let r = request.into_inner();
        let v = self
            .checker
            .check_cli(CliReq {
                command: r.command,
                cwd: r.cwd,
                operator_id: r.operator_id,
                bead_id: r.bead_id,
            })
            .await;
        Ok(tonic::Response::new(verdict_to_pb(v)))
    }

    async fn check_file(&self, request: tonic::Request<pb::CheckFileRequest>) -> Result<tonic::Response<pb::CheckResponse>, tonic::Status> {
        let r = request.into_inner();
        let access = match pb::AccessKind::try_from(r.access).unwrap_or(pb::AccessKind::Unspecified) {
            pb::AccessKind::Read => FileAccess::Read,
            pb::AccessKind::Write => FileAccess::Write,
            pb::AccessKind::Execute => FileAccess::Execute,
            pb::AccessKind::Unspecified => {
                return Err(tonic::Status::invalid_argument("AccessKind::Unspecified not allowed"));
            }
        };
        let v = self
            .checker
            .check_file(FileReq {
                path: r.path,
                access,
                operator_id: r.operator_id,
                bead_id: r.bead_id,
            })
            .await;
        Ok(tonic::Response::new(verdict_to_pb(v)))
    }

    async fn reload_policy(&self, _request: tonic::Request<pb::ReloadPolicyRequest>) -> Result<tonic::Response<pb::ReloadPolicyResponse>, tonic::Status> {
        let r = self.checker.reload_policy().await;
        Ok(tonic::Response::new(pb::ReloadPolicyResponse {
            reloaded: r.reloaded,
            error: r.error,
            network_allow_hosts: r.network_allow_hosts,
            tools_allow: r.tools_allow,
            bash_allow_patterns: r.bash_allow_patterns,
        }))
    }

    async fn get_policy_summary(&self, _request: tonic::Request<pb::GetPolicySummaryRequest>) -> Result<tonic::Response<pb::PolicySummary>, tonic::Status> {
        let s = self.checker.policy_summary().await;
        Ok(tonic::Response::new(pb::PolicySummary {
            allow_hosts: s.allow_hosts,
            allow_tools: s.allow_tools,
            allow_bash_patterns: s.allow_bash_patterns,
            runtime_allowlist_size: s.runtime_allowlist_size,
            user_grants_path: s.user_grants_path,
            project_grants_path: s.project_grants_path,
        }))
    }
}

/// Spawn a Wonk gRPC server on a UDS. Mirrors smooth_narc's
/// `serve_uds` shape. Sync — spawns the server task and returns
/// its JoinHandle.
///
/// # Errors
///
/// Returns the underlying io::Error if binding the UDS fails.
pub fn serve_uds<C: Checker>(checker: Arc<C>, uds_path: std::path::PathBuf) -> std::io::Result<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>> {
    let _ = std::fs::remove_file(&uds_path);
    let uds = tokio::net::UnixListener::bind(&uds_path)?;
    let svc = pb::wonk_server::WonkServer::new(WonkGrpcServer::new(checker));
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

    /// Stub Checker — allows tool="grep", denies everything else.
    struct StubChecker;

    #[async_trait]
    impl Checker for StubChecker {
        async fn check_network(&self, req: NetworkReq) -> Verdict {
            if req.domain == "registry.npmjs.org" {
                Verdict {
                    allowed: true,
                    reason: "static allowlist".into(),
                    was_escalated: false,
                    resolved_scope: None,
                }
            } else {
                Verdict {
                    allowed: false,
                    reason: format!("{} not allowed", req.domain),
                    was_escalated: false,
                    resolved_scope: None,
                }
            }
        }
        async fn check_tool(&self, req: ToolReq) -> Verdict {
            Verdict {
                allowed: req.tool_name == "grep",
                reason: if req.tool_name == "grep" { "ok" } else { "denied" }.into(),
                was_escalated: false,
                resolved_scope: None,
            }
        }
        async fn check_cli(&self, _req: CliReq) -> Verdict {
            Verdict {
                allowed: true,
                reason: "stub: cli always ok".into(),
                was_escalated: false,
                resolved_scope: None,
            }
        }
        async fn check_file(&self, req: FileReq) -> Verdict {
            Verdict {
                allowed: req.access == FileAccess::Read,
                reason: format!("file access={:?}", req.access),
                was_escalated: false,
                resolved_scope: None,
            }
        }
    }

    async fn build_uds_client(uds_path: std::path::PathBuf) -> pb::wonk_client::WonkClient<tonic::transport::Channel> {
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
        pb::wonk_client::WonkClient::new(channel)
    }

    #[tokio::test]
    async fn check_network_allowed_for_safe_domain() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let _server = serve_uds(Arc::new(StubChecker), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let resp = client
            .check_network(tonic::Request::new(pb::CheckNetworkRequest {
                domain: "registry.npmjs.org".into(),
                path: "/foo".into(),
                method: "GET".into(),
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.allowed);
        assert!(!resp.was_escalated);
    }

    #[tokio::test]
    async fn check_network_denied_for_unknown_domain() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let _server = serve_uds(Arc::new(StubChecker), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let resp = client
            .check_network(tonic::Request::new(pb::CheckNetworkRequest {
                domain: "attacker.example".into(),
                path: "/".into(),
                method: "GET".into(),
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.allowed);
        assert!(resp.reason.contains("attacker.example"));
    }

    #[tokio::test]
    async fn check_tool_round_trips() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let _server = serve_uds(Arc::new(StubChecker), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let allow = client
            .check_tool(tonic::Request::new(pb::CheckToolRequest {
                tool_name: "grep".into(),
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(allow.allowed);

        let deny = client
            .check_tool(tonic::Request::new(pb::CheckToolRequest {
                tool_name: "rm".into(),
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!deny.allowed);
    }

    #[tokio::test]
    async fn check_file_unspecified_access_is_invalid_argument() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let _server = serve_uds(Arc::new(StubChecker), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let err = client
            .check_file(tonic::Request::new(pb::CheckFileRequest {
                path: "/x".into(),
                access: pb::AccessKind::Unspecified as i32,
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .expect_err("status error");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn resolved_scope_flows_through_when_present() {
        struct EscalatingChecker;
        #[async_trait]
        impl Checker for EscalatingChecker {
            async fn check_network(&self, _req: NetworkReq) -> Verdict {
                Verdict {
                    allowed: true,
                    reason: "human approved at scope=session".into(),
                    was_escalated: true,
                    resolved_scope: Some(smooth_narc::judge::Scope::Session),
                }
            }
            async fn check_tool(&self, _req: ToolReq) -> Verdict {
                unreachable!()
            }
            async fn check_cli(&self, _req: CliReq) -> Verdict {
                unreachable!()
            }
            async fn check_file(&self, _req: FileReq) -> Verdict {
                unreachable!()
            }
        }

        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let _server = serve_uds(Arc::new(EscalatingChecker), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let resp = client
            .check_network(tonic::Request::new(pb::CheckNetworkRequest {
                domain: "anywhere".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.allowed);
        assert!(resp.was_escalated);
        assert_eq!(resp.resolved_scope, smooth_narc::pb::Scope::Session as i32);
    }
}

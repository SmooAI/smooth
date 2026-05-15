//! Tonic gRPC server adapter for the HostStub service.
//!
//! Routes `IssueCredential` to the `BackendRegistry` and
//! `GetCredentialBackends` to a snapshot of registered backends.
//! Pearl th-893801 Phase 2 iter-4a.

use std::path::PathBuf;
use std::sync::Arc;

use crate::backend::CredentialRequest;
use crate::pb;
use crate::registry::BackendRegistry;

/// Tonic-facing wrapper around a `BackendRegistry`.
pub struct HostStubServer {
    registry: Arc<BackendRegistry>,
}

impl HostStubServer {
    pub fn new(registry: Arc<BackendRegistry>) -> Self {
        Self { registry }
    }

    /// Borrow the underlying registry.
    pub fn registry(&self) -> &BackendRegistry {
        &self.registry
    }
}

#[tonic::async_trait]
impl pb::host_stub_server::HostStub for HostStubServer {
    async fn issue_credential(
        &self,
        request: tonic::Request<pb::IssueCredentialRequest>,
    ) -> Result<tonic::Response<pb::IssueCredentialResponse>, tonic::Status> {
        let req = request.into_inner();
        let scope_hint = pb::ScopeHint::try_from(req.scope_hint).unwrap_or(pb::ScopeHint::Unspecified).into();
        let cred_req = CredentialRequest {
            server_url: req.server_url,
            scope_hint,
            operator_id: req.operator_id,
            bead_id: req.bead_id,
        };
        let issued = self.registry.issue(&cred_req).await.map_err(crate::backend::BackendError::into_status)?;
        let expires_at = issued.expires_at.map(|ts| prost_types::Timestamp {
            seconds: ts.timestamp(),
            nanos: i32::try_from(ts.timestamp_subsec_nanos()).unwrap_or(0),
        });
        Ok(tonic::Response::new(pb::IssueCredentialResponse {
            username: issued.username,
            secret: issued.secret,
            expires_at,
            backend: issued.backend,
        }))
    }

    async fn get_credential_backends(
        &self,
        _request: tonic::Request<pb::GetCredentialBackendsRequest>,
    ) -> Result<tonic::Response<pb::CredentialBackends>, tonic::Status> {
        let backends = self
            .registry
            .list_backends()
            .into_iter()
            .map(|info| pb::Backend {
                name: info.name,
                server_globs: info.server_globs,
                ready: info.ready,
                status: info.status,
            })
            .collect();
        Ok(tonic::Response::new(pb::CredentialBackends { backends }))
    }
}

/// Bind the host-stub gRPC server on a UDS and spawn the server
/// task. The caller is responsible for ensuring the parent
/// directory exists and is mounted into the sandbox.
///
/// Returns the spawned task's join handle. The socket file is
/// removed before bind so a previous stale socket doesn't block
/// startup.
///
/// Sync (only `spawn`s).
///
/// # Errors
///
/// Returns the underlying io::Error if binding the UDS fails.
pub fn serve_uds(registry: Arc<BackendRegistry>, uds_path: PathBuf) -> std::io::Result<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>> {
    let _ = std::fs::remove_file(&uds_path);
    let uds = tokio::net::UnixListener::bind(uds_path)?;
    let svc = pb::host_stub_server::HostStubServer::new(HostStubServer::new(registry));
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
    use crate::backend::{Backend, BackendInfo, IssuedCredential};
    use async_trait::async_trait;
    use std::time::Duration;
    use tempfile::TempDir;
    use tower::service_fn;

    struct OkBackend;

    #[async_trait]
    impl Backend for OkBackend {
        fn info(&self) -> BackendInfo {
            BackendInfo {
                name: "gh".into(),
                server_globs: vec!["github.com".into(), "*.github.com".into()],
                ready: true,
                status: "ok".into(),
            }
        }
        async fn issue(&self, _request: &CredentialRequest) -> Result<IssuedCredential, crate::backend::BackendError> {
            Ok(IssuedCredential {
                username: "smooth".into(),
                secret: "gho_test".into(),
                expires_at: None,
                backend: "gh".into(),
            })
        }
    }

    async fn build_client(uds_path: PathBuf) -> pb::host_stub_client::HostStubClient<tonic::transport::Channel> {
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
        pb::host_stub_client::HostStubClient::new(channel)
    }

    #[tokio::test]
    async fn issue_credential_round_trips_over_uds() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("host.sock");
        let registry = Arc::new(BackendRegistry::new().with_backend(Arc::new(OkBackend)));
        let _server = serve_uds(registry, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client
            .issue_credential(tonic::Request::new(pb::IssueCredentialRequest {
                server_url: "https://api.github.com".into(),
                scope_hint: pb::ScopeHint::Read as i32,
                operator_id: "op-1".into(),
                bead_id: "pearl-1".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.username, "smooth");
        assert_eq!(resp.secret, "gho_test");
        assert_eq!(resp.backend, "gh");
    }

    #[tokio::test]
    async fn unknown_server_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("host.sock");
        let registry = Arc::new(BackendRegistry::new().with_backend(Arc::new(OkBackend)));
        let _server = serve_uds(registry, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let err = client
            .issue_credential(tonic::Request::new(pb::IssueCredentialRequest {
                server_url: "registry.npmjs.org".into(),
                scope_hint: pb::ScopeHint::Unspecified as i32,
                operator_id: String::new(),
                bead_id: String::new(),
            }))
            .await
            .expect_err("npm should not match a gh backend");
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn empty_server_url_is_invalid() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("host.sock");
        let registry = Arc::new(BackendRegistry::new().with_backend(Arc::new(OkBackend)));
        let _server = serve_uds(registry, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let err = client
            .issue_credential(tonic::Request::new(pb::IssueCredentialRequest {
                server_url: String::new(),
                scope_hint: pb::ScopeHint::Unspecified as i32,
                operator_id: String::new(),
                bead_id: String::new(),
            }))
            .await
            .expect_err("empty URL should error");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn list_backends_reports_registered_set() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("host.sock");
        let registry = Arc::new(BackendRegistry::new().with_backend(Arc::new(OkBackend)));
        let _server = serve_uds(registry, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client
            .get_credential_backends(tonic::Request::new(pb::GetCredentialBackendsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.backends.len(), 1);
        assert_eq!(resp.backends[0].name, "gh");
        assert!(resp.backends[0].ready);
        assert_eq!(resp.backends[0].server_globs, vec!["github.com".to_string(), "*.github.com".to_string()]);
    }
}

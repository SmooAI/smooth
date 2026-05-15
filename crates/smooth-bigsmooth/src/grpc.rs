//! Tonic gRPC server adapter for the BigSmooth service.
//!
//! BigSmooth's gRPC surface covers dispatch + AccessStore + the
//! AccessEvents / OperatorEvents server-streams. Iter-2 ships the
//! wire layer; production wiring (against the existing AppState +
//! AccessStore + orchestrator) lands in iter-3 once we're ready
//! to flip the SMOOTH_SINGLE_PROCESS flag.
//!
//! Pearl th-893801 iter-2.

use crate::pb;
use async_trait::async_trait;
use futures_util::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

/// What the gRPC adapter needs to be able to do. Each method maps
/// directly to one of the BigSmooth proto RPCs.
#[async_trait]
pub trait Orchestrator: Send + Sync + 'static {
    // Dispatch
    async fn dispatch(&self, req: pb::DispatchRequest) -> Result<pb::DispatchResponse, tonic::Status>;
    async fn cancel(&self, req: pb::CancelRequest) -> Result<pb::CancelResponse, tonic::Status>;
    async fn list_operators(&self, req: pb::ListOperatorsRequest) -> pb::ListOperatorsResponse;

    // AccessStore
    async fn file_pending_access(&self, req: pb::FilePendingAccessRequest) -> pb::FilePendingAccessResponse;
    async fn resolve_access(&self, req: pb::ResolveAccessRequest) -> Result<pb::ResolveAccessResponse, tonic::Status>;
    async fn list_pending_access(&self) -> pb::ListPendingAccessResponse;
    async fn subscribe_access_events(&self, tx: mpsc::Sender<pb::AccessEvent>);

    // Operator events
    async fn subscribe_operator_events(&self, operator_filter: String, tx: mpsc::Sender<pb::OperatorEvent>);
}

/// Tonic-facing wrapper.
pub struct BigSmoothGrpcServer<O: Orchestrator> {
    orchestrator: Arc<O>,
}

impl<O: Orchestrator> BigSmoothGrpcServer<O> {
    pub fn new(orchestrator: Arc<O>) -> Self {
        Self { orchestrator }
    }
}

#[async_trait]
impl<O: Orchestrator> pb::big_smooth_server::BigSmooth for BigSmoothGrpcServer<O> {
    async fn dispatch(&self, request: tonic::Request<pb::DispatchRequest>) -> Result<tonic::Response<pb::DispatchResponse>, tonic::Status> {
        let r = self.orchestrator.dispatch(request.into_inner()).await?;
        Ok(tonic::Response::new(r))
    }

    async fn cancel(&self, request: tonic::Request<pb::CancelRequest>) -> Result<tonic::Response<pb::CancelResponse>, tonic::Status> {
        let r = self.orchestrator.cancel(request.into_inner()).await?;
        Ok(tonic::Response::new(r))
    }

    async fn list_operators(&self, request: tonic::Request<pb::ListOperatorsRequest>) -> Result<tonic::Response<pb::ListOperatorsResponse>, tonic::Status> {
        let r = self.orchestrator.list_operators(request.into_inner()).await;
        Ok(tonic::Response::new(r))
    }

    async fn file_pending_access(
        &self,
        request: tonic::Request<pb::FilePendingAccessRequest>,
    ) -> Result<tonic::Response<pb::FilePendingAccessResponse>, tonic::Status> {
        let r = self.orchestrator.file_pending_access(request.into_inner()).await;
        Ok(tonic::Response::new(r))
    }

    async fn resolve_access(&self, request: tonic::Request<pb::ResolveAccessRequest>) -> Result<tonic::Response<pb::ResolveAccessResponse>, tonic::Status> {
        let r = self.orchestrator.resolve_access(request.into_inner()).await?;
        Ok(tonic::Response::new(r))
    }

    async fn list_pending_access(
        &self,
        _request: tonic::Request<pb::ListPendingAccessRequest>,
    ) -> Result<tonic::Response<pb::ListPendingAccessResponse>, tonic::Status> {
        let r = self.orchestrator.list_pending_access().await;
        Ok(tonic::Response::new(r))
    }

    type SubscribeAccessEventsStream = Pin<Box<dyn Stream<Item = Result<pb::AccessEvent, tonic::Status>> + Send + 'static>>;

    async fn subscribe_access_events(
        &self,
        _request: tonic::Request<pb::SubscribeAccessEventsRequest>,
    ) -> Result<tonic::Response<Self::SubscribeAccessEventsStream>, tonic::Status> {
        let (tx, rx) = mpsc::channel(64);
        let orch = self.orchestrator.clone();
        tokio::spawn(async move {
            orch.subscribe_access_events(tx).await;
        });
        let stream: Self::SubscribeAccessEventsStream = Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok));
        Ok(tonic::Response::new(stream))
    }

    type SubscribeOperatorEventsStream = Pin<Box<dyn Stream<Item = Result<pb::OperatorEvent, tonic::Status>> + Send + 'static>>;

    async fn subscribe_operator_events(
        &self,
        request: tonic::Request<pb::SubscribeOperatorEventsRequest>,
    ) -> Result<tonic::Response<Self::SubscribeOperatorEventsStream>, tonic::Status> {
        let operator_filter = request.into_inner().operator_id;
        let (tx, rx) = mpsc::channel(64);
        let orch = self.orchestrator.clone();
        tokio::spawn(async move {
            orch.subscribe_operator_events(operator_filter, tx).await;
        });
        let stream: Self::SubscribeOperatorEventsStream = Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok));
        Ok(tonic::Response::new(stream))
    }
}

// `.map()` is on tokio_stream's StreamExt.
use tokio_stream::StreamExt;

/// Spawn a BigSmooth gRPC server on a UDS.
///
/// # Errors
///
/// Returns the underlying io::Error if binding the UDS fails.
pub fn serve_uds<O: Orchestrator>(
    orchestrator: Arc<O>,
    uds_path: std::path::PathBuf,
) -> std::io::Result<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>> {
    let _ = std::fs::remove_file(&uds_path);
    let uds = tokio::net::UnixListener::bind(&uds_path)?;
    let svc = pb::big_smooth_server::BigSmoothServer::new(BigSmoothGrpcServer::new(orchestrator));
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

    /// Stub orchestrator — minimal behavior for round-trip tests.
    #[derive(Default)]
    struct StubOrchestrator {
        dispatches: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl Orchestrator for StubOrchestrator {
        async fn dispatch(&self, _req: pb::DispatchRequest) -> Result<pb::DispatchResponse, tonic::Status> {
            let n = self.dispatches.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(pb::DispatchResponse {
                operator_id: format!("op-{n}"),
                started_at: None,
            })
        }
        async fn cancel(&self, _req: pb::CancelRequest) -> Result<pb::CancelResponse, tonic::Status> {
            Ok(pb::CancelResponse { was_running: true })
        }
        async fn list_operators(&self, _req: pb::ListOperatorsRequest) -> pb::ListOperatorsResponse {
            pb::ListOperatorsResponse { operators: vec![] }
        }
        async fn file_pending_access(&self, _req: pb::FilePendingAccessRequest) -> pb::FilePendingAccessResponse {
            pb::FilePendingAccessResponse {
                id: "stub-id".into(),
                created_at: None,
            }
        }
        async fn resolve_access(&self, _req: pb::ResolveAccessRequest) -> Result<pb::ResolveAccessResponse, tonic::Status> {
            Ok(pb::ResolveAccessResponse { resolved_at: None })
        }
        async fn list_pending_access(&self) -> pb::ListPendingAccessResponse {
            pb::ListPendingAccessResponse { pending: vec![] }
        }
        async fn subscribe_access_events(&self, tx: mpsc::Sender<pb::AccessEvent>) {
            // Emit one synthetic Expired event so the test has
            // something to receive, then close.
            let _ = tx
                .send(pb::AccessEvent {
                    event: Some(pb::access_event::Event::Expired(pb::AccessExpiration {
                        id: "stub-id".into(),
                        expired_at: None,
                    })),
                })
                .await;
        }
        async fn subscribe_operator_events(&self, _operator_filter: String, tx: mpsc::Sender<pb::OperatorEvent>) {
            let _ = tx
                .send(pb::OperatorEvent {
                    operator_id: "op-1".into(),
                    timestamp: None,
                    event: Some(pb::operator_event::Event::TokenDelta(pb::TokenDelta { content: "hi".into() })),
                })
                .await;
        }
    }

    async fn build_uds_client(uds_path: std::path::PathBuf) -> pb::big_smooth_client::BigSmoothClient<tonic::transport::Channel> {
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
        pb::big_smooth_client::BigSmoothClient::new(channel)
    }

    #[tokio::test]
    async fn dispatch_round_trips() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let _server = serve_uds(Arc::new(StubOrchestrator::default()), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let r = client
            .dispatch(tonic::Request::new(pb::DispatchRequest {
                bead_id: "pearl-1".into(),
                task_message: "hi".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(r.operator_id, "op-0");
    }

    #[tokio::test]
    async fn access_store_round_trips() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let _server = serve_uds(Arc::new(StubOrchestrator::default()), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let r = client
            .file_pending_access(tonic::Request::new(pb::FilePendingAccessRequest {
                kind: smooth_narc::pb::JudgeKind::Network as i32,
                operator_id: "op".into(),
                bead_id: "pearl".into(),
                resource: "api.example.com".into(),
                detail: String::new(),
                reason: "test".into(),
                scope_options: vec![],
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(r.id, "stub-id");

        let resolved = client
            .resolve_access(tonic::Request::new(pb::ResolveAccessRequest {
                id: "stub-id".into(),
                verdict: pb::Verdict::Approve as i32,
                scope: smooth_narc::pb::Scope::Once as i32,
                glob_override: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();
        // Stub returns default Timestamp (None); just check the call succeeded.
        assert!(resolved.resolved_at.is_none());

        let list = client
            .list_pending_access(tonic::Request::new(pb::ListPendingAccessRequest::default()))
            .await
            .unwrap()
            .into_inner();
        assert!(list.pending.is_empty());
    }

    #[tokio::test]
    async fn subscribe_access_events_streams() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let _server = serve_uds(Arc::new(StubOrchestrator::default()), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let mut stream = client
            .subscribe_access_events(tonic::Request::new(pb::SubscribeAccessEventsRequest::default()))
            .await
            .unwrap()
            .into_inner();

        let evt = tokio::time::timeout(Duration::from_millis(200), StreamExt::next(&mut stream))
            .await
            .expect("got an event")
            .expect("stream item")
            .expect("ok status");
        match evt.event {
            Some(pb::access_event::Event::Expired(e)) => assert_eq!(e.id, "stub-id"),
            other => panic!("expected Expired, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscribe_operator_events_streams() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let _server = serve_uds(Arc::new(StubOrchestrator::default()), sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let mut stream = client
            .subscribe_operator_events(tonic::Request::new(pb::SubscribeOperatorEventsRequest { operator_id: String::new() }))
            .await
            .unwrap()
            .into_inner();
        let evt = tokio::time::timeout(Duration::from_millis(200), StreamExt::next(&mut stream))
            .await
            .expect("got an event")
            .expect("stream item")
            .expect("ok status");
        assert_eq!(evt.operator_id, "op-1");
        match evt.event {
            Some(pb::operator_event::Event::TokenDelta(d)) => assert_eq!(d.content, "hi"),
            other => panic!("expected TokenDelta, got {other:?}"),
        }
    }
}

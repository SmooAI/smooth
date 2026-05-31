//! Production wiring of the `BigSmooth` gRPC `Orchestrator` trait.
//!
//! Pearl th-893801 iter-3d. The AccessStore-related RPCs
//! (`FilePendingAccess`, `ResolveAccess`, `ListPendingAccess`,
//! `SubscribeAccessEvents`) are fully wired against the existing
//! `crate::access::AccessStore` — same semantics as the
//! `/api/access/*` HTTP routes.
//!
//! Dispatch / Cancel / ListOperators / SubscribeOperatorEvents
//! are stubbed (`Unimplemented`) for this iter — they need the
//! orchestrator-loop integration that lands in Phase 2 when
//! `th up` boots the sandbox. Stubbed RPCs return `Unimplemented`
//! with a clear pointer to the pearl so callers fail fast rather
//! than silently dropping work.
//!
//! When iter-3e flips `SMOOTH_SINGLE_PROCESS=1`, BS spawns
//! [`OrchestratorAdapter`] over a UDS at a known path
//! (`$XDG_RUNTIME_DIR/smooth/bigsmooth.sock`) so Narc-side calls
//! to `FilePendingAccess` land here instead of crossing the
//! legacy HTTP edge.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::access::{AccessEvent as DomainEvent, AccessStore, NewAccessRequest, ResolutionVerdict};
use crate::grpc::Orchestrator;
use crate::pb;
use smooth_narc::judge::Scope;

/// gRPC-facing adapter over the live AccessStore.
///
/// Holds an `Arc<AccessStore>` so the adapter can be cheaply
/// cloned and handed to the tonic server alongside other state.
/// Iter-3e will extend this with the orchestrator + teammates
/// handles once dispatch wiring is in.
pub struct OrchestratorAdapter {
    access: AccessStore,
}

impl OrchestratorAdapter {
    /// Build an adapter from a live `AccessStore`.
    pub fn new(access: AccessStore) -> Self {
        Self { access }
    }

    /// Borrow the underlying access store. Tests use this to
    /// inspect / mutate state outside the gRPC path.
    pub fn access(&self) -> &AccessStore {
        &self.access
    }
}

// ── conversions ─────────────────────────────────────────────

// `tonic::Status` is intentionally large (carries trailers,
// metadata, etc.). Boxing it just to satisfy result_large_err
// here would hurt the gRPC server's hot path. Allow it.
#[allow(clippy::result_large_err)]
fn judge_kind_from_pb(kind: i32) -> Result<&'static str, tonic::Status> {
    match smooth_narc::pb::JudgeKind::try_from(kind) {
        Ok(smooth_narc::pb::JudgeKind::Network) => Ok("network"),
        Ok(smooth_narc::pb::JudgeKind::Tool) => Ok("tool"),
        Ok(smooth_narc::pb::JudgeKind::File) => Ok("file"),
        Ok(smooth_narc::pb::JudgeKind::Cli) => Ok("cli"),
        Ok(smooth_narc::pb::JudgeKind::Mcp) => Ok("mcp"),
        Ok(smooth_narc::pb::JudgeKind::Port) => Ok("port"),
        Ok(smooth_narc::pb::JudgeKind::Unspecified) | Err(_) => {
            Err(tonic::Status::invalid_argument("judge_kind must be one of network/tool/file/cli/mcp/port"))
        }
    }
}

fn judge_kind_to_pb(kind: &str) -> i32 {
    let pb_kind = match kind {
        "network" => smooth_narc::pb::JudgeKind::Network,
        "tool" => smooth_narc::pb::JudgeKind::Tool,
        "file" => smooth_narc::pb::JudgeKind::File,
        "cli" => smooth_narc::pb::JudgeKind::Cli,
        "mcp" => smooth_narc::pb::JudgeKind::Mcp,
        "port" => smooth_narc::pb::JudgeKind::Port,
        _ => smooth_narc::pb::JudgeKind::Unspecified,
    };
    pb_kind as i32
}

fn scope_from_pb(scope: i32) -> Option<Scope> {
    match smooth_narc::pb::Scope::try_from(scope).ok()? {
        smooth_narc::pb::Scope::Once => Some(Scope::Once),
        smooth_narc::pb::Scope::Session => Some(Scope::Session),
        smooth_narc::pb::Scope::PearlProject => Some(Scope::PearlProject),
        smooth_narc::pb::Scope::User => Some(Scope::User),
        smooth_narc::pb::Scope::Unspecified => None,
    }
}

fn scope_to_pb(scope: Scope) -> i32 {
    let pb_scope = match scope {
        Scope::Once => smooth_narc::pb::Scope::Once,
        Scope::Session => smooth_narc::pb::Scope::Session,
        Scope::PearlProject => smooth_narc::pb::Scope::PearlProject,
        Scope::User => smooth_narc::pb::Scope::User,
    };
    pb_scope as i32
}

fn ts_to_pb(ts: DateTime<Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: ts.timestamp(),
        nanos: i32::try_from(ts.timestamp_subsec_nanos()).unwrap_or(0),
    }
}

fn pending_to_pb(req: &crate::access::PendingAccessRequest) -> pb::PendingAccess {
    pb::PendingAccess {
        id: req.id.clone(),
        kind: judge_kind_to_pb(&req.kind),
        operator_id: req.operator_id.clone(),
        bead_id: req.bead_id.clone(),
        resource: req.resource.clone(),
        detail: req.detail.clone().unwrap_or_default(),
        reason: req.reason.clone(),
        scope_options: req.scope_options.iter().map(|s| scope_to_pb(*s)).collect(),
        created_at: Some(ts_to_pb(req.created_at)),
    }
}

fn resolution_to_pb(resolution: &crate::access::AccessResolution) -> pb::AccessResolution {
    pb::AccessResolution {
        id: resolution.id.clone(),
        verdict: match resolution.verdict {
            ResolutionVerdict::Approve => pb::Verdict::Approve as i32,
            ResolutionVerdict::Deny => pb::Verdict::Deny as i32,
        },
        scope: scope_to_pb(resolution.scope),
        glob_override: resolution.glob_override.clone().unwrap_or_default(),
        resolved_at: Some(ts_to_pb(resolution.resolved_at)),
    }
}

fn domain_event_to_pb(event: DomainEvent) -> pb::AccessEvent {
    let inner = match event {
        DomainEvent::Pending(req) => pb::access_event::Event::Pending(pending_to_pb(&req)),
        DomainEvent::Resolved(resolution) => pb::access_event::Event::Resolved(resolution_to_pb(&resolution)),
        DomainEvent::Expired { id, expired_at } => pb::access_event::Event::Expired(pb::AccessExpiration {
            id,
            expired_at: Some(ts_to_pb(expired_at)),
        }),
    };
    pb::AccessEvent { event: Some(inner) }
}

#[async_trait]
impl Orchestrator for OrchestratorAdapter {
    async fn dispatch(&self, _req: pb::DispatchRequest) -> Result<pb::DispatchResponse, tonic::Status> {
        Err(tonic::Status::unimplemented("Dispatch wiring lands in phase 2 (pearl th-ea2aa5)"))
    }

    async fn cancel(&self, _req: pb::CancelRequest) -> Result<pb::CancelResponse, tonic::Status> {
        Err(tonic::Status::unimplemented("Cancel wiring lands in phase 2 (pearl th-ea2aa5)"))
    }

    async fn list_operators(&self, _req: pb::ListOperatorsRequest) -> pb::ListOperatorsResponse {
        // Phase 2 will wire this to the teammates registry; for
        // iter-3d we return an empty list so callers don't error
        // when the bench harness probes a fresh BS.
        pb::ListOperatorsResponse { operators: vec![] }
    }

    async fn file_pending_access(&self, req: pb::FilePendingAccessRequest) -> pb::FilePendingAccessResponse {
        let kind_str = match judge_kind_from_pb(req.kind) {
            Ok(k) => k.to_string(),
            Err(_) => {
                // The trait signature is infallible so we surface
                // the invalid kind via an empty id — callers can
                // detect the failure by the empty id + zero
                // timestamp. ResolveAccess on the empty id will
                // return NotFound, which is the right semantic.
                return pb::FilePendingAccessResponse {
                    id: String::new(),
                    created_at: None,
                };
            }
        };
        let scope_options = req.scope_options.iter().filter_map(|s| scope_from_pb(*s)).collect::<Vec<_>>();
        let scope_options = if scope_options.is_empty() { Scope::default_options() } else { scope_options };
        let detail = if req.detail.is_empty() { None } else { Some(req.detail) };
        let new = NewAccessRequest {
            bead_id: req.bead_id,
            operator_id: req.operator_id,
            kind: kind_str,
            resource: req.resource,
            detail,
            reason: req.reason,
            scope_options,
        };
        let (id, _future) = self.access.file_pending(new);
        // Look up the freshly-filed request to get its created_at
        // stamp. The store keys by id and we just inserted, so the
        // lookup is reliable barring an immediate resolve race —
        // in which case `now()` is a safe-enough fallback.
        let created_at = self
            .access
            .list_pending()
            .into_iter()
            .find(|r| r.id == id).map_or_else(Utc::now, |r| r.created_at);
        pb::FilePendingAccessResponse {
            id,
            created_at: Some(ts_to_pb(created_at)),
        }
    }

    async fn resolve_access(&self, req: pb::ResolveAccessRequest) -> Result<pb::ResolveAccessResponse, tonic::Status> {
        let verdict = match pb::Verdict::try_from(req.verdict) {
            Ok(pb::Verdict::Approve) => ResolutionVerdict::Approve,
            Ok(pb::Verdict::Deny) => ResolutionVerdict::Deny,
            Ok(pb::Verdict::Unspecified) | Err(_) => {
                return Err(tonic::Status::invalid_argument("verdict must be APPROVE or DENY"));
            }
        };
        let scope = scope_from_pb(req.scope).ok_or_else(|| tonic::Status::invalid_argument("scope must be once/session/pearl_project/user"))?;
        let glob_override = if req.glob_override.is_empty() { None } else { Some(req.glob_override) };
        let resolution = self.access.resolve(&req.id, verdict, scope, glob_override).map_err(|e| match e {
            crate::access::AccessError::NotFound(id) => tonic::Status::not_found(format!("no pending request with id {id}")),
            crate::access::AccessError::Poisoned => tonic::Status::internal("access store mutex was poisoned"),
        })?;
        Ok(pb::ResolveAccessResponse {
            resolved_at: Some(ts_to_pb(resolution.resolved_at)),
        })
    }

    async fn list_pending_access(&self) -> pb::ListPendingAccessResponse {
        let pending = self.access.list_pending().iter().map(pending_to_pb).collect();
        pb::ListPendingAccessResponse { pending }
    }

    async fn subscribe_access_events(&self, tx: mpsc::Sender<pb::AccessEvent>) {
        let mut rx = self.access.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if tx.send(domain_event_to_pb(event)).await.is_err() {
                        // Client cancelled — stop.
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Subscriber fell behind. Drop the gap and
                    // keep going; the TUI re-syncs via
                    // ListPendingAccess on lag.
                }
            }
        }
    }

    async fn subscribe_operator_events(&self, _operator_filter: String, _tx: mpsc::Sender<pb::OperatorEvent>) {
        // Phase 2 wires this to AppState::event_tx. For iter-3d
        // we close the stream immediately by returning — the
        // client sees a graceful end-of-stream. Not Unimplemented
        // because the trait is infallible here.
    }
}

/// Spawn a BigSmooth gRPC server on a UDS, backed by the
/// production AccessStore. Thin wrapper over
/// `crate::grpc::serve_uds`. Used by iter-3e startup glue.
///
/// # Errors
///
/// Returns the underlying io::Error if binding the UDS fails.
pub fn serve_uds(
    adapter: Arc<OrchestratorAdapter>,
    uds_path: std::path::PathBuf,
) -> std::io::Result<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>> {
    crate::grpc::serve_uds(adapter, uds_path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio_stream::StreamExt;
    use tower::service_fn;

    async fn build_client(uds_path: PathBuf) -> pb::big_smooth_client::BigSmoothClient<tonic::transport::Channel> {
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

    fn make_file_request(operator_id: &str, resource: &str) -> pb::FilePendingAccessRequest {
        pb::FilePendingAccessRequest {
            kind: smooth_narc::pb::JudgeKind::Network as i32,
            operator_id: operator_id.into(),
            bead_id: "pearl-1".into(),
            resource: resource.into(),
            detail: String::new(),
            reason: "test escalation".into(),
            scope_options: vec![],
        }
    }

    #[tokio::test]
    async fn file_pending_round_trips_via_grpc() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let access = AccessStore::new();
        let adapter = Arc::new(OrchestratorAdapter::new(access.clone()));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client
            .file_pending_access(tonic::Request::new(make_file_request("op-1", "api.example.com")))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.id.is_empty(), "id should be populated");
        assert!(resp.created_at.is_some());

        // The store should now hold the pending request.
        let pending = access.list_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, resp.id);
        assert_eq!(pending[0].resource, "api.example.com");
        assert_eq!(pending[0].kind, "network");
    }

    #[tokio::test]
    async fn file_with_unspecified_kind_returns_empty_id() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let access = AccessStore::new();
        let adapter = Arc::new(OrchestratorAdapter::new(access.clone()));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let mut req = make_file_request("op", "x.example");
        req.kind = smooth_narc::pb::JudgeKind::Unspecified as i32;
        let resp = client.file_pending_access(tonic::Request::new(req)).await.unwrap().into_inner();
        assert!(resp.id.is_empty(), "invalid kind should yield empty id");
        assert!(access.list_pending().is_empty());
    }

    #[tokio::test]
    async fn resolve_access_drives_the_store() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let access = AccessStore::new();
        let adapter = Arc::new(OrchestratorAdapter::new(access.clone()));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        // File one directly so we don't depend on the gRPC file path.
        let (id, future) = access.file_pending(NewAccessRequest::with_defaults("pearl", "op", "network", "x.example", "test"));

        let mut client = build_client(sock).await;
        let resp = client
            .resolve_access(tonic::Request::new(pb::ResolveAccessRequest {
                id: id.clone(),
                verdict: pb::Verdict::Approve as i32,
                scope: smooth_narc::pb::Scope::Session as i32,
                glob_override: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.resolved_at.is_some());

        // The original caller's future should now resolve to Approve.
        let resolution = future
            .await_resolution_with_timeout(Duration::from_millis(100))
            .await
            .expect("resolution arrived");
        assert_eq!(resolution.verdict, ResolutionVerdict::Approve);
        assert_eq!(resolution.scope, Scope::Session);
        assert!(access.list_pending().is_empty());
    }

    #[tokio::test]
    async fn resolve_unknown_id_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let access = AccessStore::new();
        let adapter = Arc::new(OrchestratorAdapter::new(access));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let err = client
            .resolve_access(tonic::Request::new(pb::ResolveAccessRequest {
                id: "no-such-id".into(),
                verdict: pb::Verdict::Approve as i32,
                scope: smooth_narc::pb::Scope::Once as i32,
                glob_override: String::new(),
            }))
            .await
            .expect_err("unknown id should error");
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn resolve_with_unspecified_verdict_is_invalid_argument() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let access = AccessStore::new();
        let (id, _f) = access.file_pending(NewAccessRequest::with_defaults("pearl", "op", "network", "x.example", "r"));
        let adapter = Arc::new(OrchestratorAdapter::new(access));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let err = client
            .resolve_access(tonic::Request::new(pb::ResolveAccessRequest {
                id,
                verdict: pb::Verdict::Unspecified as i32,
                scope: smooth_narc::pb::Scope::Once as i32,
                glob_override: String::new(),
            }))
            .await
            .expect_err("unspecified verdict must error");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn list_pending_returns_filed_requests() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let access = AccessStore::new();
        access.file_pending(NewAccessRequest::with_defaults("p1", "op1", "network", "a.example", "r"));
        access.file_pending(NewAccessRequest::with_defaults("p2", "op2", "tool", "shell.exec", "r"));
        let adapter = Arc::new(OrchestratorAdapter::new(access));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client
            .list_pending_access(tonic::Request::new(pb::ListPendingAccessRequest::default()))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.pending.len(), 2);
        let kinds: Vec<i32> = resp.pending.iter().map(|p| p.kind).collect();
        assert!(kinds.contains(&(smooth_narc::pb::JudgeKind::Network as i32)));
        assert!(kinds.contains(&(smooth_narc::pb::JudgeKind::Tool as i32)));
    }

    #[tokio::test]
    async fn subscribe_access_events_streams_pending_then_resolved() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let access = AccessStore::new();
        let adapter = Arc::new(OrchestratorAdapter::new(access.clone()));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let mut stream = client
            .subscribe_access_events(tonic::Request::new(pb::SubscribeAccessEventsRequest::default()))
            .await
            .unwrap()
            .into_inner();

        // File + resolve on the store after subscribing.
        let (id, _f) = access.file_pending(NewAccessRequest::with_defaults("pearl", "op", "network", "x.example", "r"));
        let _ = access.resolve(&id, ResolutionVerdict::Approve, Scope::Once, None);

        let evt1 = tokio::time::timeout(Duration::from_millis(200), stream.next()).await.unwrap().unwrap().unwrap();
        let evt2 = tokio::time::timeout(Duration::from_millis(200), stream.next()).await.unwrap().unwrap().unwrap();
        match evt1.event {
            Some(pb::access_event::Event::Pending(p)) => assert_eq!(p.id, id),
            other => panic!("expected Pending, got {other:?}"),
        }
        match evt2.event {
            Some(pb::access_event::Event::Resolved(r)) => {
                assert_eq!(r.id, id);
                assert_eq!(r.verdict, pb::Verdict::Approve as i32);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_returns_unimplemented_for_now() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let adapter = Arc::new(OrchestratorAdapter::new(AccessStore::new()));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let err = client
            .dispatch(tonic::Request::new(pb::DispatchRequest {
                bead_id: "pearl-1".into(),
                ..Default::default()
            }))
            .await
            .expect_err("dispatch should fail until phase 2");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        assert!(err.message().contains("th-ea2aa5"));
    }

    #[tokio::test]
    async fn list_operators_is_empty_in_iter_3d() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bs.sock");
        let adapter = Arc::new(OrchestratorAdapter::new(AccessStore::new()));
        let _server = serve_uds(adapter, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_client(sock).await;
        let resp = client
            .list_operators(tonic::Request::new(pb::ListOperatorsRequest::default()))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.operators.is_empty());
    }

    #[test]
    fn judge_kind_round_trip() {
        for kind in ["network", "tool", "file", "cli", "mcp", "port"] {
            let pb_kind = judge_kind_to_pb(kind);
            let back = judge_kind_from_pb(pb_kind).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn scope_round_trip() {
        for scope in [Scope::Once, Scope::Session, Scope::PearlProject, Scope::User] {
            let pb_scope = scope_to_pb(scope);
            assert_eq!(scope_from_pb(pb_scope), Some(scope));
        }
        assert_eq!(scope_from_pb(smooth_narc::pb::Scope::Unspecified as i32), None);
    }
}

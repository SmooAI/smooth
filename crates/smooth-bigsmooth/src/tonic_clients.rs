//! Tonic UDS client adapters for the in-VM cast.
//!
//! Pearl th-893801 iter-3f. The operator-runner (and any other
//! in-VM consumer) needs HTTP-compatible client surfaces that
//! happen to speak gRPC-over-UDS when
//! `SMOOTH_SINGLE_PROCESS=1` is set. Rather than rewrite every
//! call site, this module ships drop-in adapters whose async
//! method signatures match the existing HTTP clients
//! (`smooth_wonk::NarcClient::judge`, the Scribe `/log` POST,
//! the Wonk `/check/*` POSTs). Wiring into the runner main is
//! intentionally left to a follow-up iter — the adapters land
//! first so they can be exercised end-to-end by the iter-3g
//! smoke test.
//!
//! Each adapter builds its tonic Channel via the standard
//! `connect_with_connector(service_fn(... UnixStream::connect))`
//! pattern. The Channel is cloned per-call (tonic clients are
//! cheap to clone — they share the underlying transport
//! pool), so callers keep a single adapter for the runner's
//! lifetime.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use smooth_narc::judge::{JudgeDecision, JudgeRequest};

/// Build a tonic channel that dials the given UDS path. Returns
/// an error if the initial connect fails.
///
/// # Errors
///
/// Returns an error if the channel cannot be established.
pub async fn dial_uds(path: PathBuf) -> Result<tonic::transport::Channel> {
    // The Endpoint URI is a placeholder — the connector below
    // dials the UDS path regardless. Same pattern as the rest
    // of the iter-3 tests.
    let endpoint = tonic::transport::Endpoint::try_from("http://[::]:50051").context("build endpoint")?;
    let path_for_conn = path.clone();
    let channel = endpoint
        .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
            let path = path_for_conn.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .with_context(|| format!("connect UDS at {}", path.display()))?;
    Ok(channel)
}

// ── Narc ──────────────────────────────────────────────────

/// UDS-dialing Narc client.
///
/// Drop-in replacement for `smooth_wonk::NarcClient` when the
/// runner is co-resident with BS over UDS. Mirrors the HTTP
/// client's "any error folds to EscalateToHuman" contract so
/// upstream behavior is unchanged.
#[derive(Clone)]
pub struct NarcGrpcUds {
    socket: PathBuf,
    channel: tonic::transport::Channel,
}

impl NarcGrpcUds {
    /// Build a client dialing `socket`.
    ///
    /// # Errors
    ///
    /// Bubbles up channel-construction errors.
    pub async fn connect(socket: PathBuf) -> Result<Self> {
        let channel = dial_uds(socket.clone()).await?;
        Ok(Self { socket, channel })
    }

    /// Socket path the client is dialing. Useful for diagnostics.
    #[must_use]
    pub fn socket(&self) -> &PathBuf {
        &self.socket
    }

    /// Escalate a request to Narc. Any transport / proto error
    /// folds to an `EscalateToHuman` decision so Wonk fails
    /// closed — same shape as the legacy HTTP client.
    pub async fn judge(&self, request: &JudgeRequest) -> JudgeDecision {
        let pb_req: smooth_narc::pb::JudgeRequest = request.clone().into();
        let mut client = smooth_narc::pb::narc_client::NarcClient::new(self.channel.clone());
        let resp = match client.judge(tonic::Request::new(pb_req)).await {
            Ok(resp) => resp.into_inner(),
            Err(status) => {
                return JudgeDecision::escalate(format!("Narc gRPC at {} unreachable: {status}", self.socket.display()));
            }
        };
        match smooth_narc::judge::JudgeDecision::try_from(resp) {
            Ok(decision) => decision,
            Err(e) => JudgeDecision::escalate(format!("failed to decode Narc response: {e}")),
        }
    }
}

// ── Scribe ────────────────────────────────────────────────

/// UDS-dialing Scribe client.
///
/// Replaces `smooth_scribe::spawn_forwarder`'s HTTP POST to
/// Archivist with a client-streaming gRPC Log call. The
/// runner constructs one of these once and calls
/// [`Self::append`] for each log entry — entries are batched
/// internally before being sent.
#[derive(Clone)]
pub struct ScribeGrpcUds {
    socket: PathBuf,
    sender: tokio::sync::mpsc::Sender<smooth_scribe::pb::LogEntry>,
}

impl ScribeGrpcUds {
    /// Build a Scribe client dialing `socket`. Spawns a
    /// background task that owns the client-streaming Log RPC
    /// and forwards every appended entry.
    ///
    /// # Errors
    ///
    /// Bubbles up channel-construction errors.
    pub async fn connect(socket: PathBuf) -> Result<Self> {
        let channel = dial_uds(socket.clone()).await?;
        let (tx, rx) = tokio::sync::mpsc::channel::<smooth_scribe::pb::LogEntry>(256);
        let outbound_socket = socket.clone();
        tokio::spawn(async move {
            let mut client = smooth_scribe::pb::scribe_client::ScribeClient::new(channel);
            let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
            if let Err(e) = client.log(tonic::Request::new(stream)).await {
                tracing::warn!(socket = %outbound_socket.display(), error = %e, "scribe gRPC log stream ended with error");
            }
        });
        Ok(Self { socket, sender: tx })
    }

    /// Socket path the client is dialing.
    #[must_use]
    pub fn socket(&self) -> &PathBuf {
        &self.socket
    }

    /// Send a log entry. Returns false if the background stream
    /// task has terminated (channel closed). Non-blocking other
    /// than the bounded-channel back-pressure.
    pub async fn append(&self, entry: smooth_scribe::pb::LogEntry) -> bool {
        self.sender.send(entry).await.is_ok()
    }
}

// ── BigSmooth (access) ─────────────────────────────────────

/// UDS-dialing BigSmooth client for the AccessStore RPCs.
///
/// Wraps the generated `BigSmoothClient` with a stable
/// constructor that takes a UDS path instead of a hostname.
/// Used by Narc-side code that needs to file pending access
/// requests against the central AccessStore — replaces the
/// HTTP path through `/api/access/pending`.
#[derive(Clone)]
pub struct BigSmoothGrpcUds {
    socket: PathBuf,
    channel: tonic::transport::Channel,
}

impl BigSmoothGrpcUds {
    /// Build a BigSmooth client dialing `socket`.
    ///
    /// # Errors
    ///
    /// Bubbles up channel-construction errors.
    pub async fn connect(socket: PathBuf) -> Result<Self> {
        let channel = dial_uds(socket.clone()).await?;
        Ok(Self { socket, channel })
    }

    /// Socket path the client is dialing.
    #[must_use]
    pub fn socket(&self) -> &PathBuf {
        &self.socket
    }

    /// Borrow a fresh `BigSmoothClient` channel. tonic clients
    /// are cheap to clone (they share the transport pool), so
    /// callers grab one per RPC.
    #[must_use]
    pub fn client(&self) -> crate::pb::big_smooth_client::BigSmoothClient<tonic::transport::Channel> {
        crate::pb::big_smooth_client::BigSmoothClient::new(self.channel.clone())
    }
}

// ── Bundled handle ────────────────────────────────────────

/// Convenience bundle holding all three UDS clients. The
/// runner constructs one of these from `socket_dir()` paths
/// when `SMOOTH_SINGLE_PROCESS=1`.
#[derive(Clone)]
pub struct GrpcCastClients {
    pub narc: Arc<NarcGrpcUds>,
    pub scribe: Arc<ScribeGrpcUds>,
    pub bigsmooth: Arc<BigSmoothGrpcUds>,
}

impl GrpcCastClients {
    /// Connect all three clients against the standard socket
    /// layout produced by `single_process::bootstrap_grpc_cast`
    /// (`<dir>/{narc,scribe,bigsmooth}.sock`).
    ///
    /// # Errors
    ///
    /// Bubbles up the first connect error.
    pub async fn connect_all(socket_dir: &std::path::Path) -> Result<Self> {
        let narc = NarcGrpcUds::connect(socket_dir.join("narc.sock")).await?;
        let scribe = ScribeGrpcUds::connect(socket_dir.join("scribe.sock")).await?;
        let bigsmooth = BigSmoothGrpcUds::connect(socket_dir.join("bigsmooth.sock")).await?;
        Ok(Self {
            narc: Arc::new(narc),
            scribe: Arc::new(scribe),
            bigsmooth: Arc::new(bigsmooth),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::single_process::bootstrap_grpc_cast_in_dir;
    use smooth_narc::judge::{Decision, JudgeKind};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    const MIN_POLICY_TOML: &str = r#"
[metadata]
operator_id = "op"
bead_id = "pearl"
phase = "execute"

[auth]
token = "tok"

[network]

[filesystem]
writable = true
deny_patterns = []

[[mounts]]
guest_path = "/workspace"
host_path = "/tmp/work"

[tools]
allow = []
deny = []

[beads]

[mcp]

[access_requests]
enabled = true
"#;

    async fn bring_up_cast(tmp: &TempDir) -> crate::single_process::GrpcCastHandles {
        let narc = Arc::new(crate::boardroom_narc::BoardroomNarc::without_llm());
        let policy = smooth_policy::Policy::from_toml(MIN_POLICY_TOML).expect("parse policy");
        let policy_holder = smooth_wonk::policy::PolicyHolder::from_policy(policy);
        let negotiator = smooth_wonk::negotiate::Negotiator::new("http://127.0.0.1:1/no-leader", policy_holder.clone());
        let wonk = Arc::new(smooth_wonk::server::AppState::new(policy_holder, negotiator));
        let access = crate::access::AccessStore::new();
        let handles = bootstrap_grpc_cast_in_dir(tmp.path().to_path_buf(), narc, wonk, access).expect("bootstrap");
        tokio::time::sleep(Duration::from_millis(50)).await;
        handles
    }

    #[tokio::test]
    async fn narc_client_round_trips_a_decision() {
        let tmp = TempDir::new().unwrap();
        let mut handles = bring_up_cast(&tmp).await;
        let client = NarcGrpcUds::connect(handles.narc_sock.clone()).await.expect("connect narc");
        let request = JudgeRequest {
            kind: JudgeKind::Network,
            operator_id: "op-1".into(),
            bead_id: "pearl".into(),
            phase: "execute".into(),
            resource: "registry.npmjs.org".into(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        };
        let decision = client.judge(&request).await;
        assert_eq!(decision.decision, Decision::Approve);
        handles.shutdown();
    }

    #[tokio::test]
    async fn narc_client_folds_dead_socket_to_escalate() {
        let tmp = TempDir::new().unwrap();
        let mut handles = bring_up_cast(&tmp).await;
        let client = NarcGrpcUds::connect(handles.narc_sock.clone()).await.expect("connect narc");
        // Tear down the server. The next call should not panic;
        // it should fold into EscalateToHuman.
        handles.shutdown();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let request = JudgeRequest {
            kind: JudgeKind::Network,
            operator_id: "op-1".into(),
            bead_id: "pearl".into(),
            phase: "execute".into(),
            resource: "x.example".into(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        };
        let decision = client.judge(&request).await;
        assert_eq!(decision.decision, Decision::EscalateToHuman);
        assert!(decision.reason.contains("unreachable"));
    }

    #[tokio::test]
    async fn scribe_client_appends_via_streaming_log() {
        let tmp = TempDir::new().unwrap();
        let mut handles = bring_up_cast(&tmp).await;
        let scribe_store = handles.scribe_store.clone();
        let client = ScribeGrpcUds::connect(handles.scribe_sock.clone()).await.expect("connect scribe");

        // Append a couple of entries. Each lands in the
        // gRPC-backed MemoryLogStore.
        for n in 0..3 {
            let entry = smooth_scribe::pb::LogEntry {
                timestamp: Some(prost_types::Timestamp {
                    seconds: 1_000_000 + n,
                    nanos: 0,
                }),
                source: "operator-runner".into(),
                operator_id: "op-1".into(),
                bead_id: "pearl".into(),
                level: smooth_scribe::pb::Level::Info as i32,
                message: format!("hello {n}"),
                fields: std::collections::HashMap::new(),
                trace_id: String::new(),
                span_id: String::new(),
            };
            assert!(client.append(entry).await);
        }
        // Give the streaming client a beat to flush.
        tokio::time::sleep(Duration::from_millis(80)).await;
        use smooth_scribe::store::LogStore;
        assert_eq!(scribe_store.count(), 3);
        handles.shutdown();
    }

    #[tokio::test]
    async fn bigsmooth_client_lists_pending_access() {
        let tmp = TempDir::new().unwrap();
        let narc = Arc::new(crate::boardroom_narc::BoardroomNarc::without_llm());
        let policy = smooth_policy::Policy::from_toml(MIN_POLICY_TOML).expect("parse policy");
        let policy_holder = smooth_wonk::policy::PolicyHolder::from_policy(policy);
        let negotiator = smooth_wonk::negotiate::Negotiator::new("http://127.0.0.1:1/no-leader", policy_holder.clone());
        let wonk = Arc::new(smooth_wonk::server::AppState::new(policy_holder, negotiator));
        let access = crate::access::AccessStore::new();
        // File a request directly on the store so the gRPC
        // ListPendingAccess has something to return.
        access.file_pending(crate::access::NewAccessRequest::with_defaults(
            "pearl",
            "op",
            "network",
            "api.example.com",
            "test",
        ));

        let mut handles = bootstrap_grpc_cast_in_dir(tmp.path().to_path_buf(), narc, wonk, access).expect("bootstrap");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let bs = BigSmoothGrpcUds::connect(handles.bigsmooth_sock.clone()).await.expect("connect bigsmooth");
        let mut client = bs.client();
        let resp = client
            .list_pending_access(tonic::Request::new(crate::pb::ListPendingAccessRequest::default()))
            .await
            .expect("list pending")
            .into_inner();
        assert_eq!(resp.pending.len(), 1);
        assert_eq!(resp.pending[0].resource, "api.example.com");
        handles.shutdown();
    }

    #[tokio::test]
    async fn connect_all_resolves_against_bootstrap_dir() {
        let tmp = TempDir::new().unwrap();
        let mut handles = bring_up_cast(&tmp).await;
        let clients = GrpcCastClients::connect_all(tmp.path()).await.expect("connect all");
        assert!(clients.narc.socket().ends_with("narc.sock"));
        assert!(clients.scribe.socket().ends_with("scribe.sock"));
        assert!(clients.bigsmooth.socket().ends_with("bigsmooth.sock"));
        handles.shutdown();
    }
}

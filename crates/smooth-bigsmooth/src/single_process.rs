//! Single-process gRPC cast bootstrap. Pearl th-893801 iter-3e.
//!
//! When `SMOOTH_SINGLE_PROCESS=1` is set, BS spawns all four
//! cast members (Narc, Wonk, Scribe, BigSmooth) as in-process
//! tonic servers on UDS sockets rather than the legacy
//! per-cast-VM HTTP topology. Each server is backed by the
//! production state already owned by BS — no new policy holders
//! or stores are created.
//!
//! Socket layout (under `socket_dir()`):
//!
//! * `narc.sock` — `smooth_narc::grpc::Judge` over `SafehouseNarc`
//!   (iter-3a wiring).
//! * `wonk.sock` — `smooth_wonk::grpc::Checker` over the Wonk
//!   `AppState` (iter-3b wiring).
//! * `scribe.sock` — `smooth_scribe::grpc::Logger` over a fresh
//!   in-memory `LogStore` (iter-3c wiring). The store handle is
//!   returned so the caller can read entries back for tests.
//! * `bigsmooth.sock` — `smooth_bigsmooth::grpc::Orchestrator`
//!   over the live `AccessStore` (iter-3d wiring).
//!
//! Iter-3f rewires the operative's HTTP clients to dial
//! these UDS sockets when the flag is set. Until then this
//! module just brings the listeners up so the smoke test can
//! confirm the bootstrap path works.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};

use smooth_narc::grpc::Judge;
use smooth_scribe::store::MemoryLogStore;
use smooth_scribe::store_grpc::GrpcLogStoreAdapter;
use smooth_wonk::grpc::Checker;

/// Env var that gates the single-process gRPC bootstrap.
pub const ENABLE_VAR: &str = "SMOOTH_SINGLE_PROCESS";

/// Returns true when `SMOOTH_SINGLE_PROCESS=1` is set in the
/// process environment.
#[must_use]
pub fn is_enabled() -> bool {
    matches!(std::env::var(ENABLE_VAR).as_deref(), Ok("1" | "true" | "TRUE"))
}

/// Resolve the directory the gRPC sockets live under.
///
/// Honors `SMOOTH_SINGLE_PROCESS_SOCKET_DIR` first (tests set
/// this); falls back to `$XDG_RUNTIME_DIR/smooth/` and finally
/// `/tmp/smooth-<pid>/`. The directory is created if missing.
///
/// # Errors
///
/// Returns an error if the chosen directory cannot be created.
pub fn socket_dir() -> Result<PathBuf> {
    let dir = std::env::var("SMOOTH_SINGLE_PROCESS_SOCKET_DIR").map_or_else(
        |_| {
            std::env::var("XDG_RUNTIME_DIR").map_or_else(
                |_| PathBuf::from(format!("/tmp/smooth-{}", std::process::id())),
                |xdg| PathBuf::from(xdg).join("smooth"),
            )
        },
        PathBuf::from,
    );
    std::fs::create_dir_all(&dir).with_context(|| format!("create grpc socket dir at {}", dir.display()))?;
    Ok(dir)
}

/// Handles owned by the caller after `bootstrap_grpc_cast`.
///
/// The join handles keep the gRPC servers alive — dropping them
/// cancels the listeners. The socket paths are surfaced so
/// callers (and tests) can dial the servers without re-deriving
/// the directory layout.
#[must_use]
pub struct GrpcCastHandles {
    pub socket_dir: PathBuf,
    pub narc_sock: PathBuf,
    pub wonk_sock: PathBuf,
    pub scribe_sock: PathBuf,
    pub bigsmooth_sock: PathBuf,
    /// Fresh in-memory log store backing the Scribe gRPC. Tests
    /// (and iter-3f wiring) use this to read entries back.
    pub scribe_store: Arc<MemoryLogStore>,
    handles: Vec<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>>,
}

impl GrpcCastHandles {
    /// Number of currently-live server tasks. Used by tests to
    /// assert all four servers started.
    #[must_use]
    pub fn server_count(&self) -> usize {
        self.handles.len()
    }

    /// Abort every gRPC server task. Idempotent.
    pub fn shutdown(&mut self) {
        for handle in self.handles.drain(..) {
            handle.abort();
        }
        // Best-effort cleanup of stale socket files. UnixListener
        // doesn't unlink on drop.
        for path in [&self.narc_sock, &self.wonk_sock, &self.scribe_sock, &self.bigsmooth_sock] {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Spawn all four gRPC servers backed by the provided production
/// state. Returns the handles + socket paths.
///
/// The caller is responsible for keeping the returned handle
/// alive for as long as the gRPC surface should be reachable.
///
/// # Errors
///
/// Bubbles up any UDS binding error from the four `serve_uds`
/// helpers, plus directory-creation errors from `socket_dir`.
pub fn bootstrap_grpc_cast<J, C>(narc: Arc<J>, wonk: Arc<C>, access: crate::access::AccessStore) -> Result<GrpcCastHandles>
where
    J: Judge + 'static,
    C: Checker + 'static,
{
    let dir = socket_dir()?;
    bootstrap_grpc_cast_in_dir(dir, narc, wonk, access)
}

/// Same as [`bootstrap_grpc_cast`] but with the socket directory
/// chosen by the caller. Useful for tests that need isolated
/// directories without touching shared env state.
///
/// # Errors
///
/// Bubbles up any UDS binding error from the four `serve_uds`
/// helpers.
pub fn bootstrap_grpc_cast_in_dir<J, C>(dir: PathBuf, narc: Arc<J>, wonk: Arc<C>, access: crate::access::AccessStore) -> Result<GrpcCastHandles>
where
    J: Judge + 'static,
    C: Checker + 'static,
{
    std::fs::create_dir_all(&dir).with_context(|| format!("create grpc socket dir at {}", dir.display()))?;
    let narc_sock = dir.join("narc.sock");
    let wonk_sock = dir.join("wonk.sock");
    let scribe_sock = dir.join("scribe.sock");
    let bigsmooth_sock = dir.join("bigsmooth.sock");

    let narc_handle = smooth_narc::grpc::serve_uds(narc, narc_sock.clone()).with_context(|| format!("bind narc.sock at {}", narc_sock.display()))?;
    let wonk_handle = smooth_wonk::grpc::serve_uds(wonk, wonk_sock.clone()).with_context(|| format!("bind wonk.sock at {}", wonk_sock.display()))?;

    let scribe_store = Arc::new(MemoryLogStore::new());
    let scribe_adapter = Arc::new(GrpcLogStoreAdapter::new(scribe_store.clone()));
    let scribe_handle =
        smooth_scribe::grpc::serve_uds(scribe_adapter, scribe_sock.clone()).with_context(|| format!("bind scribe.sock at {}", scribe_sock.display()))?;

    let bigsmooth_adapter = Arc::new(crate::orchestrator_grpc::OrchestratorAdapter::new(access));
    let bigsmooth_handle = crate::orchestrator_grpc::serve_uds(bigsmooth_adapter, bigsmooth_sock.clone())
        .with_context(|| format!("bind bigsmooth.sock at {}", bigsmooth_sock.display()))?;

    tracing::info!(
        dir = %dir.display(),
        narc = %narc_sock.display(),
        wonk = %wonk_sock.display(),
        scribe = %scribe_sock.display(),
        bigsmooth = %bigsmooth_sock.display(),
        "single-process: gRPC cast spawned"
    );

    Ok(GrpcCastHandles {
        socket_dir: dir,
        narc_sock,
        wonk_sock,
        scribe_sock,
        bigsmooth_sock,
        scribe_store,
        handles: vec![narc_handle, wonk_handle, scribe_handle, bigsmooth_handle],
    })
}

/// Bootstrap helper specific to BS's existing state shape.
///
/// Pulls the `SafehouseNarc` + `AccessStore` directly out of an
/// `AppState` and constructs a fresh Wonk `AppState` seeded with
/// a permissive default policy (mirrors the legacy safehouse
/// spawn). Returns the handles + the Wonk `Arc` so callers that
/// need to mutate policy at runtime can hold a reference.
///
/// # Errors
///
/// Inherits all errors from `bootstrap_grpc_cast` plus the
/// default-policy generation + parse path.
pub fn bootstrap_from_app_state(state: &crate::server::AppState) -> Result<(GrpcCastHandles, Arc<smooth_wonk::server::AppState>)> {
    let default_policy_toml = crate::policy::generate_policy_for_task(
        "safehouse",
        "safehouse",
        "execute",
        "safehouse-token",
        &[],
        crate::policy::TaskType::Coding,
        vec![],
    )
    .context("single-process: generate default wonk policy")?;
    let policy = smooth_policy::Policy::from_toml(&default_policy_toml).map_err(|e| anyhow::anyhow!("single-process: parse wonk policy: {e}"))?;
    let policy_holder = smooth_wonk::policy::PolicyHolder::from_policy(policy);
    let negotiator = smooth_wonk::negotiate::Negotiator::new("http://127.0.0.1:1/no-leader", policy_holder.clone());
    let wonk = Arc::new(smooth_wonk::server::AppState::new(policy_holder, negotiator));

    let narc = Arc::new(state.safehouse_narc.clone());
    let handles = bootstrap_grpc_cast(narc, wonk.clone(), state.access.clone())?;
    Ok((handles, wonk))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;
    use tower::service_fn;

    fn build_state() -> (
        Arc<crate::safehouse_narc::SafehouseNarc>,
        Arc<smooth_wonk::server::AppState>,
        crate::access::AccessStore,
    ) {
        let narc = Arc::new(crate::safehouse_narc::SafehouseNarc::without_llm());
        let policy = smooth_policy::Policy::from_toml(MIN_POLICY_TOML).expect("parse policy");
        let policy_holder = smooth_wonk::policy::PolicyHolder::from_policy(policy);
        let negotiator = smooth_wonk::negotiate::Negotiator::new("http://127.0.0.1:1/no-leader", policy_holder.clone());
        let wonk = Arc::new(smooth_wonk::server::AppState::new(policy_holder, negotiator));
        let access = crate::access::AccessStore::new();
        (narc, wonk, access)
    }

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

    async fn connect_uds<F, T>(path: PathBuf, build: F) -> T
    where
        F: FnOnce(tonic::transport::Channel) -> T,
    {
        let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")
            .unwrap()
            .connect_with_connector(service_fn(move |_: tonic::transport::Uri| {
                let path = path.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(path).await?;
                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                }
            }))
            .await
            .expect("connect UDS");
        build(channel)
    }

    #[tokio::test]
    async fn bootstrap_spawns_all_four_sockets() {
        let tmp = TempDir::new().unwrap();
        let (narc, wonk, access) = build_state();
        let mut handles = bootstrap_grpc_cast_in_dir(tmp.path().to_path_buf(), narc, wonk, access).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(handles.server_count(), 4);
        for sock in [&handles.narc_sock, &handles.wonk_sock, &handles.scribe_sock, &handles.bigsmooth_sock] {
            assert!(sock.exists(), "{} should exist after bootstrap", sock.display());
        }
        handles.shutdown();
    }

    #[tokio::test]
    async fn each_socket_serves_its_grpc_after_bootstrap() {
        let tmp = TempDir::new().unwrap();
        let (narc, wonk, access) = build_state();
        let mut handles = bootstrap_grpc_cast_in_dir(tmp.path().to_path_buf(), narc, wonk, access.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Narc: GetCacheStats should respond with the default
        // entries=0 on a fresh SafehouseNarc.
        let mut narc_client = connect_uds(handles.narc_sock.clone(), smooth_narc::pb::narc_client::NarcClient::new).await;
        let stats = narc_client
            .get_cache_stats(tonic::Request::new(smooth_narc::pb::GetCacheStatsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(stats.entries, 0);

        // Wonk: PolicySummary should return a populated summary
        // (the default policy is permissive but non-empty).
        let mut wonk_client = connect_uds(handles.wonk_sock.clone(), smooth_wonk::pb::wonk_client::WonkClient::new).await;
        let summary = wonk_client
            .get_policy_summary(tonic::Request::new(smooth_wonk::pb::GetPolicySummaryRequest {}))
            .await
            .unwrap()
            .into_inner();
        // Summary returns counts as u32; just confirm the call succeeds.
        let _ = summary;

        // Scribe: GetStats reports total_entries=0 on a fresh store.
        let mut scribe_client = connect_uds(handles.scribe_sock.clone(), smooth_scribe::pb::scribe_client::ScribeClient::new).await;
        let scribe_stats = scribe_client
            .get_stats(tonic::Request::new(smooth_scribe::pb::GetStatsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(scribe_stats.total_entries, 0);

        // BigSmooth: ListPendingAccess on a fresh AccessStore is
        // empty; filing one through the store side should show up
        // via the gRPC.
        let mut bs_client = connect_uds(handles.bigsmooth_sock.clone(), crate::pb::big_smooth_client::BigSmoothClient::new).await;
        let pending = bs_client
            .list_pending_access(tonic::Request::new(crate::pb::ListPendingAccessRequest::default()))
            .await
            .unwrap()
            .into_inner();
        assert!(pending.pending.is_empty());
        access.file_pending(crate::access::NewAccessRequest::with_defaults("pearl", "op", "network", "x.example", "r"));
        let pending = bs_client
            .list_pending_access(tonic::Request::new(crate::pb::ListPendingAccessRequest::default()))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(pending.pending.len(), 1);
        handles.shutdown();
    }

    #[tokio::test]
    async fn shutdown_removes_socket_files() {
        let tmp = TempDir::new().unwrap();
        let (narc, wonk, access) = build_state();
        let mut handles = bootstrap_grpc_cast_in_dir(tmp.path().to_path_buf(), narc, wonk, access).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let paths = [
            handles.narc_sock.clone(),
            handles.wonk_sock.clone(),
            handles.scribe_sock.clone(),
            handles.bigsmooth_sock.clone(),
        ];
        for p in &paths {
            assert!(p.exists(), "{} should exist before shutdown", p.display());
        }
        handles.shutdown();
        // Give the abort time to land.
        tokio::time::sleep(Duration::from_millis(20)).await;
        for p in &paths {
            assert!(!p.exists(), "{} should be removed after shutdown", p.display());
        }
    }

    /// Confirms is_enabled() honors the env-var contract. Kept
    /// in its own serialized helper rather than the parallel
    /// tokio tests so it doesn't fight with concurrent tests
    /// that might mutate the same env vars in the future.
    #[test]
    fn is_enabled_reads_env_var_when_set() {
        // Use a scoped guard pattern: snapshot prior value and
        // restore on drop so we don't poison other tests in the
        // same process.
        struct EnvGuard(Option<String>);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(prev) => std::env::set_var(ENABLE_VAR, prev),
                    None => std::env::remove_var(ENABLE_VAR),
                }
            }
        }
        let prior = std::env::var(ENABLE_VAR).ok();
        let _g = EnvGuard(prior);
        std::env::remove_var(ENABLE_VAR);
        assert!(!is_enabled());
        std::env::set_var(ENABLE_VAR, "1");
        assert!(is_enabled());
        std::env::set_var(ENABLE_VAR, "false");
        assert!(!is_enabled());
    }
}

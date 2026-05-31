//! `smooth-host-stub` binary entry point.
//!
//! Pearl th-893801 Phase 2 iter-4a. Boots a HostStub gRPC server
//! on the UDS path given by `SMOOTH_HOST_STUB_SOCKET` (default
//! `/run/smooth/host.sock`). The bin's default backend set is
//! empty — concrete backends (gh, aws-sts, …) land in
//! follow-up iters once the host-side CLI shellouts are
//! audited.

#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use smooth_host_stub::BackendRegistry;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,smooth_host_stub=info")))
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

    let socket_path = std::env::var("SMOOTH_HOST_STUB_SOCKET").map_or_else(|_| PathBuf::from("/run/smooth/host.sock"), PathBuf::from);

    if let Some(parent) = socket_path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // Iter-4a: empty registry. Backends ship in follow-up iters.
    let registry = Arc::new(BackendRegistry::new());

    tracing::info!(
        socket = %socket_path.display(),
        backends = registry.len(),
        "smooth-host-stub starting"
    );
    let handle = smooth_host_stub::serve_uds(registry, socket_path)?;
    handle.await??;
    Ok(())
}

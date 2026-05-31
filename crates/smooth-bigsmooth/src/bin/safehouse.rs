//! Safehouse binary — Big Smooth running inside its own microVM.
//!
//! This binary is cross-compiled to `aarch64-unknown-linux-musl` via
//! `scripts/build-safehouse.sh` and baked into the Safehouse OCI image.
//! Bill spawns the VM with `SMOOTH_SAFEHOUSE_MODE=1`, which tells Big
//! Smooth to boot with its own cast (Wonk/Goalie/Narc/Scribe/Archivist)
//! and expose its API on guest port 4400 and Archivist on guest port
//! 4401.
//!
//! Env vars consumed:
//!
//! * `SMOOTH_SAFEHOUSE_DB` — path to `smooth.db` (default `/root/.smooth/smooth.db`)
//! * `SMOOTH_SAFEHOUSE_PORT` — Big Smooth API port (default `4400`)
//! * `SMOOTH_SAFEHOUSE_MODE=1` — enables the safehouse cast bootstrap
//! * `SMOOTH_BOOTSTRAP_BILL_URL` — Bill's URL (from the host) so Big Smooth
//!   can ask Bill to spawn operator pods. Typical value:
//!   `http://host.containers.internal:<bill_port>`.

#![allow(clippy::expect_used)]

use std::net::SocketAddr;
use std::path::PathBuf;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,smooth_bigsmooth=info")))
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

    let port: u16 = std::env::var("SMOOTH_SAFEHOUSE_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(4400);

    // Pearl store: Dolt-backed. Use a per-session temp dir so each
    // Safehouse boot starts with a clean pearl database (no stale data
    // from previous runs). The Dolt DB is ephemeral to the VM session.
    let dolt_dir = std::env::var("SMOOTH_DOLT_DIR").map_or_else(|_| PathBuf::from("/tmp/smooth-dolt"), PathBuf::from);
    let pearl_store = if dolt_dir.exists() {
        tracing::info!(dolt = %dolt_dir.display(), "safehouse: opening existing Dolt pearl store");
        smooth_pearls::PearlStore::open(&dolt_dir)?
    } else {
        tracing::info!(dolt = %dolt_dir.display(), "safehouse: initializing Dolt pearl store");
        smooth_pearls::PearlStore::init(&dolt_dir)?
    };

    // Force in-process-cast mode regardless of the env var if the
    // caller used this binary directly (it's dedicated to that mode —
    // the `th up` path on the host uses the same flag). Set both the
    // new and legacy var names during the Phase 4 transition
    // (pearl th-893801 iter-6a).
    std::env::set_var("SMOOTH_VM_MODE", "1");
    std::env::set_var("SMOOTH_SAFEHOUSE_MODE", "1");

    // Initialize the sandbox client BEFORE spawning the cast or serving
    // requests. In Safehouse mode, SMOOTH_BOOTSTRAP_BILL_URL is always
    // set, so this picks the BillSandboxClient.
    smooth_bigsmooth::sandbox::init_sandbox_client();

    // Spawn the Safehouse's own security cast: Wonk, Goalie, Narc,
    // Scribe, and Archivist. The handles carry URLs that
    // dispatch_ws_task_sandboxed needs to inject SMOOTH_ARCHIVIST_URL
    // into every operator VM's env.
    // Clone the pearl_store for Diver (PearlStore wraps a SmoothDolt which
    // can be reopened from the same directory).
    let dolt_dir_for_diver = dolt_dir.clone();
    let diver_pearl_store = smooth_pearls::PearlStore::open(&dolt_dir_for_diver).ok();
    let handles = smooth_bigsmooth::safehouse::spawn_safehouse_cast(diver_pearl_store)
        .await
        .expect("safehouse: failed to spawn cast");
    // Diagnostic: confirm the env vars that operator_facing_archivist_url() depends on.
    let bill_url = std::env::var("SMOOTH_BOOTSTRAP_BILL_URL").unwrap_or_else(|_| "<NOT SET>".into());
    let arch_port = std::env::var("SMOOTH_ARCHIVIST_HOST_PORT").unwrap_or_else(|_| "<NOT SET>".into());
    tracing::info!(
        bill_url = %bill_url,
        archivist_host_port = %arch_port,
        operator_facing = ?handles.operator_facing_archivist_url(),
        "safehouse: archivist env diagnostic"
    );
    tracing::info!(
        archivist = %handles.archivist_url,
        wonk = %handles.wonk_url,
        scribe = %handles.scribe_url,
        goalie = %handles.goalie_url,
        "safehouse: cast spawned"
    );

    let state = smooth_bigsmooth::server::AppState::new(pearl_store).with_safehouse(handles);

    // Pearl th-893801 iter-3e: when SMOOTH_SINGLE_PROCESS=1,
    // spawn the cast as in-process gRPC servers on UDS at known
    // paths alongside (not replacing) the existing HTTP cast.
    // Iter-3f flips the operator-runner over to dial the UDS
    // sockets; until then this is co-resident.
    let _grpc_cast = if smooth_bigsmooth::single_process::is_enabled() {
        match smooth_bigsmooth::single_process::bootstrap_from_app_state(&state) {
            Ok((handles, _wonk)) => {
                tracing::info!(
                    dir = %handles.socket_dir.display(),
                    "safehouse: SMOOTH_SINGLE_PROCESS=1 — gRPC cast up alongside HTTP"
                );
                Some(handles)
            }
            Err(e) => {
                tracing::error!(error = %e, "safehouse: failed to bring up gRPC cast — continuing with HTTP-only");
                None
            }
        }
    } else {
        None
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "safehouse: starting Big Smooth");
    smooth_bigsmooth::server::start(state, addr).await
}

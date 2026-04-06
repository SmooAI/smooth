//! Boardroom binary — Big Smooth running inside its own microVM.
//!
//! This binary is cross-compiled to `aarch64-unknown-linux-musl` via
//! `scripts/build-boardroom.sh` and baked into the Boardroom OCI image.
//! Bill spawns the VM with `SMOOTH_BOARDROOM_MODE=1`, which tells Big
//! Smooth to boot with its own cast (Wonk/Goalie/Narc/Scribe/Archivist)
//! and expose its API on guest port 4400 and Archivist on guest port
//! 4401.
//!
//! Env vars consumed:
//!
//! * `SMOOTH_BOARDROOM_DB` — path to `smooth.db` (default `/root/.smooth/smooth.db`)
//! * `SMOOTH_BOARDROOM_PORT` — Big Smooth API port (default `4400`)
//! * `SMOOTH_BOARDROOM_MODE=1` — enables the boardroom cast bootstrap
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

    let db_path = std::env::var("SMOOTH_BOARDROOM_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root/.smooth/smooth.db"));
    let port: u16 = std::env::var("SMOOTH_BOARDROOM_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(4400);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    tracing::info!(db = %db_path.display(), port, "boardroom: opening db");
    let db = smooth_bigsmooth::db::Database::open(&db_path)?;

    // Pearl store: Dolt-backed. Try to open existing, or auto-init.
    let dolt_dir = PathBuf::from("/root/.smooth/dolt");
    let pearl_store = if dolt_dir.exists() {
        tracing::info!(dolt = %dolt_dir.display(), "boardroom: opening existing Dolt pearl store");
        smooth_pearls::PearlStore::open(&dolt_dir)?
    } else {
        tracing::info!(dolt = %dolt_dir.display(), "boardroom: initializing Dolt pearl store");
        smooth_pearls::PearlStore::init(&dolt_dir)?
    };

    // Force boardroom mode regardless of the env var if the caller used
    // this binary directly (it's dedicated to Boardroom mode — the
    // `th up` path on the host uses a different binary).
    std::env::set_var("SMOOTH_BOARDROOM_MODE", "1");

    // Initialize the sandbox client BEFORE spawning the cast or serving
    // requests. In Boardroom mode, SMOOTH_BOOTSTRAP_BILL_URL is always
    // set, so this picks the BillSandboxClient.
    smooth_bigsmooth::sandbox::init_sandbox_client();

    // Spawn the Boardroom's own security cast: Wonk, Goalie, Narc,
    // Scribe, and Archivist. The handles carry URLs that
    // dispatch_ws_task_sandboxed needs to inject SMOOTH_ARCHIVIST_URL
    // into every operator VM's env.
    let handles = smooth_bigsmooth::boardroom::spawn_boardroom_cast()
        .await
        .expect("boardroom: failed to spawn cast");
    // Diagnostic: confirm the env vars that operator_facing_archivist_url() depends on.
    let bill_url = std::env::var("SMOOTH_BOOTSTRAP_BILL_URL").unwrap_or_else(|_| "<NOT SET>".into());
    let arch_port = std::env::var("SMOOTH_ARCHIVIST_HOST_PORT").unwrap_or_else(|_| "<NOT SET>".into());
    tracing::info!(
        bill_url = %bill_url,
        archivist_host_port = %arch_port,
        operator_facing = ?handles.operator_facing_archivist_url(),
        "boardroom: archivist env diagnostic"
    );
    tracing::info!(
        archivist = %handles.archivist_url,
        wonk = %handles.wonk_url,
        scribe = %handles.scribe_url,
        goalie = %handles.goalie_url,
        "boardroom: cast spawned"
    );

    let state = smooth_bigsmooth::server::AppState::new(db, pearl_store).with_boardroom(handles);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "boardroom: starting Big Smooth");
    smooth_bigsmooth::server::start(state, addr).await
}

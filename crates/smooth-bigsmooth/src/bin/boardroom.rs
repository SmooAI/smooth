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

    let db_path = std::env::var("SMOOTH_BOARDROOM_DB").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/root/.smooth/smooth.db"));
    let port: u16 = std::env::var("SMOOTH_BOARDROOM_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(4400);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    tracing::info!(db = %db_path.display(), port, "boardroom: opening db");
    let db = smooth_bigsmooth::db::Database::open(&db_path)?;
    let pearl_store = smooth_pearls::PearlStore::open(&db_path)?;

    // Force boardroom mode regardless of the env var if the caller used
    // this binary directly (it's dedicated to Boardroom mode — the
    // `th up` path on the host uses a different binary).
    std::env::set_var("SMOOTH_BOARDROOM_MODE", "1");

    let state = smooth_bigsmooth::server::AppState::new(db, pearl_store);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "boardroom: starting Big Smooth");
    smooth_bigsmooth::server::start(state, addr).await
}

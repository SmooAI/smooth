//! `smooth-daemon` — the always-on personal-agent daemon entry point.
//!
//! Resolves the bind address (`SMOOTH_DAEMON_BIND`, default loopback `:4400`),
//! builds the daemon state, and serves the HTTP/WebSocket surface until
//! Ctrl-C / SIGTERM. Logging honours `RUST_LOG` (default `info`, with the
//! daemon at `debug`).
//!
//! Later wired behind `th up` / `th service` (a cross-crate pearl); for now run
//! it directly: `SMOOTH_API_URL=… SMOOTH_API_KEY=… SMOOTH_MODEL=… smooth-daemon`.

use std::process::ExitCode;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "smooth-daemon exited with error");
            eprintln!("smooth-daemon: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let addr = smooth_daemon::config::resolve_bind()?;
    let state = smooth_daemon::AppState::new();
    smooth_daemon::serve(state, addr).await
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,smooth_daemon=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}

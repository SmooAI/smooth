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
    // Durable SQLite-backed state so events/sessions survive a restart.
    let mut state = smooth_daemon::AppState::persistent_default()?;
    tracing::info!(db = %smooth_daemon::AppState::default_db_path().display(), "durable state");

    // Egress boundary (opt-in via SMOOTH_EGRESS_ALLOWLIST): start the goalie
    // forward proxy on loopback and point the bash tool at it, so agent shell
    // commands can only reach the exact hosts on the allowlist.
    if let Some(setup) = smooth_daemon::config::resolve_egress() {
        let audit = smooth_goalie::AuditLogger::new(&smooth_daemon::config::egress_audit_path().to_string_lossy())?;
        if !setup.rejected.is_empty() {
            tracing::warn!(rejected = ?setup.rejected, "egress allowlist dropped invalid entries");
        }
        tracing::info!(proxy = %setup.proxy_addr, hosts = setup.allowlist.len(), "egress boundary ON");
        let proxy_addr = setup.proxy_addr.clone();
        let allowlist = setup.allowlist;
        tokio::spawn(async move {
            if let Err(e) = smooth_goalie::run_proxy_local(&proxy_addr, allowlist, audit).await {
                tracing::error!(error = %e, "egress proxy exited — sandboxed egress now fails closed");
            }
        });
        state.egress_proxy = Some(setup.proxy_addr);
    }

    smooth_daemon::serve(state, addr).await
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,smooth_daemon=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}

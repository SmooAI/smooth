//! A SmoothFlow engine on a port and nothing else — the host the e2e suites
//! (macOS XCUITests, `th flow` smoke) point at instead of a full `smooth-daemon`
//! (which takes the machine-wide lock, arms `tailscale serve`, and wants LLM
//! credentials). Same router + supervisor code as the daemon, pearl th-8e3087.
//!
//! ```sh
//! cargo run -p smooai-smooth-daemon --example flow_e2e_server -- \
//!     --addr 127.0.0.1:0 --db /tmp/flow.db --workspace /tmp/ws --token e2e-tok --tmux-socket flow-e2e
//! ```
//!
//! Prints `listening on 127.0.0.1:<port>` once bound and writes the address to
//! `<workspace>/.flow-e2e-addr` (what `tests/fixtures/fake-claude` reads), then
//! serves until killed.

#![allow(clippy::expect_used, reason = "a test host: failing loudly is the point")]

use std::path::PathBuf;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `RUST_LOG` (default `warn`) — the supervisor's trace lines are how a
    // silent state machine gets debugged.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")))
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().collect();
    let addr = arg(&args, "--addr").unwrap_or_else(|| "127.0.0.1:0".into());
    let workspace = PathBuf::from(arg(&args, "--workspace").unwrap_or_else(|| ".".into()));
    let db = arg(&args, "--db").map_or_else(|| workspace.join("flow.db"), PathBuf::from);
    let token = arg(&args, "--token").filter(|t| !t.is_empty());
    if let Some(sock) = arg(&args, "--tmux-socket") {
        std::env::set_var("SMOOTH_FLOW_TMUX_SOCKET", sock);
    }
    // `home` (harness manifests) and `daemon_url` follow the defaults: $HOME —
    // the caller points it at a throwaway dir — and none.
    let mut cfg = smooth_flow::EngineConfig {
        db_path: db,
        version: "e2e".into(),
        machine_label: "flow-e2e".into(),
        ..smooth_flow::EngineConfig::new(workspace.clone())
    };
    cfg.pty_host = pty_host(&cfg.host.default_socket());
    let engine = smooth_flow::Engine::open(cfg)?;
    drop(smooth_daemon::flow_route::spawn_supervisor(engine.clone()));
    let router = smooth_daemon::flow_route::flow_router(engine, token);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let bound = listener.local_addr()?;
    std::fs::write(workspace.join(".flow-e2e-addr"), bound.to_string())?;
    println!("listening on {bound}");
    axum::serve(listener, router).await?;
    Ok(())
}

/// `SMOOTH_FLOW_HOST=pty` (th-dc9822): this example has no `flow-host`
/// subcommand, so its sessions are hosted by the `smooth-daemon` cargo built
/// beside it (`target/<profile>/examples/..`).
#[cfg(all(unix, feature = "pty-host"))]
fn pty_host(owner: &str) -> Option<std::sync::Arc<dyn smooth_flow::SessionHost>> {
    let exe = std::env::current_exe().ok()?;
    let daemon = exe.parent()?.parent()?.join("smooth-daemon");
    daemon
        .is_file()
        .then(|| std::sync::Arc::new(smooth_flow::PtyHost::for_daemon(owner, daemon)) as std::sync::Arc<dyn smooth_flow::SessionHost>)
}

#[cfg(not(all(unix, feature = "pty-host")))]
fn pty_host(_owner: &str) -> Option<std::sync::Arc<dyn smooth_flow::SessionHost>> {
    None
}

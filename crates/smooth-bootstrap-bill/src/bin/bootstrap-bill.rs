//! Bootstrap Bill — host-side broker for The Board.
//!
//! Usage:
//!
//! ```bash
//! bootstrap-bill --listen 127.0.0.1:0 --print-port
//! ```
//!
//! On startup Bill binds the TCP listener, optionally prints the resolved
//! port on stdout (so parents can capture it from a pipe), and installs a
//! panic hook + SIGINT handler that destroys every registered sandbox
//! before exiting. Sandboxes leaked across a panic leave a microVM
//! running that nothing can find; don't do that to the user.

#![allow(clippy::expect_used)]

use std::net::SocketAddr;

use anyhow::Context;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use smooth_bootstrap_bill::server;

#[derive(Parser, Debug)]
#[command(version, about = "Bootstrap Bill — The Board's host-side broker", long_about = None)]
struct Args {
    /// Address to bind on. Use `127.0.0.1:0` to ask the kernel for an
    /// ephemeral port.
    #[arg(long, default_value = "127.0.0.1:4444")]
    listen: SocketAddr,

    /// If set, print the resolved listen port on a single line to stdout
    /// after binding ("BILL_PORT=<port>"). Useful for parent processes
    /// that pipe stdout to read back the kernel-assigned port.
    #[arg(long)]
    print_port: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

    let args = Args::parse();

    // Install a panic hook that destroys every sandbox before we go down.
    // We use a blocking-safe shutdown because panic hooks run outside a
    // tokio context; `futures::executor::block_on` works in a pinch, but
    // we don't want to pull futures in. Instead: spawn a short-lived
    // tokio runtime inside the hook.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("bill: PANIC — destroying all sandboxes before exit");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok();
        if let Some(rt) = rt {
            rt.block_on(server::destroy_all());
        }
        default_hook(info);
    }));

    let (local, handle) = server::listen(args.listen).await.context("bill: listen")?;
    if args.print_port {
        println!("BILL_PORT={}", local.port());
        // Flush immediately so parents reading line-by-line can see it.
        use std::io::Write;
        std::io::stdout().flush().ok();
    }
    eprintln!("bill: ready at {local}");

    // SIGINT → graceful shutdown: destroy all sandboxes, then exit.
    tokio::select! {
        _ = handle => {
            eprintln!("bill: accept loop exited");
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("bill: SIGINT — destroying all sandboxes");
            server::destroy_all().await;
        }
    }

    Ok(())
}

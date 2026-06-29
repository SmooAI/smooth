//! `smooth-daemon` — the always-on personal-agent daemon binary.
//!
//! **Standalone by design.** The `th` CLI does not statically link the operator
//! runtime; instead `th daemon <args…>` resolves + spawns this binary (fetching
//! it on demand), and this binary owns the full daemon CLI:
//!
//! - `smooth-daemon` (default) / `smooth-daemon operator [--addr]` — run
//!   smooth-operator's local deployment flavor (canonical WS protocol + official
//!   widget + kernel-sandboxed tools), durable via the local sqlite adapter.
//!   **This IS the daemon** — the bespoke `serve_persistent` agent loop is
//!   retired (EPIC th-c89c2a: one operator runtime, no second loop).
//! - `smooth-daemon audit [--lines]` — tail the egress proxy's audit log.
//!
//! Logging honours `RUST_LOG` (default `info`, daemon at `debug`).

use std::net::SocketAddr;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "smooth-daemon", version, about = "Smoo AI always-on personal-agent daemon")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the operator (default). `smooth-daemon` with no subcommand is the
    /// operator's local flavor on `:8787` — canonical WS protocol + widget,
    /// durable, egress-gated. Same as `operator` without `--addr`.
    Run,
    /// Run the operator's local deployment flavor (canonical WS protocol + widget).
    /// Same as the default `Run` — kept as an explicit form that takes `--addr`.
    Operator {
        /// Address to bind the local-flavor operator on.
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: String,
    },
    /// Tail the egress proxy's audit log (allowed/blocked off-box decisions).
    Audit {
        /// How many recent decisions to show.
        #[arg(long, default_value = "20")]
        lines: usize,
    },
}

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

async fn run() -> Result<()> {
    // The north star: `th daemon` IS the operator. Both the default (`Run`) and
    // the explicit `operator` subcommand run smooth-operator's local flavor
    // (canonical WS protocol + official widget), durable via the local sqlite
    // adapter, egress-gated when configured. No second agent loop — the bespoke
    // serve_persistent path is retired (EPIC th-c89c2a).
    match Cli::parse().cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => {
            let socket: SocketAddr = smooth_operator_server::local::DEFAULT_LOCAL_ADDR
                .parse()
                .expect("DEFAULT_LOCAL_ADDR is a valid SocketAddr");
            smooth_daemon::serve_local_flavor(socket).await
        }
        Cmd::Operator { addr } => {
            let socket: SocketAddr = addr.parse().with_context(|| format!("invalid --addr {addr:?}"))?;
            smooth_daemon::serve_local_flavor(socket).await
        }
        Cmd::Audit { lines } => cmd_audit(lines),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,smooth_daemon=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}

// ---- status -----------------------------------------------------------------

// ---- audit ------------------------------------------------------------------

fn cmd_audit(lines: usize) -> Result<()> {
    let path = smooth_daemon::config::egress_audit_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => {
            println!("no egress audit log at {} — has the egress boundary handled any requests yet?", path.display());
            return Ok(());
        }
    };
    let all: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if all.is_empty() {
        println!("egress audit log is empty");
        return Ok(());
    }
    for line in &all[all.len().saturating_sub(lines)..] {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            println!("{}", format_audit_line(&v));
        }
    }
    Ok(())
}

fn format_audit_line(v: &serde_json::Value) -> String {
    let ts = v["timestamp"].as_str().unwrap_or("");
    let allowed = v["allowed"].as_bool().unwrap_or(false);
    let domain = v["domain"].as_str().unwrap_or("?");
    let method = v["method"].as_str().unwrap_or("");
    let mark = if allowed { "ALLOW" } else { "BLOCK" };
    format!("{ts}  {mark}  {method:<7} {domain}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn audit_line_formats_allow_and_block() {
        let allow = format_audit_line(&serde_json::json!({"timestamp":"t","allowed":true,"domain":"api.smoo.ai","method":"GET"}));
        assert!(allow.contains("ALLOW") && allow.contains("api.smoo.ai"));
        let block = format_audit_line(&serde_json::json!({"timestamp":"t","allowed":false,"domain":"evil.test","method":"POST"}));
        assert!(block.contains("BLOCK") && block.contains("evil.test"));
    }

    #[test]
    fn default_local_addr_parses() {
        let _: SocketAddr = smooth_operator_server::local::DEFAULT_LOCAL_ADDR.parse().unwrap();
    }
}

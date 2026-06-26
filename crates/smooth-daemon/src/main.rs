//! `smooth-daemon` — the always-on personal-agent daemon binary.
//!
//! **Standalone by design.** The `th` CLI does not statically link the operator
//! runtime; instead `th daemon <args…>` resolves + spawns this binary (fetching
//! it on demand), and this binary owns the full daemon CLI:
//!
//! - `smooth-daemon` / `smooth-daemon run` — run the bespoke daemon in the
//!   foreground (durable state + egress boundary).
//! - `smooth-daemon operator [--addr]` — run the operator's local deployment
//!   flavor (canonical WS protocol + official widget + kernel-sandboxed tools).
//! - `smooth-daemon status [--port]` — query a running daemon's `/api/status`.
//! - `smooth-daemon audit [--lines]` — tail the egress proxy's audit log.
//! - `smooth-daemon schedule list|add|rm` — manage proactive scheduled tasks.
//!
//! Logging honours `RUST_LOG` (default `info`, daemon at `debug`).

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

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
    /// Run the bespoke daemon in the foreground (durable state + egress). Default.
    Run,
    /// Run the operator's local deployment flavor (canonical WS protocol + widget).
    Operator {
        /// Address to bind the local-flavor operator on.
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: String,
    },
    /// Show the running daemon's status by querying its `/api/status`.
    Status {
        /// Daemon API port.
        #[arg(long, default_value = "4400")]
        port: u16,
    },
    /// Tail the egress proxy's audit log (allowed/blocked off-box decisions).
    Audit {
        /// How many recent decisions to show.
        #[arg(long, default_value = "20")]
        lines: usize,
    },
    /// Manage scheduled/proactive tasks (`/api/schedule`).
    Schedule {
        /// Daemon API port.
        #[arg(long, default_value = "4400")]
        port: u16,
        #[command(subcommand)]
        cmd: ScheduleCmd,
    },
}

#[derive(Subcommand)]
enum ScheduleCmd {
    /// List scheduled tasks.
    List,
    /// Add a scheduled task (provide exactly one of --every-minutes / --daily).
    Add {
        /// The prompt to run on the cadence.
        #[arg(long)]
        prompt: String,
        /// Fire every N minutes.
        #[arg(long, conflicts_with = "daily")]
        every_minutes: Option<u64>,
        /// Fire daily at HH:MM (UTC).
        #[arg(long, conflicts_with = "every_minutes")]
        daily: Option<String>,
    },
    /// Remove a scheduled task by id.
    Rm {
        /// The schedule id (from `schedule list`).
        id: String,
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
    match Cli::parse().cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => {
            // Canonical entry: durable state + egress boundary (if configured) + serve.
            let addr = smooth_daemon::config::resolve_bind()?;
            smooth_daemon::serve_persistent(addr).await
        }
        Cmd::Operator { addr } => {
            let socket: SocketAddr = addr.parse().with_context(|| format!("invalid --addr {addr:?}"))?;
            smooth_daemon::serve_local_flavor(socket).await
        }
        Cmd::Status { port } => cmd_status(port).await,
        Cmd::Audit { lines } => cmd_audit(lines),
        Cmd::Schedule { port, cmd } => cmd_schedule(port, cmd).await,
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,smooth_daemon=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}

// ---- status -----------------------------------------------------------------

async fn cmd_status(port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/api/status");
    let client = reqwest::Client::builder().timeout(Duration::from_secs(3)).build()?;
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await?;
            println!("{}", format_daemon_status(&body));
        }
        Ok(resp) => println!("daemon returned HTTP {} at {url}", resp.status()),
        Err(_) => println!("daemon not reachable at {url} — is `smooth-daemon` running?"),
    }
    Ok(())
}

fn format_daemon_uptime(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600)
    } else if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Render the daemon's `/api/status` JSON into a human-readable block.
fn format_daemon_status(body: &serde_json::Value) -> String {
    let version = body["version"].as_str().unwrap_or("unknown");
    let mode = body["permission_mode"].as_str().unwrap_or("?");
    let active = body["active_tasks"].as_u64().unwrap_or(0);
    let egress = body["egress_proxy"].as_str().map_or_else(|| "off".to_owned(), |p| format!("on ({p})"));
    let uptime = body["uptime_seconds"].as_u64().map_or_else(|| "?".to_owned(), format_daemon_uptime);
    format!("smooth-daemon v{version}\n  uptime:       {uptime}\n  mode:         {mode}\n  egress:       {egress}\n  active tasks: {active}")
}

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

// ---- schedule ---------------------------------------------------------------

async fn cmd_schedule(port: u16, cmd: ScheduleCmd) -> Result<()> {
    let base = format!("http://127.0.0.1:{port}/api/schedule");
    let client = reqwest::Client::builder().timeout(Duration::from_secs(3)).build()?;
    let unreachable = || println!("daemon not reachable at {base} — is `smooth-daemon` running?");
    match cmd {
        ScheduleCmd::List => match client.get(&base).send().await {
            Ok(r) if r.status().is_success() => {
                let list: Vec<serde_json::Value> = r.json().await?;
                if list.is_empty() {
                    println!("no schedules");
                }
                for s in &list {
                    println!("{}", format_schedule_line(s));
                }
            }
            Ok(r) => println!("list failed: HTTP {}", r.status()),
            Err(_) => unreachable(),
        },
        ScheduleCmd::Add { prompt, every_minutes, daily } => {
            let kind = build_schedule_kind(every_minutes, daily.as_deref())?;
            let body = serde_json::json!({ "prompt": prompt, "schedule": kind });
            match client.post(&base).json(&body).send().await {
                Ok(r) if r.status().is_success() => {
                    let s: serde_json::Value = r.json().await?;
                    println!("added {}", format_schedule_line(&s));
                }
                Ok(r) => println!("add failed: HTTP {}", r.status()),
                Err(_) => unreachable(),
            }
        }
        ScheduleCmd::Rm { id } => match client.delete(format!("{base}/{id}")).send().await {
            Ok(r) if r.status().is_success() => println!("removed schedule {id}"),
            Ok(r) => println!("remove failed: HTTP {}", r.status()),
            Err(_) => unreachable(),
        },
    }
    Ok(())
}

/// Build the `ScheduleKind` JSON for `POST /api/schedule` from the CLI flags.
fn build_schedule_kind(every_minutes: Option<u64>, daily: Option<&str>) -> Result<serde_json::Value> {
    match (every_minutes, daily) {
        (Some(m), None) => {
            if m == 0 {
                anyhow::bail!("--every-minutes must be at least 1");
            }
            Ok(serde_json::json!({ "kind": "every_n_seconds", "secs": m * 60 }))
        }
        (None, Some(hhmm)) => {
            let (h, m) = hhmm.split_once(':').ok_or_else(|| anyhow::anyhow!("--daily must be HH:MM, got {hhmm:?}"))?;
            let hour: u8 = h.parse().map_err(|_| anyhow::anyhow!("invalid hour in {hhmm:?}"))?;
            let minute: u8 = m.parse().map_err(|_| anyhow::anyhow!("invalid minute in {hhmm:?}"))?;
            if hour > 23 || minute > 59 {
                anyhow::bail!("--daily out of range (00:00–23:59): {hhmm:?}");
            }
            Ok(serde_json::json!({ "kind": "daily_at", "hour": hour, "minute": minute }))
        }
        (None, None) => anyhow::bail!("provide --every-minutes N or --daily HH:MM"),
        (Some(_), Some(_)) => anyhow::bail!("--every-minutes and --daily are mutually exclusive"),
    }
}

fn format_schedule_line(s: &serde_json::Value) -> String {
    let id = s["id"].as_str().unwrap_or("?");
    let prompt = s["prompt"].as_str().unwrap_or("");
    let next = s["next_due"].as_str().unwrap_or("");
    let enabled = s["enabled"].as_bool().unwrap_or(true);
    let cadence = match s["kind"]["kind"].as_str() {
        Some("every_n_seconds") => format!("every {}m", s["kind"]["secs"].as_u64().unwrap_or(0) / 60),
        Some("daily_at") => format!(
            "daily {:02}:{:02}",
            s["kind"]["hour"].as_u64().unwrap_or(0),
            s["kind"]["minute"].as_u64().unwrap_or(0)
        ),
        _ => "?".to_owned(),
    };
    let disabled = if enabled { "" } else { " (disabled)" };
    format!("{id}  {cadence:<11} next {next}{disabled}  {prompt}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn build_schedule_kind_from_flags() {
        assert_eq!(build_schedule_kind(Some(30), None).unwrap()["secs"], 1800);
        assert_eq!(build_schedule_kind(None, Some("08:30")).unwrap()["hour"], 8);
        assert!(build_schedule_kind(Some(0), None).is_err());
        assert!(build_schedule_kind(None, Some("25:00")).is_err());
        assert!(build_schedule_kind(None, None).is_err());
        assert!(build_schedule_kind(Some(1), Some("08:00")).is_err());
    }

    #[test]
    fn formats_status_and_uptime() {
        assert_eq!(format_daemon_uptime(90), "1m 30s");
        assert_eq!(format_daemon_uptime(3661), "1h 1m");
        let status = format_daemon_status(&serde_json::json!({"version":"1.2.3","permission_mode":"default","active_tasks":2,"uptime_seconds":3661}));
        assert!(status.contains("v1.2.3") && status.contains("1h 1m"));
    }
}

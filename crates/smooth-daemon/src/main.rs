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
    /// Manage proactive schedules — prompts the always-on agent fires on a
    /// cadence (the running daemon picks them up on its next tick).
    Schedule {
        #[command(subcommand)]
        cmd: ScheduleCmd,
    },
    /// Inspect the Gate-1 permission rules (`~/.smooth/permissions.toml`).
    Permissions {
        #[command(subcommand)]
        cmd: PermissionsCmd,
    },
}

#[derive(Subcommand)]
enum PermissionsCmd {
    /// Show what verdict (deny/ask/allow) the rules give a command or write.
    Check {
        /// The command (or, with `--write`, the workspace-relative path).
        input: String,
        /// Check a `write_file`/`edit_file` to this path instead of a bash command.
        #[arg(long)]
        write: bool,
    },
    /// Print the resolved permissions-file path.
    Path,
}

#[derive(Subcommand)]
enum ScheduleCmd {
    /// Add a proactive schedule: a prompt fired on a cadence.
    Add {
        /// The prompt to send the agent when it fires.
        prompt: String,
        /// Fire on an interval, e.g. `30m`, `2h`, `90s`, `1d`. Mutually
        /// exclusive with `--daily-at`.
        #[arg(long, conflicts_with = "daily_at")]
        every: Option<String>,
        /// Fire once per day at this UTC time, `HH:MM` (e.g. `09:30`).
        #[arg(long)]
        daily_at: Option<String>,
    },
    /// List all schedules (next-due first).
    List,
    /// Remove a schedule by id.
    Remove {
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
        Cmd::Schedule { cmd } => cmd_schedule(cmd).await,
        Cmd::Permissions { cmd } => cmd_permissions(&cmd),
    }
}

fn cmd_permissions(cmd: &PermissionsCmd) -> Result<()> {
    use smooth_policy::auto_mode::Decision;

    match cmd {
        PermissionsCmd::Path => {
            match smooth_tools::permission::config_path() {
                Some(p) => println!("{}", p.display()),
                None => println!("(could not resolve a home directory)"),
            }
            Ok(())
        }
        PermissionsCmd::Check { input, write } => {
            let rules = smooth_tools::permission::load();
            let verdict = if *write { rules.decide("Write", input) } else { rules.decide_bash(input) };
            let label = match verdict {
                Decision::Deny => "DENY",
                Decision::Ask => "ASK",
                Decision::Allow => "ALLOW",
            };
            let kind = if *write { "write" } else { "bash" };
            println!("{label}  ({kind}) {input}");
            Ok(())
        }
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

// ---- schedule ---------------------------------------------------------------

/// Parse an interval like `30m`, `2h`, `90s`, `1d` into seconds. Bare digits are
/// treated as seconds. Returns an error on an empty/garbled value or zero.
fn parse_every(s: &str) -> Result<u64> {
    let s = s.trim();
    anyhow::ensure!(!s.is_empty(), "empty interval");
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1u64),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some('d') => (&s[..s.len() - 1], 86_400),
        Some(c) if c.is_ascii_digit() => (s, 1),
        _ => anyhow::bail!("invalid interval {s:?} (use e.g. 30m, 2h, 90s, 1d)"),
    };
    let n: u64 = num.trim().parse().with_context(|| format!("invalid interval number in {s:?}"))?;
    let secs = n.checked_mul(mult).context("interval too large")?;
    anyhow::ensure!(secs > 0, "interval must be greater than zero");
    Ok(secs)
}

/// Parse a `HH:MM` 24-hour UTC time into `(hour, minute)`.
fn parse_daily_at(s: &str) -> Result<(u8, u8)> {
    let (h, m) = s.trim().split_once(':').context("daily-at must be HH:MM (e.g. 09:30)")?;
    let hour: u8 = h.parse().with_context(|| format!("invalid hour in {s:?}"))?;
    let minute: u8 = m.parse().with_context(|| format!("invalid minute in {s:?}"))?;
    anyhow::ensure!(hour < 24, "hour must be 0-23");
    anyhow::ensure!(minute < 60, "minute must be 0-59");
    Ok((hour, minute))
}

async fn cmd_schedule(cmd: ScheduleCmd) -> Result<()> {
    use smooth_daemon::schedule::{Schedule, ScheduleKind, ScheduleStore, SqliteScheduleStore};

    let store = SqliteScheduleStore::open(&smooth_daemon::operator::schedule_store_path()).context("opening the schedule store")?;
    match cmd {
        ScheduleCmd::Add { prompt, every, daily_at } => {
            let kind = match (every, daily_at) {
                (Some(e), _) => ScheduleKind::EveryNSeconds { secs: parse_every(&e)? },
                (None, Some(d)) => {
                    let (hour, minute) = parse_daily_at(&d)?;
                    ScheduleKind::DailyAt { hour, minute }
                }
                (None, None) => anyhow::bail!("pick a cadence: --every <30m|2h|…> or --daily-at <HH:MM>"),
            };
            let id = uuid::Uuid::new_v4().simple().to_string();
            let schedule = Schedule::new(id.clone(), prompt, kind, chrono::Utc::now());
            let next = schedule.next_due;
            store.upsert(schedule).await?;
            println!("added schedule {id} (next due {next})");
        }
        ScheduleCmd::List => {
            let all = store.list().await?;
            if all.is_empty() {
                println!("no schedules — add one with `smooth-daemon schedule add \"<prompt>\" --every 30m`");
                return Ok(());
            }
            for s in all {
                let cadence = match s.kind {
                    ScheduleKind::EveryNSeconds { secs } => format!("every {secs}s"),
                    ScheduleKind::DailyAt { hour, minute } => format!("daily at {hour:02}:{minute:02}Z"),
                };
                let state = if s.enabled { "on" } else { "off" };
                println!("{}  [{}]  {}  next {}  — {}", s.id, state, cadence, s.next_due, s.prompt);
            }
        }
        ScheduleCmd::Remove { id } => {
            store.delete(&id).await?;
            println!("removed schedule {id}");
        }
    }
    Ok(())
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

    #[test]
    fn parse_every_handles_units_and_rejects_junk() {
        assert_eq!(parse_every("90s").unwrap(), 90);
        assert_eq!(parse_every("30m").unwrap(), 1800);
        assert_eq!(parse_every("2h").unwrap(), 7200);
        assert_eq!(parse_every("1d").unwrap(), 86_400);
        assert_eq!(parse_every("45").unwrap(), 45, "bare digits = seconds");
        assert!(parse_every("0m").is_err(), "zero rejected");
        assert!(parse_every("").is_err());
        assert!(parse_every("abc").is_err());
        assert!(parse_every("10x").is_err());
    }

    #[test]
    fn parse_daily_at_validates_time() {
        assert_eq!(parse_daily_at("09:30").unwrap(), (9, 30));
        assert_eq!(parse_daily_at("00:00").unwrap(), (0, 0));
        assert_eq!(parse_daily_at("23:59").unwrap(), (23, 59));
        assert!(parse_daily_at("24:00").is_err(), "hour out of range");
        assert!(parse_daily_at("09:60").is_err(), "minute out of range");
        assert!(parse_daily_at("0930").is_err(), "missing colon");
    }
}

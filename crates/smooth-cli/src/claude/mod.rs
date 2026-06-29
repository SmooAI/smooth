//! `th claude` — supervise Claude Code sessions running inside tmux.
//!
//! v1 ships the 1:1 topology: launch a session, auto-detect the last
//! message, and on the account-wide rate-limit throttle back off with
//! jitter and resend until it lands. Attach to drive it interactively.
//!
//! The pieces are built so the 1:N farm (one Big Smooth leading N
//! sessions on a shared governor) and N:1 / mixed topologies are later
//! wirings of the same `supervisor` + `governor` + `registry`.

pub mod control;
pub mod detect;
pub mod governor;
pub mod registry;
pub mod supervisor;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;

use supervisor::RunOpts;

#[derive(Debug, Subcommand)]
pub enum ClaudeCommands {
    /// Launch a Claude Code session in a supervised tmux session and keep
    /// it alive: on the account-wide rate-limit throttle ("temporarily
    /// limiting requests"), back off with jitter and resend the last
    /// message until it lands. Attach with `th claude attach <id>` to
    /// drive it; the session lives as long as this supervisor runs.
    Run {
        /// Initial prompt to send once the TUI is ready. Omit to just
        /// launch + supervise an interactive session you attach to.
        prompt: Option<String>,
        /// Working directory for the session (default: current dir).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Label/role shown in `th claude ls`.
        #[arg(long)]
        label: Option<String>,
        /// Command to launch (default: `claude`).
        #[arg(long, default_value = "claude")]
        command: String,
        /// Seconds between pane polls.
        #[arg(long, default_value_t = 2)]
        poll_secs: u64,
    },
    /// List supervised Claude sessions (prunes any whose tmux session has
    /// died).
    Ls {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Attach your terminal to a supervised session (`tmux attach`).
    /// Accepts a full id or a unique prefix.
    Attach {
        /// Session id (or unique prefix) from `th claude ls`.
        id: String,
    },
    /// Set who drives a session: `driving` (Big Smooth sends input +
    /// rescues rate-limits), `manual` (you drive; the supervisor only
    /// rescues your throttled turns), or `paused` (supervisor stands
    /// down). Lets you hand control back and forth without killing the
    /// session.
    Mode {
        /// Session id (or unique prefix) from `th claude ls`.
        id: String,
        /// `driving` | `manual` | `paused`.
        mode: String,
    },
}

/// Dispatch a `th claude` subcommand.
///
/// # Errors
/// Propagates launch/attach failures.
pub async fn cmd_claude(cmd: ClaudeCommands) -> Result<()> {
    match cmd {
        ClaudeCommands::Run {
            prompt,
            cwd,
            label,
            command,
            poll_secs,
        } => {
            let cwd = match cwd {
                Some(c) => c,
                None => std::env::current_dir().context("resolving current directory")?,
            };
            run(RunOpts {
                cwd,
                label,
                command,
                initial_prompt: prompt,
                poll: Duration::from_secs(poll_secs.max(1)),
                boot_timeout: Duration::from_secs(30),
            })
            .await
        }
        ClaudeCommands::Ls { json } => ls(json),
        ClaudeCommands::Attach { id } => attach(&id),
        ClaudeCommands::Mode { id, mode } => set_mode(&id, &mode),
    }
}

fn set_mode(id: &str, mode: &str) -> Result<()> {
    let parsed: control::Mode = mode.parse()?;
    // Resolve the id against live sessions so a typo fails loudly instead
    // of silently writing a control file no supervisor reads.
    let entry = registry::read_live_and_prune()
        .into_iter()
        .find(|e| e.id == id || e.id.starts_with(id))
        .ok_or_else(|| anyhow!("no live session matching `{id}` — try `th claude ls`"))?;
    control::write_mode(&entry.id, parsed)?;
    println!("{} session {} → {}", "⇄".cyan(), entry.id.bold(), parsed.to_string().bold());
    Ok(())
}

async fn run(opts: RunOpts) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_task = stop.clone();

    // The supervise loop is blocking (tmux subprocess calls + sleeps), so
    // it runs on a blocking thread while the async side owns Ctrl-C.
    let mut handle = tokio::task::spawn_blocking(move || supervisor::supervise_blocking(opts, stop_for_task));

    println!("{} supervising — {} to stop", "claude".bold(), "Ctrl-C".cyan());
    tokio::select! {
        res = &mut handle => return res.context("supervisor task panicked")?,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\n{} stopping…", "⏹".yellow());
            stop.store(true, Ordering::SeqCst);
        }
    }
    handle.await.context("supervisor task panicked")?
}

fn ls(json: bool) -> Result<()> {
    let live = registry::read_live_and_prune();
    if json {
        println!("{}", serde_json::to_string_pretty(&live)?);
        return Ok(());
    }
    if live.is_empty() {
        println!("No supervised Claude sessions. Start one with `{}`.", "th claude run".cyan());
        return Ok(());
    }
    println!(
        "{:<10} {:<8} {:<12} {:<8} {}",
        "ID".bold(),
        "MODE".bold(),
        "LABEL".bold(),
        "STARTED".bold(),
        "CWD".bold()
    );
    for e in &live {
        println!(
            "{:<10} {:<8} {:<12} {:<8} {}",
            e.id.cyan(),
            control::read_mode(&e.id).as_str(),
            e.label.as_deref().unwrap_or("-"),
            e.started_at.format("%H:%M").to_string(),
            e.cwd.dimmed()
        );
    }
    Ok(())
}

fn attach(id: &str) -> Result<()> {
    let matches: Vec<_> = registry::read_live_and_prune()
        .into_iter()
        .filter(|e| e.id == id || e.id.starts_with(id))
        .collect();
    let entry = match matches.as_slice() {
        [] => return Err(anyhow!("no live session matching `{id}` — try `th claude ls`")),
        [one] => one.clone(),
        many => {
            let ids: Vec<_> = many.iter().map(|e| e.id.as_str()).collect();
            return Err(anyhow!("`{id}` is ambiguous — matches {}", ids.join(", ")));
        }
    };

    // Hand the terminal over to tmux by replacing this process.
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new("tmux")
        .args(["-L", &entry.socket, "attach", "-t", &entry.session])
        .exec();
    Err(anyhow!("failed to exec `tmux attach` for session {}: {err}", entry.session))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    // Build a tiny clap harness so the subcommand wiring is validated
    // without depending on the whole `th` Cli.
    #[derive(clap::Parser)]
    struct Harness {
        #[command(subcommand)]
        cmd: super::ClaudeCommands,
    }

    #[test]
    fn clap_wiring_is_valid() {
        Harness::command().debug_assert();
    }

    #[test]
    fn run_parses_prompt_and_flags() {
        use clap::Parser;
        let h = Harness::try_parse_from(["x", "run", "fix the bug", "--label", "fixer", "--poll-secs", "3"]).unwrap();
        match h.cmd {
            super::ClaudeCommands::Run {
                prompt,
                label,
                poll_secs,
                command,
                ..
            } => {
                assert_eq!(prompt.as_deref(), Some("fix the bug"));
                assert_eq!(label.as_deref(), Some("fixer"));
                assert_eq!(poll_secs, 3);
                assert_eq!(command, "claude");
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn attach_requires_id() {
        use clap::Parser;
        assert!(Harness::try_parse_from(["x", "attach"]).is_err());
        assert!(Harness::try_parse_from(["x", "attach", "abc"]).is_ok());
    }

    #[test]
    fn mode_parses_id_and_mode() {
        use clap::Parser;
        let h = Harness::try_parse_from(["x", "mode", "ab12", "manual"]).unwrap();
        match h.cmd {
            super::ClaudeCommands::Mode { id, mode } => {
                assert_eq!(id, "ab12");
                assert_eq!(mode, "manual");
            }
            _ => panic!("expected Mode"),
        }
        // mode requires both args.
        assert!(Harness::try_parse_from(["x", "mode", "ab12"]).is_err());
    }
}

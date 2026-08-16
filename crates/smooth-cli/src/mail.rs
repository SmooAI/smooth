//! `th agent` / `th msg` / `th inbox` — agent presence + agent-to-agent mail.
//!
//! Pearl th-374f85 (ADR-010). Backed by [`smooth_pearls::MailStore`]: one
//! SQLite file per machine (`~/.smooth/mail.db`), not the per-repo Dolt pearl
//! store. That kills three problems at once — the mailbox no longer depends on
//! which worktree you happen to be standing in, concurrent agents no longer
//! wedge each other with Dolt's single writer, and a send is an instant local
//! write instead of a ~0.7s Dolt boot plus a git push.
//!
//! The `--push` / `--pull` flags from the Dolt era are still **accepted** (the
//! SessionStart hook and existing scripts pass them) but do nothing.

use std::fmt::Write as _;
use std::path::PathBuf;

use anstream::{eprintln, println};
use anyhow::{bail, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use smooth_pearls::{AgentStatus, MailMessage, MessageKind};

use crate::mail_backend::{load_config, save_config, Backend, CloudMailStore, Mail, MailConfig};

#[derive(Subcommand)]
pub enum AgentCommands {
    /// Register this session as a named agent (idempotent). Other agents can
    /// then message it by name. Re-registering an existing handle resumes it,
    /// keeping its mail, task, and status.
    Register {
        /// Agent handle. Defaults to the resolved handle (`th agent whoami`).
        #[arg(long)]
        name: Option<String>,
        /// Harness/tool tag (claude-code, opencode, pi, shell). Falls back to
        /// $SMOOTH_HARNESS, then "unknown".
        #[arg(long)]
        harness: Option<String>,
        /// PID of the LONG-LIVED session process (e.g. `--pid $PPID` from a
        /// SessionStart hook), so `th agent list` can reap it when it dies.
        /// Falls back to $SMOOTH_AGENT_PID. Deliberately NOT this `th`
        /// invocation's own pid — that exits immediately and would make every
        /// agent read as offline a second after registering.
        #[arg(long)]
        pid: Option<i64>,
        /// Register a SECOND identity for this session even though it already
        /// has one. Almost always wrong — see the error this unlocks.
        #[arg(long)]
        force: bool,
        /// Deprecated no-op — mail is machine-local, there is nothing to push.
        #[arg(long, hide = true)]
        no_push: bool,
    },
    /// List registered agents (most recently seen first). Agents whose process
    /// is gone are reaped to `offline` first.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show the handle this session resolves to, plus the store it uses.
    Whoami {
        #[arg(long)]
        json: bool,
    },
    /// Publish presence: what this agent is doing right now.
    Status {
        /// idle | working | waiting | offline
        #[arg(long)]
        status: String,
        /// Free-form "what I'm working on". Pass an empty string to clear.
        #[arg(long)]
        task: Option<String>,
        /// Which agent (defaults to the resolved handle).
        #[arg(long)]
        name: Option<String>,
    },
    /// Claim a durable handle for this session: renames the current handle
    /// (carrying its mail) if it has one, else registers the name fresh.
    Claim {
        /// The handle to claim.
        name: String,
    },
    /// Rename an agent handle, carrying its inbox and sent mail with it.
    Rename {
        /// Current handle to rename.
        #[arg(long)]
        from: String,
        /// New handle.
        #[arg(long)]
        to: String,
        /// Deprecated no-op.
        #[arg(long, hide = true)]
        no_push: bool,
    },
    /// Mark this (or a named) agent offline.
    Offline {
        #[arg(long)]
        name: Option<String>,
    },
    /// Choose where mail lives: this machine (default, free, offline) or your
    /// Smoo account (shared across machines, paid after a 14-day trial).
    Backend {
        #[command(subcommand)]
        cmd: BackendCommands,
    },
}

#[derive(Subcommand)]
pub enum BackendCommands {
    /// Show the current backend — and, on cloud, who you are signed in as and
    /// how your trial/subscription stands.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Switch backends.
    ///
    /// `sqlite` (the default) keeps mail in `~/.smooth/mail.db` on this machine:
    /// no account, no network, and every `th msg` / `th agent` command works.
    /// `cloud` puts it on your Smoo account so agents on OTHER machines join the
    /// same bus — that needs `th auth login`, and is a paid feature after a
    /// 14-day trial. Mail is NOT migrated between the two.
    Set {
        /// sqlite | cloud
        backend: String,
    },
}

#[derive(Subcommand)]
pub enum MsgCommands {
    /// Send a message: `th msg send <to> <body...>` (`all` broadcasts).
    Send {
        /// Recipient agent name, or `all`. May also be given as --to.
        #[arg(value_name = "TO")]
        to_pos: Option<String>,
        /// Message body — the remaining words. May also be given as --body.
        #[arg(value_name = "BODY")]
        body_pos: Vec<String>,
        /// Recipient (flag form, for compatibility).
        #[arg(long = "to")]
        to: Option<String>,
        /// Body (flag form, for compatibility).
        #[arg(long = "body")]
        body: Option<String>,
        /// Sender name. Defaults to the resolved handle.
        #[arg(long)]
        from: Option<String>,
        /// Reply under an existing message's thread (its id or any id in it).
        #[arg(long)]
        re: Option<String>,
        /// What this message is for: note|request|result|handoff|cancel.
        #[arg(long = "type", default_value = "note")]
        kind: String,
        /// Higher sorts first in the recipient's inbox.
        #[arg(long, default_value = "0")]
        priority: i64,
        /// Deprecated no-op.
        #[arg(long, hide = true)]
        no_push: bool,
    },
    /// Show messages addressed to me (and broadcasts).
    Inbox {
        /// Whose inbox (defaults to the resolved handle).
        #[arg(long)]
        agent: Option<String>,
        /// Only unread messages.
        #[arg(long)]
        unread: bool,
        /// Acknowledge the listed messages after showing them.
        #[arg(long)]
        mark_read: bool,
        #[arg(long, default_value = "50")]
        limit: usize,
        #[arg(long)]
        json: bool,
        /// Deprecated no-op.
        #[arg(long, hide = true)]
        pull: bool,
    },
    /// Acknowledge messages (mark them read for you).
    #[command(visible_alias = "read")]
    Ack {
        /// Message ids to acknowledge.
        ids: Vec<String>,
        /// Acknowledge every unread message instead.
        #[arg(long)]
        all: bool,
        /// Whose read state (defaults to the resolved handle).
        #[arg(long)]
        agent: Option<String>,
    },
    /// Print just the number of unread messages. Nothing else on stdout, so a
    /// statusline or prompt can inline it without parsing.
    UnreadCount {
        /// Whose inbox (defaults to the resolved handle).
        #[arg(long)]
        agent: Option<String>,
    },
    /// Reply to a message (threads automatically).
    Reply {
        /// Message id being replied to.
        id: String,
        #[arg(long)]
        body: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long = "type", default_value = "result")]
        kind: String,
        #[arg(long, default_value = "0")]
        priority: i64,
    },
    /// Show a full thread (a root message + its replies).
    Thread { id: String },
    /// Poll for new messages and print them as they arrive.
    Watch {
        /// Whose inbox (defaults to the resolved handle).
        #[arg(long)]
        agent: Option<String>,
        /// Seconds between polls.
        #[arg(long, default_value = "5")]
        interval: u64,
        /// Exit as soon as there is unread mail, printing it. The primitive
        /// behind the th-mail skill's background watcher.
        #[arg(long)]
        once: bool,
        /// Print messages as a JSON array (implies machine consumption).
        #[arg(long)]
        json: bool,
        /// Deprecated no-op.
        #[arg(long, hide = true)]
        no_pull: bool,
        /// Deprecated no-op.
        #[arg(long, hide = true)]
        pull: bool,
    },
}

// ── Identity ───────────────────────────────────────────────────────

fn env_handle(var: &str) -> Option<String> {
    std::env::var(var).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Where the SessionStart hook records what handle a Claude Code session
/// registered under. `$SMOOTH_AGENT_SESSIONS_DIR` relocates it — tests use that
/// rather than moving `$HOME`, which is process-global and would break any
/// unrelated test running in parallel.
fn session_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SMOOTH_AGENT_SESSIONS_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".smooth")
        .join("agent-sessions")
}

/// Env vars a harness may use to name the current session, in priority order.
///
/// `CLAUDE_CODE_SESSION_ID` is the one Claude Code actually exports to tool
/// subprocesses. `CLAUDE_SESSION_ID` is what this resolver originally read —
/// and it is never set, so the session-file tier never fired and every
/// `th msg inbox` with no `--agent` silently answered for `user@host` instead
/// of the handle the SessionStart hook registered (pearl th-fa9f40: a whole
/// session's mail read from an inbox nobody was writing to). Both are kept; a
/// second `getenv` is cheaper than guessing which one a harness exports.
const SESSION_ID_VARS: [&str; 2] = ["CLAUDE_CODE_SESSION_ID", "CLAUDE_SESSION_ID"];

/// The handle identifying THIS session — `$SMOOTH_AGENT_HANDLE`, else
/// `$SMOOTH_AGENT`, else whatever the SessionStart hook recorded for this
/// session id — or `None` when nothing identifies the caller.
///
/// The one source of truth for "who am I": [`resolve_handle`] adds the
/// `user@host` fallback for the CLI, and `mcp_serve::resolve_agent_id` refuses
/// rather than falling back. Neither re-derives the chain.
#[must_use]
pub fn session_handle() -> Option<String> {
    if let Some(h) = env_handle("SMOOTH_AGENT_HANDLE").or_else(|| env_handle("SMOOTH_AGENT")) {
        return Some(h);
    }
    SESSION_ID_VARS.iter().filter_map(|v| env_handle(v)).find_map(|sid| {
        std::fs::read_to_string(session_state_dir().join(sid))
            .ok()
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
    })
}

/// [`session_handle`], falling back to `user@short-hostname`.
#[must_use]
pub fn resolve_handle() -> String {
    if let Some(h) = session_handle() {
        return h;
    }
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "agent".to_string());
    let host = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".to_string());
    let host = host.split('.').next().unwrap_or(&host);
    format!("{user}@{host}")
}

/// The pid to record for liveness reaping: `$SMOOTH_AGENT_PID`, else none.
///
/// ponytail: never `std::process::id()`. `th` is a one-shot child of whatever
/// session is really the agent, so recording our own pid would have every
/// agent reaped to `offline` the moment the command returned. Callers that
/// know a durable pid pass it (`--pid $PPID` from the SessionStart hook);
/// everyone else gets NULL and is judged by `last_seen` instead.
fn supervised_pid() -> Option<i64> {
    env_handle("SMOOTH_AGENT_PID").and_then(|v| v.parse().ok())
}

fn default_harness() -> String {
    env_handle("SMOOTH_HARNESS").unwrap_or_else(|| "unknown".to_string())
}

/// Point every session-state file that still names `old` at `new`, so the
/// SessionStart-hook handle and the store agree after a rename/claim. Returns
/// how many files were rewritten; a missing directory is simply zero.
fn rewrite_session_handles(old: &str, new: &str) -> usize {
    if old == new {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(session_state_dir()) else { return 0 };
    let mut n = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if std::fs::read_to_string(&path).is_ok_and(|c| c.trim() == old) && std::fs::write(&path, new).is_ok() {
            n += 1;
        }
    }
    n
}

async fn store() -> Result<Mail> {
    Mail::open().await
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn deprecated(flag: &str) {
    eprintln!("{} {flag} is a no-op — agent mail is machine-local now (no remote to sync).", "note:".dimmed());
}

// ── Rendering ──────────────────────────────────────────────────────

fn print_message(m: &MailMessage) {
    let when = m.created_at.format("%Y-%m-%d %H:%M").to_string();
    let unread = if m.read_at.is_none() {
        "●".yellow().to_string()
    } else {
        "○".dimmed().to_string()
    };
    let thread = m.thread_id.as_ref().map(|t| format!(" {}", format!("(re {t})").dimmed())).unwrap_or_default();
    let mut tags = String::new();
    if m.kind != MessageKind::Note {
        let _ = write!(tags, " [{}]", m.kind.magenta());
    }
    if m.priority != 0 {
        let _ = write!(tags, " {}", format!("p{}", m.priority).red());
    }
    println!(
        "{unread} {} {} → {}{thread}{tags}  {}",
        m.id.dimmed(),
        m.from_agent.cyan(),
        m.to_agent.green(),
        when.dimmed(),
    );
    for line in m.body.lines() {
        println!("    {line}");
    }
}

fn print_messages(msgs: &[MailMessage], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(msgs)?);
    } else {
        for m in msgs {
            print_message(m);
        }
    }
    Ok(())
}

// ── th agent ───────────────────────────────────────────────────────

/// Dispatch a `th agent` subcommand.
///
/// # Errors
/// Returns an error if the store can't be opened or the operation fails.
pub async fn cmd_agent(cmd: AgentCommands) -> Result<()> {
    // Handled before the store is opened, deliberately: if `cloud` is selected
    // and you're signed out, opening it fails — and `backend set sqlite` is
    // exactly the command you need to get out of that.
    if let AgentCommands::Backend { cmd } = cmd {
        return cmd_backend(cmd).await;
    }
    let s = store().await?;
    match cmd {
        AgentCommands::Register {
            name,
            harness,
            pid,
            force,
            no_push,
        } => {
            if no_push {
                deprecated("--no-push");
            }
            let name = name.unwrap_or_else(resolve_handle);
            // A session that already has a handle registering a DIFFERENT one
            // splits its identity in two: mail keeps arriving at the old
            // mailbox while every later command reads the new one, and the
            // failure looks exactly like "no mail" (pearl th-fa9f40). `claim`
            // is the sanctioned path — it renames and carries the mail across.
            if let Some(existing) = session_handle().filter(|e| *e != name) {
                if !force {
                    bail!(
                        "this session is already agent {existing} — registering {name} would create a SECOND identity, \
                         and mail sent to {existing} would never be seen.\n  \
                         → rename and carry your mail over: th agent claim {name}\n  \
                         → really want two mailboxes: th agent register --name {name} --force"
                    );
                }
                eprintln!(
                    "{} registering {name} as a second identity — {existing} keeps its own mail.",
                    "warning:".yellow().bold()
                );
            }
            let harness = harness.unwrap_or_else(default_harness);
            s.register(&name, &harness, pid.or_else(supervised_pid), &cwd()).await?;
            println!("{} registered as {} ({})", "✓".green().bold(), name.green().bold(), harness.dimmed());
            println!("  {} continuously check: {}", "→".dimmed(), format!("th msg watch --agent {name}").cyan());
        }
        AgentCommands::List { json } => {
            let agents = s.list_agents().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&agents)?);
            } else if agents.is_empty() {
                println!("No agents registered. Run: {}", "th agent register".cyan());
            } else {
                println!("{}", format!("{} registered agent(s):", agents.len()).bold());
                for a in &agents {
                    let dot = match a.status {
                        AgentStatus::Working => "●".green().to_string(),
                        AgentStatus::Waiting => "●".yellow().to_string(),
                        AgentStatus::Idle => "●".cyan().to_string(),
                        AgentStatus::Offline => "○".dimmed().to_string(),
                    };
                    let where_ = a.branch.as_deref().map(|b| format!(" {}", b.dimmed())).unwrap_or_default();
                    let task = a.task.as_deref().map(|t| format!("  {t}")).unwrap_or_default();
                    println!(
                        "  {dot} {}  {} {}{where_}{task}  {}",
                        a.name.bold(),
                        a.harness.dimmed(),
                        a.status.to_string().dimmed(),
                        format!("last-seen {}", a.last_seen.format("%Y-%m-%d %H:%M")).dimmed(),
                    );
                }
            }
        }
        AgentCommands::Whoami { json } => {
            let handle = resolve_handle();
            let registered = s.get_agent(&handle).await?;
            let unread = s.unread_count(&handle).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "handle": handle,
                        "store": s.location(),
                        "backend": s.backend().as_str(),
                        "registered": registered.is_some(),
                        "agent": registered,
                        "unread": unread,
                    }))?
                );
            } else {
                println!("{} {}", "handle".dimmed(), handle.green().bold());
                println!("{}  {} {}", "store ".dimmed(), s.location().dimmed(), format!("({})", s.backend()).dimmed());
                match &registered {
                    Some(a) => println!("{} {} ({})", "status".dimmed(), a.status, a.task.as_deref().unwrap_or("no task")),
                    None => println!("{} {}", "status".dimmed(), "not registered — run `th agent register`".yellow()),
                }
                println!("{} {unread}", "unread".dimmed());
            }
        }
        AgentCommands::Status { status, task, name } => {
            let name = name.unwrap_or_else(resolve_handle);
            let status: AgentStatus = status.parse()?;
            s.set_status(&name, status, task.as_deref()).await?;
            println!(
                "{} {} is {}{}",
                "✓".green().bold(),
                name.bold(),
                status.to_string().green(),
                task.map(|t| format!(" — {t}")).unwrap_or_default()
            );
        }
        AgentCommands::Claim { name } => {
            let name = name.trim().to_string();
            if name.is_empty() {
                bail!("agent name must not be empty");
            }
            let previous = resolve_handle();
            // Rename only when the old handle exists and the new one is free —
            // that's the path that carries the mail. Otherwise just register,
            // which resumes `name` if it already exists.
            if previous != name && s.get_agent(&previous).await?.is_some() && s.get_agent(&name).await?.is_none() {
                s.rename(&previous, &name).await?;
                println!(
                    "{} claimed {} (was {}, mail carried over)",
                    "✓".green().bold(),
                    name.green().bold(),
                    previous.dimmed()
                );
            } else {
                // Say which of the two this was: resuming a handle that already
                // exists hands you ITS mail, not the mail of the handle you
                // were using a second ago.
                let resumed = previous != name && s.get_agent(&name).await?.is_some();
                s.register(&name, &default_harness(), supervised_pid(), &cwd()).await?;
                if resumed {
                    println!(
                        "{} resumed existing agent {} — you now read its mail, not {}'s",
                        "✓".green().bold(),
                        name.green().bold(),
                        previous.dimmed()
                    );
                } else {
                    println!("{} claimed {}", "✓".green().bold(), name.green().bold());
                }
            }
            let rewritten = rewrite_session_handles(&previous, &name);
            if rewritten > 0 {
                println!("  {} updated {rewritten} session handle file(s)", "→".dimmed());
            }
        }
        AgentCommands::Rename { from, to, no_push } => {
            if no_push {
                deprecated("--no-push");
            }
            s.rename(&from, &to).await?;
            rewrite_session_handles(&from, &to);
            println!("{} {} renamed to {}", "✓".green().bold(), from.dimmed(), to.green().bold());
        }
        // Peeled off above, before the store is opened.
        AgentCommands::Backend { .. } => unreachable!("handled before the store is opened"),
        AgentCommands::Offline { name } => {
            let name = name.unwrap_or_else(resolve_handle);
            s.set_status(&name, AgentStatus::Offline, None).await?;
            println!("{} {} marked offline", "✓".green().bold(), name);
        }
    }
    Ok(())
}

// ── th agent backend ───────────────────────────────────────────────

/// Fetch the cloud entitlement, or `None` when we can't (signed out, offline) —
/// `backend status` must still print WHICH backend is selected in that case,
/// since that's the state the user is trying to diagnose.
async fn cloud_entitlement() -> (Option<String>, Option<crate::mail_backend::Entitlement>) {
    match CloudMailStore::connect().await {
        Ok(c) => match c.entitlement().await {
            Ok(e) => (None, Some(e)),
            Err(e) => (Some(format!("{e:#}")), None),
        },
        Err(e) => (Some(format!("{e:#}")), None),
    }
}

async fn cmd_backend(cmd: BackendCommands) -> Result<()> {
    match cmd {
        BackendCommands::Status { json } => {
            let backend = load_config().backend;
            let (problem, ent) = if backend == Backend::Cloud { cloud_entitlement().await } else { (None, None) };
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "backend": backend.as_str(),
                        "config": crate::mail_backend::config_path().display().to_string(),
                        "store": match backend {
                            Backend::Sqlite => smooth_pearls::mail_store::default_path().display().to_string(),
                            Backend::Cloud => format!("{}/user/agent-mail", smooth_api_client::base_url()),
                        },
                        "entitled": ent.as_ref().map(|e| e.entitled),
                        "entitlement": ent.as_ref().map(crate::mail_backend::Entitlement::summary),
                        "problem": problem,
                    }))?
                );
                return Ok(());
            }
            println!("{} {}", "backend".dimmed(), backend.to_string().green().bold());
            match backend {
                Backend::Sqlite => {
                    println!(
                        "{}   {}",
                        "store".dimmed(),
                        smooth_pearls::mail_store::default_path().display().to_string().dimmed()
                    );
                    println!("{}   {}", "cost ".dimmed(), "free — no account, works offline".dimmed());
                    println!(
                        "\n  {} share one bus with agents on OTHER machines: {}",
                        "→".dimmed(),
                        "th agent backend set cloud".cyan()
                    );
                }
                Backend::Cloud => {
                    println!(
                        "{}   {}",
                        "store".dimmed(),
                        format!("{}/user/agent-mail", smooth_api_client::base_url()).dimmed()
                    );
                    match (&ent, &problem) {
                        (Some(e), _) => {
                            let line = e.summary();
                            println!(
                                "{}  {}",
                                "access".dimmed(),
                                if e.entitled { line.green().to_string() } else { line.yellow().to_string() }
                            );
                        }
                        (None, Some(p)) => {
                            println!("{}  {}", "access".dimmed(), p.yellow());
                            println!(
                                "\n  {} the free local mailbox is always there: {}",
                                "→".dimmed(),
                                "th agent backend set sqlite".cyan()
                            );
                        }
                        (None, None) => {}
                    }
                }
            }
            println!("{}  {}", "config".dimmed(), crate::mail_backend::config_path().display().to_string().dimmed());
        }
        BackendCommands::Set { backend } => {
            let backend = Backend::parse(&backend)?;
            save_config(&MailConfig { backend })?;
            println!("{} mail backend is now {}", "✓".green().bold(), backend.to_string().green().bold());
            match backend {
                Backend::Sqlite => println!(
                    "  {}",
                    format!("{} — free, offline, this machine only", smooth_pearls::mail_store::default_path().display()).dimmed()
                ),
                Backend::Cloud => {
                    // Reading the entitlement is what STARTS the trial, so this
                    // doubles as "your 14 days begin now" and gets the days-left
                    // number in front of the user immediately.
                    let (problem, ent) = cloud_entitlement().await;
                    match (ent, problem) {
                        (Some(e), _) => println!("  {}", e.summary().dimmed()),
                        (None, Some(p)) => {
                            println!("  {} {p}", "!".yellow().bold());
                            println!("  {} fix that, or go back with {}", "→".dimmed(), "th agent backend set sqlite".cyan());
                        }
                        (None, None) => {}
                    }
                    println!("  {}", "existing local mail is NOT migrated — it stays in ~/.smooth/mail.db".dimmed());
                }
            }
        }
    }
    Ok(())
}

// ── th msg ─────────────────────────────────────────────────────────

/// Dispatch a `th msg` subcommand.
///
/// # Errors
/// Returns an error if the store can't be opened, arguments are missing, or
/// the operation fails.
pub async fn cmd_msg(cmd: MsgCommands) -> Result<()> {
    let s = store().await?;
    match cmd {
        MsgCommands::Send {
            to_pos,
            body_pos,
            to,
            body,
            from,
            re,
            kind,
            priority,
            no_push,
        } => {
            if no_push {
                deprecated("--no-push");
            }
            let Some(to) = to.or(to_pos) else {
                bail!("missing recipient — usage: th msg send <to|all> <body...>");
            };
            let body = body.unwrap_or_else(|| body_pos.join(" "));
            if body.trim().is_empty() {
                bail!("missing body — usage: th msg send <to|all> <body...>");
            }
            let from = from.unwrap_or_else(resolve_handle);
            let kind: MessageKind = kind.parse()?;
            // Replies inherit the original message's thread root.
            let thread_id = match re {
                Some(ref rid) => s.get_message(rid).await?.map(|m| m.thread_root().to_string()),
                None => None,
            };
            let id = s.send(&from, &to, &body, kind, priority, thread_id.as_deref()).await?;
            println!("{} sent {} to {}", "✓".green().bold(), id.dimmed(), to.green());
        }
        MsgCommands::Inbox {
            agent,
            unread,
            mark_read,
            limit,
            json,
            pull,
        } => {
            if pull {
                deprecated("--pull");
            }
            let who = agent.unwrap_or_else(resolve_handle);
            let _ = s.touch(&who).await; // heartbeat (best-effort)
            let msgs = s.inbox(&who, unread, limit).await?;
            if json {
                print_messages(&msgs, true)?;
            } else if msgs.is_empty() {
                println!("{}", format!("Inbox for {who} is empty{}.", if unread { " (no unread)" } else { "" }).dimmed());
            } else {
                println!("{}", format!("{} message(s) for {who}:", msgs.len()).bold());
                print_messages(&msgs, false)?;
            }
            if mark_read {
                for m in &msgs {
                    s.ack(&who, &m.id).await?;
                }
            }
        }
        MsgCommands::Ack { ids, all, agent } => {
            let who = agent.unwrap_or_else(resolve_handle);
            if all {
                let n = s.ack_all(&who).await?;
                println!("{} acknowledged {n} message(s) for {who}", "✓".green().bold());
            } else if ids.is_empty() {
                bail!("nothing to acknowledge — pass message ids or --all");
            } else {
                for id in &ids {
                    s.ack(&who, id).await?;
                }
                println!("{} acknowledged {}", "✓".green().bold(), ids.join(", ").dimmed());
            }
        }
        MsgCommands::UnreadCount { agent } => {
            let who = agent.unwrap_or_else(resolve_handle);
            // Bare number, no decoration — the statusline hook embeds this.
            println!("{}", s.unread_count(&who).await?);
        }
        MsgCommands::Reply {
            id,
            body,
            from,
            kind,
            priority,
        } => {
            let from = from.unwrap_or_else(resolve_handle);
            let Some(orig) = s.get_message(&id).await? else {
                bail!("no message {id}");
            };
            let root = orig.thread_root().to_string();
            let to = orig.from_agent;
            let new_id = s.send(&from, &to, &body, kind.parse()?, priority, Some(&root)).await?;
            // Replying is reading: don't leave the message we just answered unread.
            s.ack(&from, &id).await?;
            println!("{} replied {} to {}", "✓".green().bold(), new_id.dimmed(), to.green());
        }
        MsgCommands::Thread { id } => {
            let Some(m) = s.get_message(&id).await? else {
                bail!("no message {id}");
            };
            let thread = s.thread(m.thread_root()).await?;
            println!("{}", format!("Thread {} ({} message(s)):", m.thread_root(), thread.len()).bold());
            print_messages(&thread, false)?;
        }
        MsgCommands::Watch {
            agent,
            interval,
            once,
            json,
            no_pull,
            pull,
        } => {
            if no_pull || pull {
                deprecated(if pull { "--pull" } else { "--no-pull" });
            }
            let who = agent.unwrap_or_else(resolve_handle);
            if !once && !json {
                println!("👀 watching inbox for {} (every {interval}s). Ctrl-C to stop.", who.green().bold());
            }
            let interval = std::time::Duration::from_secs(interval.max(1));
            loop {
                let _ = s.touch(&who).await;
                match s.inbox(&who, true, 200).await {
                    Ok(msgs) if !msgs.is_empty() => {
                        print_messages(&msgs, json)?;
                        if once {
                            // Leave the mail UNREAD: the caller decides when it
                            // has actually been handled (`th msg ack`), so a
                            // dropped watcher cycle never loses a message.
                            return Ok(());
                        }
                        for m in &msgs {
                            s.ack(&who, &m.id).await?; // consume so it doesn't repeat
                        }
                    }
                    Ok(_) => {}
                    // A failed poll is NOT an empty inbox. `--once` is consumed
                    // by watch-once.sh, whose caller reads "exited without
                    // messages" as "no mail" — so it must fail loudly instead
                    // (pearl th-ad0701). A long-lived watcher rides out
                    // transient errors, which is what the retry is for.
                    Err(e) if once => return Err(e.context("inbox poll failed — mail state is unknown, not empty")),
                    Err(e) => eprintln!("{} inbox poll failed: {e}", "!".yellow()),
                }
                tokio::time::sleep(interval).await;
            }
        }
    }
    Ok(())
}

/// `th inbox` — convenience alias for `th msg inbox`.
///
/// # Errors
/// Returns an error if the store can't be opened or the query fails.
pub async fn cmd_inbox() -> Result<()> {
    cmd_msg(MsgCommands::Inbox {
        agent: None,
        unread: false,
        mark_read: false,
        limit: 50,
        json: false,
        pull: false,
    })
    .await
}

/// Serializes every test in this binary that reads or writes the agent-handle
/// env vars — they are process-global, and the test harness runs threads in
/// parallel. `mcp_serve`'s identity tests take the same lock.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// The vars these tests own. `$HOME` is deliberately NOT among them: it is
    /// read by unrelated tests in this binary, which run in parallel.
    const OWNED_VARS: [&str; 7] = [
        "SMOOTH_AGENT_HANDLE",
        "SMOOTH_AGENT",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "SMOOTH_HARNESS",
        "SMOOTH_AGENT_PID",
        "SMOOTH_AGENT_SESSIONS_DIR",
    ];

    struct EnvGuard;

    impl EnvGuard {
        fn clear() -> Self {
            for v in OWNED_VARS {
                std::env::remove_var(v);
            }
            Self
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for v in OWNED_VARS {
                std::env::remove_var(v);
            }
        }
    }

    #[test]
    fn handle_resolution_order() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = tempfile::tempdir().unwrap();
        let _g = EnvGuard::clear();
        let dir = tmp.path().join("agent-sessions");
        std::env::set_var("SMOOTH_AGENT_SESSIONS_DIR", &dir);

        // 4. Fallback is user@short-host — no dots, always has an @.
        let fallback = resolve_handle();
        assert!(fallback.contains('@'), "fallback should be user@host, got {fallback}");

        // 3. A session-state file wins over the fallback — under EITHER session
        // id var. Regression for th-fa9f40: only `CLAUDE_SESSION_ID` was read,
        // Claude Code exports `CLAUDE_CODE_SESSION_ID`, so this tier never
        // fired and every no-flag `th msg` command answered for `user@host`.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sess-1"), "cc-smooth-ab12").unwrap();
        for var in ["CLAUDE_CODE_SESSION_ID", "CLAUDE_SESSION_ID"] {
            std::env::set_var(var, "sess-1");
            assert_eq!(resolve_handle(), "cc-smooth-ab12", "{var} must resolve the recorded handle");
            // A session id with no recorded file falls back rather than erroring.
            std::env::set_var(var, "sess-missing");
            assert_eq!(resolve_handle(), fallback);
            std::env::remove_var(var);
        }
        assert_eq!(session_handle(), None, "nothing to identify the caller is None, not a guess");
        std::env::set_var("CLAUDE_SESSION_ID", "sess-1");

        // 2. $SMOOTH_AGENT beats the session file.
        std::env::set_var("SMOOTH_AGENT", "from-env");
        assert_eq!(resolve_handle(), "from-env");

        // 1. $SMOOTH_AGENT_HANDLE beats everything.
        std::env::set_var("SMOOTH_AGENT_HANDLE", "worker-1");
        assert_eq!(resolve_handle(), "worker-1");

        // Blank env vars are ignored, not honored as an empty handle.
        std::env::set_var("SMOOTH_AGENT_HANDLE", "   ");
        assert_eq!(resolve_handle(), "from-env");
    }

    #[test]
    fn claim_rewrites_matching_session_files_only() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = tempfile::tempdir().unwrap();
        let _g = EnvGuard::clear();

        let dir = tmp.path().join("agent-sessions");
        std::env::set_var("SMOOTH_AGENT_SESSIONS_DIR", &dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine"), "cc-old").unwrap();
        std::fs::write(dir.join("someone-else"), "other-agent").unwrap();

        assert_eq!(rewrite_session_handles("cc-old", "fix-auth"), 1);
        assert_eq!(std::fs::read_to_string(dir.join("mine")).unwrap(), "fix-auth");
        assert_eq!(std::fs::read_to_string(dir.join("someone-else")).unwrap(), "other-agent");
        // Same-name is a no-op, and a missing directory is zero, not an error.
        assert_eq!(rewrite_session_handles("x", "x"), 0);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(rewrite_session_handles("a", "b"), 0);
    }

    #[test]
    fn supervised_pid_is_opt_in() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _g = EnvGuard::clear();
        assert_eq!(supervised_pid(), None, "th's own pid must never be recorded");
        std::env::set_var("SMOOTH_AGENT_PID", "4242");
        assert_eq!(supervised_pid(), Some(4242));
        std::env::set_var("SMOOTH_AGENT_PID", "not-a-pid");
        assert_eq!(supervised_pid(), None);
    }

    /// Point the mail store at a private sqlite file for the duration of a
    /// test. Both vars are process-global, so callers hold `ENV_LOCK`.
    struct StoreGuard;

    impl StoreGuard {
        fn at(dir: &std::path::Path) -> Self {
            std::env::set_var("SMOOTH_MAIL_DB", dir.join("mail.db"));
            // A missing config file means the default (sqlite) — this keeps a
            // developer whose real backend is `cloud` from taking the test
            // over the network.
            std::env::set_var("SMOOTH_MAIL_CONFIG", dir.join("mail.toml"));
            Self
        }
    }

    impl Drop for StoreGuard {
        fn drop(&mut self) {
            std::env::remove_var("SMOOTH_MAIL_DB");
            std::env::remove_var("SMOOTH_MAIL_CONFIG");
        }
    }

    fn register(name: &str, force: bool) -> AgentCommands {
        AgentCommands::Register {
            name: Some(name.to_string()),
            harness: Some("test".to_string()),
            pid: None,
            force,
            no_push: false,
        }
    }

    /// th-fa9f40: the incident in one test. A session that already answers to
    /// one handle must not quietly acquire a second one — that is how mail
    /// went to `cc-smooai-cc9e` while the agent watched `smooai-claude`.
    #[tokio::test]
    // ENV_LOCK is held across awaits ON PURPOSE: it serializes tests that
    // mutate process-global env (SMOOTH_MAIL_DB, SMOOTH_AGENT_HANDLE).
    #[allow(clippy::await_holding_lock)]
    async fn register_refuses_a_second_identity_for_the_session() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = tempfile::tempdir().unwrap();
        let _g = EnvGuard::clear();
        let _s = StoreGuard::at(tmp.path());
        std::env::set_var("SMOOTH_AGENT_HANDLE", "cc-smooai-cc9e");

        let err = cmd_agent(register("smooai-claude", false)).await.expect_err("must refuse");
        let err = err.to_string();
        assert!(
            err.contains("cc-smooai-cc9e") && err.contains("th agent claim"),
            "must name the handle and the fix: {err}"
        );

        // Re-registering the SAME handle is the SessionStart hook's path and
        // stays idempotent; --force is the deliberate escape hatch.
        cmd_agent(register("cc-smooai-cc9e", false)).await.expect("same handle is idempotent");
        cmd_agent(register("smooai-claude", true)).await.expect("--force proceeds");
    }

    /// th-ad0701: a store that cannot be opened is an ERROR, never an empty
    /// inbox. `main` propagates it, so the process exits non-zero.
    #[tokio::test]
    // ENV_LOCK held across awaits on purpose — see above.
    #[allow(clippy::await_holding_lock)]
    async fn a_broken_store_is_an_error_not_an_empty_inbox() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = tempfile::tempdir().unwrap();
        let _g = EnvGuard::clear();
        let _s = StoreGuard::at(tmp.path());
        // A directory where the db file should be: open fails, exactly as a
        // full disk fails when applying the schema.
        std::fs::create_dir(tmp.path().join("mail.db")).unwrap();

        assert!(
            cmd_msg(MsgCommands::Inbox {
                agent: Some("someone".into()),
                unread: false,
                mark_read: false,
                limit: 50,
                json: false,
                pull: false,
            })
            .await
            .is_err(),
            "an unopenable mail store must fail, not print an empty inbox"
        );
        assert!(cmd_inbox().await.is_err(), "th inbox must fail the same way");
    }

    #[test]
    fn default_harness_falls_back() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _g = EnvGuard::clear();
        assert_eq!(default_harness(), "unknown");
        std::env::set_var("SMOOTH_HARNESS", "claude-code");
        assert_eq!(default_harness(), "claude-code");
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap::Parser;

    /// Mirrors the real `th` shape closely enough to parse `agent`/`msg`.
    #[derive(Parser)]
    #[command(name = "th")]
    struct TestCli {
        #[command(subcommand)]
        cmd: TestCommands,
    }

    #[derive(Subcommand)]
    enum TestCommands {
        #[command(visible_alias = "agents")]
        Agent {
            #[command(subcommand)]
            cmd: AgentCommands,
        },
        #[command(visible_alias = "msgs")]
        Msg {
            #[command(subcommand)]
            cmd: MsgCommands,
        },
    }

    fn parse(args: &[&str]) -> TestCli {
        TestCli::try_parse_from(args).expect("parse")
    }

    #[test]
    fn send_accepts_positional_and_flag_forms() {
        let TestCommands::Msg {
            cmd: MsgCommands::Send { to_pos, body_pos, .. },
        } = parse(&["th", "msg", "send", "bob", "hello", "there"]).cmd
        else {
            panic!("expected send")
        };
        assert_eq!(to_pos.as_deref(), Some("bob"));
        assert_eq!(body_pos.join(" "), "hello there");

        let TestCommands::Msg {
            cmd: MsgCommands::Send { to, body, .. },
        } = parse(&["th", "msg", "send", "--to", "bob", "--body", "hi"]).cmd
        else {
            panic!("expected send")
        };
        assert_eq!(to.as_deref(), Some("bob"));
        assert_eq!(body.as_deref(), Some("hi"));
    }

    #[test]
    fn flags_after_a_positional_body_are_still_flags() {
        // Regression: `trailing_var_arg` swallowed `--type`/`--priority` into
        // the body, so `th msg send bob hi --type request` sent the literal
        // text "hi --type request" as a plain note.
        let TestCommands::Msg {
            cmd: MsgCommands::Send { body_pos, kind, priority, .. },
        } = parse(&["th", "msg", "send", "bob", "standup", "in", "5", "--type", "request", "--priority", "2"]).cmd
        else {
            panic!("expected send")
        };
        assert_eq!(body_pos.join(" "), "standup in 5");
        assert_eq!(kind.parse::<MessageKind>().unwrap(), MessageKind::Request);
        assert_eq!(priority, 2);
    }

    #[test]
    fn register_takes_an_explicit_supervisor_pid() {
        let TestCommands::Agent {
            cmd: AgentCommands::Register { pid, .. },
        } = parse(&["th", "agent", "register", "--name", "x", "--pid", "991"]).cmd
        else {
            panic!("expected register")
        };
        assert_eq!(pid, Some(991));
    }

    #[test]
    fn send_accepts_type_priority_and_deprecated_no_push() {
        let TestCommands::Msg {
            cmd: MsgCommands::Send { kind, priority, no_push, .. },
        } = parse(&[
            "th",
            "msg",
            "send",
            "--to",
            "b",
            "--body",
            "x",
            "--type",
            "handoff",
            "--priority",
            "3",
            "--no-push",
        ])
        .cmd
        else {
            panic!("expected send")
        };
        assert_eq!(kind.parse::<MessageKind>().unwrap(), MessageKind::Handoff);
        assert_eq!(priority, 3);
        assert!(no_push, "the SessionStart hook still passes --no-push; it must parse");
    }

    #[test]
    fn deprecated_pull_flags_still_parse() {
        assert!(matches!(
            parse(&["th", "msg", "inbox", "--pull", "--unread"]).cmd,
            TestCommands::Msg {
                cmd: MsgCommands::Inbox { pull: true, unread: true, .. }
            }
        ));
        assert!(matches!(
            parse(&["th", "msg", "watch", "--no-pull", "--once", "--json"]).cmd,
            TestCommands::Msg {
                cmd: MsgCommands::Watch {
                    no_pull: true,
                    once: true,
                    json: true,
                    ..
                }
            }
        ));
        assert!(matches!(
            parse(&["th", "agent", "register", "--name", "x", "--no-push"]).cmd,
            TestCommands::Agent {
                cmd: AgentCommands::Register { no_push: true, .. }
            }
        ));
    }

    #[test]
    fn read_is_an_alias_of_ack() {
        let TestCommands::Msg {
            cmd: MsgCommands::Ack { ids, all, .. },
        } = parse(&["th", "msg", "read", "msg-abc12345"]).cmd
        else {
            panic!("expected ack")
        };
        assert_eq!(ids, vec!["msg-abc12345"]);
        assert!(!all);
        assert!(matches!(
            parse(&["th", "msg", "ack", "--all"]).cmd,
            TestCommands::Msg {
                cmd: MsgCommands::Ack { all: true, .. }
            }
        ));
    }

    #[test]
    fn plural_aliases_resolve() {
        assert!(matches!(parse(&["th", "agents", "list"]).cmd, TestCommands::Agent { .. }));
        assert!(matches!(parse(&["th", "msgs", "inbox"]).cmd, TestCommands::Msg { .. }));
    }

    #[test]
    fn agent_status_and_claim_parse() {
        let TestCommands::Agent {
            cmd: AgentCommands::Status { status, task, .. },
        } = parse(&["th", "agent", "status", "--status", "working", "--task", "th-374f85"]).cmd
        else {
            panic!("expected status")
        };
        assert_eq!(status.parse::<AgentStatus>().unwrap(), AgentStatus::Working);
        assert_eq!(task.as_deref(), Some("th-374f85"));

        let TestCommands::Agent {
            cmd: AgentCommands::Claim { name },
        } = parse(&["th", "agent", "claim", "fix-auth"]).cmd
        else {
            panic!("expected claim")
        };
        assert_eq!(name, "fix-auth");
    }
}

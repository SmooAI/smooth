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
use smooth_pearls::{AgentStatus, MailMessage, MailStore, MessageKind};

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

/// This session's handle: `$SMOOTH_AGENT_HANDLE`, else `$SMOOTH_AGENT`, else
/// whatever the SessionStart hook recorded for `$CLAUDE_SESSION_ID`, else
/// `user@short-hostname`.
#[must_use]
pub fn resolve_handle() -> String {
    if let Some(h) = env_handle("SMOOTH_AGENT_HANDLE").or_else(|| env_handle("SMOOTH_AGENT")) {
        return h;
    }
    if let Some(sid) = env_handle("CLAUDE_SESSION_ID") {
        if let Ok(h) = std::fs::read_to_string(session_state_dir().join(sid)) {
            let h = h.trim().to_string();
            if !h.is_empty() {
                return h;
            }
        }
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

fn store() -> Result<MailStore> {
    MailStore::open_default()
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
pub fn cmd_agent(cmd: AgentCommands) -> Result<()> {
    let s = store()?;
    match cmd {
        AgentCommands::Register { name, harness, pid, no_push } => {
            if no_push {
                deprecated("--no-push");
            }
            let name = name.unwrap_or_else(resolve_handle);
            let harness = harness.unwrap_or_else(default_harness);
            s.register(&name, &harness, pid.or_else(supervised_pid), &cwd())?;
            println!("{} registered as {} ({})", "✓".green().bold(), name.green().bold(), harness.dimmed());
            println!("  {} continuously check: {}", "→".dimmed(), format!("th msg watch --agent {name}").cyan());
        }
        AgentCommands::List { json } => {
            let agents = s.list_agents()?;
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
            let registered = s.get_agent(&handle)?;
            let unread = s.unread_count(&handle)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "handle": handle,
                        "store": smooth_pearls::mail_store::default_path().display().to_string(),
                        "registered": registered.is_some(),
                        "agent": registered,
                        "unread": unread,
                    }))?
                );
            } else {
                println!("{} {}", "handle".dimmed(), handle.green().bold());
                println!(
                    "{}  {}",
                    "store ".dimmed(),
                    smooth_pearls::mail_store::default_path().display().to_string().dimmed()
                );
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
            s.set_status(&name, status, task.as_deref())?;
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
            if previous != name && s.get_agent(&previous)?.is_some() && s.get_agent(&name)?.is_none() {
                s.rename(&previous, &name)?;
                println!(
                    "{} claimed {} (was {}, mail carried over)",
                    "✓".green().bold(),
                    name.green().bold(),
                    previous.dimmed()
                );
            } else {
                s.register(&name, &default_harness(), supervised_pid(), &cwd())?;
                println!("{} claimed {}", "✓".green().bold(), name.green().bold());
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
            s.rename(&from, &to)?;
            rewrite_session_handles(&from, &to);
            println!("{} {} renamed to {}", "✓".green().bold(), from.dimmed(), to.green().bold());
        }
        AgentCommands::Offline { name } => {
            let name = name.unwrap_or_else(resolve_handle);
            s.set_status(&name, AgentStatus::Offline, None)?;
            println!("{} {} marked offline", "✓".green().bold(), name);
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
pub fn cmd_msg(cmd: MsgCommands) -> Result<()> {
    let s = store()?;
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
                Some(ref rid) => s.get_message(rid)?.map(|m| m.thread_root().to_string()),
                None => None,
            };
            let id = s.send(&from, &to, &body, kind, priority, thread_id.as_deref())?;
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
            let _ = s.touch(&who); // heartbeat (best-effort)
            let msgs = s.inbox(&who, unread, limit)?;
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
                    s.ack(&who, &m.id)?;
                }
            }
        }
        MsgCommands::Ack { ids, all, agent } => {
            let who = agent.unwrap_or_else(resolve_handle);
            if all {
                let n = s.ack_all(&who)?;
                println!("{} acknowledged {n} message(s) for {who}", "✓".green().bold());
            } else if ids.is_empty() {
                bail!("nothing to acknowledge — pass message ids or --all");
            } else {
                for id in &ids {
                    s.ack(&who, id)?;
                }
                println!("{} acknowledged {}", "✓".green().bold(), ids.join(", ").dimmed());
            }
        }
        MsgCommands::Reply {
            id,
            body,
            from,
            kind,
            priority,
        } => {
            let from = from.unwrap_or_else(resolve_handle);
            let Some(orig) = s.get_message(&id)? else {
                bail!("no message {id}");
            };
            let root = orig.thread_root().to_string();
            let to = orig.from_agent;
            let new_id = s.send(&from, &to, &body, kind.parse()?, priority, Some(&root))?;
            // Replying is reading: don't leave the message we just answered unread.
            s.ack(&from, &id)?;
            println!("{} replied {} to {}", "✓".green().bold(), new_id.dimmed(), to.green());
        }
        MsgCommands::Thread { id } => {
            let Some(m) = s.get_message(&id)? else {
                bail!("no message {id}");
            };
            let thread = s.thread(m.thread_root())?;
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
                let _ = s.touch(&who);
                match s.inbox(&who, true, 200) {
                    Ok(msgs) if !msgs.is_empty() => {
                        print_messages(&msgs, json)?;
                        if once {
                            // Leave the mail UNREAD: the caller decides when it
                            // has actually been handled (`th msg ack`), so a
                            // dropped watcher cycle never loses a message.
                            return Ok(());
                        }
                        for m in &msgs {
                            s.ack(&who, &m.id)?; // consume so it doesn't repeat
                        }
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("{} inbox poll failed: {e}", "!".yellow()),
                }
                std::thread::sleep(interval);
            }
        }
    }
    Ok(())
}

/// `th inbox` — convenience alias for `th msg inbox`.
///
/// # Errors
/// Returns an error if the store can't be opened or the query fails.
pub fn cmd_inbox() -> Result<()> {
    cmd_msg(MsgCommands::Inbox {
        agent: None,
        unread: false,
        mark_read: false,
        limit: 50,
        json: false,
        pull: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `resolve_handle` and `rewrite_session_handles` both read process-global
    /// env vars, so these tests must not interleave with each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// The vars these tests own. `$HOME` is deliberately NOT among them: it is
    /// read by unrelated tests in this binary, which run in parallel.
    const OWNED_VARS: [&str; 6] = [
        "SMOOTH_AGENT_HANDLE",
        "SMOOTH_AGENT",
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

        // 3. A session-state file wins over the fallback.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sess-1"), "cc-smooth-ab12").unwrap();
        std::env::set_var("CLAUDE_SESSION_ID", "sess-1");
        assert_eq!(resolve_handle(), "cc-smooth-ab12");
        // A session id with no recorded file falls back rather than erroring.
        std::env::set_var("CLAUDE_SESSION_ID", "sess-missing");
        assert_eq!(resolve_handle(), fallback);
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

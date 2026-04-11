//! `th` — Smoo AI CLI entry point.
//!
//! Single binary for agent orchestration, config management, and platform tools.

mod hooks;

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dialoguer::{theme::ColorfulTheme, Input, Password, Select};
use owo_colors::OwoColorize;

/// Smooth — AI agent orchestration platform.
/// Run with no arguments to launch the interactive coding assistant.
#[derive(Parser)]
#[command(name = "th", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start Smooth platform
    Up {
        /// Skip starting the leader service
        #[arg(long)]
        no_leader: bool,
        /// Leader port
        #[arg(long, default_value = "4400")]
        port: u16,
        /// Run in foreground (default: daemonize)
        #[arg(long)]
        foreground: bool,
    },
    /// Stop Smooth platform
    Down,
    /// Show system health
    Status,
    /// Provider authentication
    Auth {
        #[command(subcommand)]
        cmd: AuthCommands,
    },
    /// Trigger work on a bead
    Run { bead_id: String },
    /// Pause a running Smooth Operator
    Pause { bead_id: String },
    /// Resume a paused Smooth Operator
    Resume { bead_id: String },
    /// Send guidance to a running Smooth Operator
    Steer { bead_id: String, message: String },
    /// Cancel a running Smooth Operator
    Cancel { bead_id: String },
    /// Approve a pending review
    Approve { bead_id: String },
    /// Show messages requiring attention
    Inbox,
    /// Smooth Operator management
    Operators,
    /// Project management
    Project {
        #[command(subcommand)]
        cmd: ProjectCommands,
    },
    /// View/set local configuration
    Config {
        #[command(subcommand)]
        cmd: ConfigCommands,
    },
    /// Database management
    Db {
        #[command(subcommand)]
        cmd: DbCommands,
    },
    /// Jira integration
    Jira {
        #[command(subcommand)]
        cmd: JiraCommands,
    },
    /// SmooAI platform tools
    Smoo {
        #[command(subcommand)]
        cmd: SmooCommands,
    },
    /// View audit logs
    Audit {
        #[command(subcommand)]
        cmd: AuditCommands,
    },
    /// Open web interface
    Web,
    /// Git worktree management
    Worktree {
        #[command(subcommand)]
        cmd: WorktreeCommands,
    },
    /// Tailscale integration
    Tailscale {
        #[command(subcommand)]
        cmd: TailscaleCommands,
    },
    /// Operator access control
    Access {
        #[command(subcommand)]
        cmd: AccessCommands,
    },
    /// Launch interactive coding assistant (same as running th with no args)
    Code {
        /// Run in headless mode (non-interactive)
        #[arg(long)]
        headless: bool,
        /// Message to send (headless mode)
        #[arg(long)]
        message: Option<String>,
        /// Read message from file
        #[arg(long)]
        file: Option<String>,
        /// Model to use
        #[arg(long)]
        model: Option<String>,
        /// Budget limit in USD
        #[arg(long)]
        budget: Option<f64>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Git hook management (install, run).
    Hooks {
        #[command(subcommand)]
        cmd: HooksCommands,
    },
    /// Pearl tracking (built-in work-item tracker).
    ///
    /// Lineage: beads → issues → pearls. There is no alias — pearls is the
    /// only spelling.
    Pearls {
        #[command(subcommand)]
        cmd: PearlCommands,
    },
    /// Configure per-activity model routing (which model for thinking, coding, etc.)
    Routing {
        #[command(subcommand)]
        cmd: RoutingCommands,
    },
    /// System health check and auto-fix
    Doctor,
}

#[derive(Subcommand)]
enum RoutingCommands {
    /// Show current routing configuration
    Show,
    /// Apply a preset routing configuration
    Preset {
        /// Preset name: low-cost, codex, anthropic
        name: Option<String>,
    },
    /// Set routing for a specific activity
    Set {
        /// Activity: thinking, coding, planning, reviewing, judge, summarize
        activity: String,
        /// Model in provider/model format (e.g. openrouter/deepseek/deepseek-v3.2)
        model: String,
    },
}

#[derive(Subcommand)]
enum AuthCommands {
    /// Add or update a provider
    Login {
        /// Provider: kimi-code, kimi, openrouter, openai, anthropic, ollama, google
        provider: Option<String>,
        /// API key
        #[arg(long)]
        api_key: Option<String>,
    },
    /// List configured providers
    Providers,
    /// Get or set default provider
    Default { provider: Option<String> },
    /// Remove a provider
    Remove { provider: String },
    /// Show authentication status
    Status,
}

#[derive(Subcommand)]
enum ProjectCommands {
    /// Create a project
    Create { name: String, description: Option<String> },
    /// List projects
    List,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Show configuration
    Show,
    /// Set a config value
    Set { key: String, value: String },
}

#[derive(Subcommand)]
enum DbCommands {
    /// Show database status
    Status,
    /// Backup database
    Backup,
    /// Show database path
    Path,
}

#[derive(Subcommand)]
enum JiraCommands {
    /// Sync with Jira
    Sync,
    /// Show Jira status
    Status,
}

#[derive(Subcommand)]
enum SmooCommands {
    /// Config schema management
    Config {
        #[command(subcommand)]
        cmd: SmooConfigCommands,
    },
    /// List SmooAI agents
    Agents,
}

#[derive(Subcommand)]
enum SmooConfigCommands {
    Push,
    Pull,
    Set { key: String, value: String },
    Get { key: String },
    List,
    Diff,
}

#[derive(Subcommand)]
enum AuditCommands {
    /// Show recent audit log entries
    Tail {
        actor: Option<String>,
        #[arg(short, long, default_value = "50")]
        lines: usize,
    },
    /// List actors with audit logs
    List,
    /// Show audit log directory
    Path,
}

#[derive(Subcommand)]
enum WorktreeCommands {
    /// Create a worktree
    Create { branch: String },
    /// List worktrees
    List,
    /// Remove a worktree
    Remove { branch: String },
    /// Merge a worktree to main
    Merge { branch: String },
}

#[derive(Subcommand)]
enum TailscaleCommands {
    /// Show Tailscale status
    Status,
}

#[derive(Subcommand)]
enum AccessCommands {
    /// List pending access requests
    Pending,
    /// Approve domain access for a bead
    Approve {
        /// Bead ID
        bead: String,
        /// Domain to approve
        domain: String,
    },
    /// Deny domain access for a bead
    Deny {
        /// Bead ID
        bead: String,
        /// Domain to deny
        domain: String,
    },
    /// Show current policy for an operator
    Policy {
        /// Operator ID
        operator_id: String,
    },
}

#[derive(Subcommand)]
enum HooksCommands {
    /// Install git hooks (.githooks/) with cargo quality gates + pearl integration
    Install,
    /// Run pearl-specific hook logic (called from .githooks/ scripts)
    Run {
        /// Hook name: pre-commit, pre-push, prepare-commit-msg, post-checkout, post-merge
        hook: String,
        /// Arguments passed by git to the hook
        args: Vec<String>,
    },
    /// Check if hooks are properly installed
    Status,
}

#[derive(Subcommand)]
enum PearlCommands {
    /// Create a new issue
    Create {
        #[arg(long)]
        title: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, default_value = "task")]
        r#type: String,
        #[arg(long, default_value = "2")]
        priority: u8,
        #[arg(long)]
        label: Vec<String>,
    },
    /// List pearls
    List {
        #[arg(long)]
        status: Option<String>,
    },
    /// Show issue details
    Show { id: String },
    /// Update an issue
    Update {
        id: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, alias = "desc")]
        description: Option<String>,
        #[arg(long)]
        priority: Option<u8>,
        #[arg(long)]
        assign: Option<String>,
    },
    /// Close pearls
    Close { ids: Vec<String> },
    /// Reopen an issue
    Reopen { id: String },
    /// Add dependency
    Dep {
        #[command(subcommand)]
        cmd: DepCommands,
    },
    /// Add comment
    Comment { id: String, content: String },
    /// Search pearls
    Search { query: String },
    /// Show statistics
    Stats,
    /// Show ready pearls (open, no blockers)
    Ready,
    /// Show blocked pearls
    Blocked,
    /// Add/remove labels
    Label {
        id: String,
        #[command(subcommand)]
        cmd: LabelCommands,
    },
    /// Initialize a Dolt pearl database in this repo (.smooth/dolt/)
    Init,
    /// Show Dolt commit history for pearls
    Log {
        /// Number of entries to show
        #[arg(short, default_value = "20")]
        n: usize,
    },
    /// Push pearl data to git remote (refs/dolt/data)
    Push,
    /// Pull pearl data from git remote
    Pull,
    /// Manage Dolt remotes for pearl sync
    Remote {
        #[command(subcommand)]
        cmd: RemoteCommands,
    },
    /// Garbage collect the pearl database (compact for git)
    Gc,
    /// Migrate from beads
    MigrateFromBeads,
    /// Migrate pearls from legacy SQLite (smooth.db) into Dolt
    MigrateFromSqlite,
    /// List all registered pearl projects
    Projects,
}

#[derive(Subcommand)]
enum DepCommands {
    /// Add a dependency (issue depends on blocker)
    Add { issue: String, depends_on: String },
    /// Remove a dependency
    Remove { issue: String, depends_on: String },
}

#[derive(Subcommand)]
enum LabelCommands {
    /// Add a label
    Add { label: String },
    /// Remove a label
    Remove { label: String },
}

#[derive(Subcommand)]
enum RemoteCommands {
    /// Add a Dolt remote (e.g., git origin URL)
    Add { name: String, url: String },
    /// List configured remotes
    List,
    /// Remove a remote
    Remove { name: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("smooth=info".parse()?))
        .init();

    let cli = Cli::parse();

    match cli.command {
        // No subcommand = launch smooth-code (THE Smooth experience)
        None => cmd_code(false, None, None, None, None, false).await,
        Some(Commands::Code {
            headless,
            message,
            file,
            model,
            budget,
            json,
        }) => cmd_code(headless, message, file, model, budget, json).await,
        Some(Commands::Doctor) => cmd_doctor().await,
        Some(Commands::Up { no_leader, port, foreground }) => cmd_up(no_leader, port, foreground).await,
        Some(Commands::Down) => cmd_down().await,
        Some(Commands::Status) => cmd_status().await,
        Some(Commands::Db { cmd }) => cmd_db(cmd),
        Some(Commands::Auth { cmd }) => cmd_auth(cmd).await,
        Some(Commands::Operators) => cmd_operators().await,
        Some(Commands::Inbox) => cmd_inbox().await,
        Some(Commands::Run { bead_id }) => cmd_run(&bead_id).await,
        Some(Commands::Approve { bead_id }) => cmd_approve(&bead_id).await,
        Some(Commands::Pause { bead_id }) => cmd_steer(&bead_id, "pause", None).await,
        Some(Commands::Resume { bead_id }) => cmd_steer(&bead_id, "resume", None).await,
        Some(Commands::Steer { bead_id, message }) => cmd_steer(&bead_id, "steer", Some(&message)).await,
        Some(Commands::Cancel { bead_id }) => cmd_steer(&bead_id, "cancel", None).await,
        Some(Commands::Hooks { cmd }) => cmd_hooks(cmd),
        Some(Commands::Pearls { cmd }) => cmd_pearls(cmd).await,
        Some(Commands::Audit { cmd }) => cmd_audit(cmd),
        Some(Commands::Web) => {
            println!("Web UI: http://localhost:4400");
            println!("Start with: th up");
            Ok(())
        }
        Some(Commands::Worktree { cmd }) => cmd_worktree(cmd),
        Some(Commands::Tailscale { cmd }) => cmd_tailscale(cmd),
        Some(Commands::Access { cmd }) => cmd_access(cmd).await,
        Some(Commands::Jira { cmd }) => cmd_jira(cmd).await,
        Some(Commands::Routing { cmd }) => cmd_routing(cmd).await,
        Some(_) => {
            println!("Command not yet implemented. Coming soon!");
            Ok(())
        }
    }
}

// ── Command implementations ────────────────────────────────

/// PID file for the daemon process.
fn pid_file_path() -> std::path::PathBuf {
    dirs_next::home_dir().unwrap_or_default().join(".smooth").join("smooth.pid")
}

/// Log file for daemon output.
fn log_file_path() -> std::path::PathBuf {
    dirs_next::home_dir().unwrap_or_default().join(".smooth").join("smooth.log")
}

async fn cmd_up(no_leader: bool, port: u16, foreground: bool) -> Result<()> {
    // Daemon mode: re-exec ourselves with --foreground and redirect output to log file
    if !foreground {
        // Check if already running
        let pid_path = pid_file_path();
        if pid_path.exists() {
            if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
                if let Ok(pid) = pid_str.trim().parse::<u32>() {
                    // Check if process is still alive
                    let alive = std::process::Command::new("kill")
                        .args(["-0", &pid.to_string()])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    if alive {
                        println!();
                        println!();
                        println!("  {} {}", "●".yellow(), format!("Smooth is already running (pid {pid})").yellow());
                        println!();
                        println!("    {}  {}", "Web UI".dimmed(), format!("http://localhost:{port}").cyan().bold());
                        println!("    {}  {}", "Logs  ".dimmed(), log_file_path().display().to_string().dimmed());
                        println!("    {}  {}", "Stop  ".dimmed(), "th down".dimmed());
                        println!();
                        return Ok(());
                    }
                }
            }
            // Stale pid file — remove it
            let _ = std::fs::remove_file(&pid_path);
        }

        let log_path = log_file_path();
        // Ensure ~/.smooth/ exists
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log_file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
        let log_err = log_file.try_clone()?;

        let exe = std::env::current_exe()?;
        let mut args = vec!["up".to_string(), "--foreground".to_string(), "--port".to_string(), port.to_string()];
        if no_leader {
            args.push("--no-leader".to_string());
        }

        let child = std::process::Command::new(exe)
            .args(&args)
            .stdout(log_file)
            .stderr(log_err)
            .stdin(std::process::Stdio::null())
            .spawn()?;

        let pid = child.id();
        std::fs::write(&pid_path, pid.to_string())?;

        // The visible text (without ANSI) that goes inside the box
        let inner = format!(" Smooth started (pid {pid}) ");
        let w = inner.len() + 2; // +2 for left/right padding
        println!();
        println!("  \x1b[2m\u{256d}{}\u{256e}\x1b[0m", "\u{2500}".repeat(w));
        println!("  \x1b[2m\u{2502}\x1b[0m \x1b[32;1m{inner}\x1b[0m \x1b[2m\u{2502}\x1b[0m");
        println!("  \x1b[2m\u{2570}{}\u{256f}\x1b[0m", "\u{2500}".repeat(w));
        println!("    {}  {}", "Web UI".dimmed(), format!("http://localhost:{port}").cyan().bold());
        println!("    {}  {}", "Logs  ".dimmed(), log_path.display().to_string().dimmed());
        println!("    {}  {}", "Stop  ".dimmed(), "th down".dimmed());
        println!();
        return Ok(());
    }

    // Foreground mode — actual server startup
    println!();
    println!("  {} / {}", "Smoo AI".bold(), "Smooth".green().bold());
    println!();

    // Initialize database
    let db_path = smooth_bigsmooth::db::default_db_path();
    let db = smooth_bigsmooth::db::Database::open(&db_path)?;
    println!("  {} Database   {}", "\u{2713}".green().bold(), db_path.display().to_string().dimmed());

    // Initialize pearl store (Dolt-backed)
    let pearl_store = match find_dolt_dir() {
        Ok(dolt_dir) => {
            let store = smooth_pearls::PearlStore::open(&dolt_dir)?;
            println!("  {} Pearls     {}", "\u{2713}".green().bold(), dolt_dir.display().to_string().dimmed());
            store
        }
        Err(_) => {
            // Auto-init Dolt in cwd if no .smooth/dolt/ found
            let cwd = std::env::current_dir()?;
            let dolt_dir = cwd.join(".smooth").join("dolt");
            let store = smooth_pearls::PearlStore::init(&dolt_dir)?;
            println!(
                "  {} Pearls     {} {}",
                "\u{2713}".green().bold(),
                dolt_dir.display().to_string().dimmed(),
                "(auto-initialized)".dimmed()
            );
            store
        }
    };

    if no_leader {
        println!();
        println!("  {}", "Smooth infrastructure ready (leader skipped).".green());
        return Ok(());
    }

    // Start leader (API + embedded web UI on same port)
    let state = smooth_bigsmooth::server::AppState::new(db, pearl_store);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!(
        "  {} Leader     {}",
        "\u{2713}".green().bold(),
        format!("http://localhost:{port}").cyan().bold()
    );
    println!(
        "  {} Web UI     {}",
        "\u{2713}".green().bold(),
        format!("http://localhost:{port}").cyan().bold()
    );
    println!();

    smooth_bigsmooth::server::start(state, addr).await
}

async fn cmd_down() -> Result<()> {
    let pid_path = pid_file_path();
    if !pid_path.exists() {
        println!("  {}", "Smooth is not running.".yellow());
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(&pid_path)?;
    let pid: u32 = pid_str.trim().parse().context("invalid pid file")?;

    // Send SIGTERM
    let status = std::process::Command::new("kill").arg(pid.to_string()).status()?;

    let pid_tag_owned = format!("(pid {pid})");
    let pid_tag = pid_tag_owned.dimmed();
    if status.success() {
        println!("  \u{1f534} {} {pid_tag}", "Smooth stopped".green().bold());
    } else {
        println!("  {} {pid_tag}", "Cleaning up stale pid file".dimmed());
    }
    std::fs::remove_file(&pid_path)?;
    Ok(())
}

async fn cmd_status() -> Result<()> {
    let url = "http://localhost:4400/health";
    match reqwest::get(url).await {
        Ok(resp) => {
            let body: serde_json::Value = resp.json().await?;

            // Version
            let version = body["version"].as_str().unwrap_or("unknown");
            println!();
            println!(
                "  {} {} {} {}",
                "Smooth".bold(),
                format!("v{version}").bold().green(),
                "\u{2014}".dimmed(),
                "http://localhost:4400".cyan().bold()
            );

            // Uptime
            if let Some(uptime_secs) = body["uptime_seconds"].as_u64().or_else(|| body["uptime"].as_u64()) {
                let formatted = if uptime_secs >= 3600 {
                    format!("{}h {}m", uptime_secs / 3600, (uptime_secs % 3600) / 60)
                } else if uptime_secs >= 60 {
                    format!("{}m {}s", uptime_secs / 60, uptime_secs % 60)
                } else {
                    format!("{uptime_secs}s")
                };
                println!("  {}: {}", "Uptime".dimmed(), formatted);
            }
            println!();

            // Leader
            let leader_status = body["leader"].as_str().or_else(|| body["status"].as_str()).unwrap_or("healthy");
            let (icon, label) = status_indicator(leader_status);
            println!("  {icon} {:<12} {label}", "Leader");

            // Database
            let db_status = body["database"].as_str().unwrap_or("healthy");
            let (icon, label) = status_indicator(db_status);
            println!("  {icon} {:<12} {} {}", "Database", label, "(SQLite)".dimmed());

            // Sandbox
            let sandbox_status = body["sandbox"].as_str().or_else(|| body["sandboxes"].as_str()).unwrap_or("healthy");
            let active = body["sandbox_active"].as_u64().or_else(|| body["sandboxes_active"].as_u64()).unwrap_or(0);
            let max = body["sandbox_max"].as_u64().or_else(|| body["sandboxes_max"].as_u64()).unwrap_or(3);
            let (icon, label) = status_indicator(sandbox_status);
            println!("  {icon} {:<12} {} {}", "Sandbox", label, format!("({active}/{max} active)").dimmed());

            // Tailscale
            if let Some(ts) = body.get("tailscale") {
                let ts_status = ts.as_str().unwrap_or("unknown");
                let hostname = body["tailscale_hostname"].as_str().unwrap_or("");
                let (icon, label) = status_indicator(ts_status);
                let suffix = if hostname.is_empty() { String::new() } else { format!(" ({})", hostname) };
                println!("  {icon} {:<12} {label}{}", "Tailscale", suffix.dimmed());
            }

            // Pearls
            if let Ok(store) = open_pearl_store() {
                if let Ok(stats) = store.stats() {
                    println!(
                        "  {} {:<12} {} open, {} active, {} closed",
                        "\u{2713}".green().bold(),
                        "Pearls",
                        stats.open.to_string().bold(),
                        stats.in_progress.to_string().bold(),
                        stats.closed.to_string().dimmed()
                    );
                }
            }
            println!();
        }
        Err(_) => {
            println!();
            println!("  {}", "Smooth is not running.".yellow());
            println!("  Start with: {}", "th up".bold());
            println!();
        }
    }
    Ok(())
}

/// Return a colored status indicator (icon, colored label) for health status strings.
fn status_indicator(status: &str) -> (String, String) {
    match status {
        "healthy" | "running" | "connected" | "ok" => ("\u{2713}".green().bold().to_string(), "healthy".green().to_string()),
        "degraded" | "warning" => ("\u{26a0}".yellow().bold().to_string(), "degraded".yellow().to_string()),
        _ => ("\u{2717}".red().bold().to_string(), status.red().to_string()),
    }
}

fn cmd_db(cmd: DbCommands) -> Result<()> {
    let db_path = smooth_bigsmooth::db::default_db_path();
    match cmd {
        DbCommands::Status => {
            if db_path.exists() {
                let metadata = std::fs::metadata(&db_path)?;
                println!("Database: {}", db_path.display());
                println!("Size: {:.1} KB", metadata.len() as f64 / 1024.0);
            } else {
                println!("Database not created yet. Run: th up");
            }
        }
        DbCommands::Path => println!("{}", db_path.display()),
        DbCommands::Backup => {
            if !db_path.exists() {
                println!("No database to backup.");
                return Ok(());
            }
            let backup_dir = dirs_next::home_dir().unwrap_or_default().join(".smooth").join("backups");
            std::fs::create_dir_all(&backup_dir)?;
            let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S");
            let backup_path = backup_dir.join(format!("smooth-{timestamp}.db"));
            std::fs::copy(&db_path, &backup_path)?;
            println!("Backup saved to: {}", backup_path.display());
        }
    }
    Ok(())
}

async fn cmd_auth(cmd: AuthCommands) -> Result<()> {
    let providers_path = dirs_next::home_dir().map(|h| h.join(".smooth/providers.json"));

    match cmd {
        AuthCommands::Status => {
            println!();
            println!("  {}", "Auth Status".bold().cyan());
            println!();

            // Check providers.json for configured providers
            if let Some(ref path) = providers_path {
                if path.exists() {
                    match smooth_operator::providers::ProviderRegistry::load_from_file(path) {
                        Ok(registry) => {
                            let providers = registry.list_providers();
                            if providers.is_empty() {
                                println!(
                                    "  {} {:<12} {}",
                                    "\u{2717}".red().bold(),
                                    "Providers",
                                    "none configured \u{2014} run: th auth login <provider>".red()
                                );
                            } else {
                                println!(
                                    "  {} {:<12} {} configured ({})",
                                    "\u{2713}".green().bold(),
                                    "Providers",
                                    providers.len().to_string().green().bold(),
                                    providers.join(", ")
                                );
                            }
                        }
                        Err(_) => {
                            println!(
                                "  {} {:<12} {}",
                                "\u{2717}".red().bold(),
                                "Providers",
                                "providers.json exists but cannot be read".red()
                            );
                        }
                    }
                } else {
                    println!(
                        "  {} {:<12} {}",
                        "\u{2717}".red().bold(),
                        "Providers",
                        "not configured \u{2014} run: th auth login <provider>".red()
                    );
                }
            }

            let leader_up = reqwest::get("http://localhost:4400/health").await.is_ok();
            if leader_up {
                println!("  {} {:<12} {}", "\u{2713}".green().bold(), "Leader", "running".green());
            } else {
                println!("  {} {:<12} {}", "\u{2717}".red().bold(), "Leader", "not running \u{2014} run: th up".red());
            }
            println!();
        }
        AuthCommands::Login { provider, api_key } => {
            let path = providers_path.as_ref().context("cannot determine home directory")?;

            // Provider catalog: (id, display name, models, needs_key)
            // First entry is the recommended default — it's surfaced at the
            // top of the picker. SmooAI Gateway is the hosted LiteLLM-backed
            // gateway run by SmooAI with billing, moderation, governance,
            // and provider routing on the server side.
            let catalog: Vec<(&str, &str, Vec<&str>, bool)> = vec![
                (
                    "smooai-gateway",
                    "SmooAI Gateway (recommended)",
                    vec![
                        "smooth-default",
                        "smooth-coding",
                        "smooth-thinking",
                        "smooth-planning",
                        "smooth-reviewing",
                        "smooth-judge",
                        "smooth-summarize",
                    ],
                    true,
                ),
                (
                    "llmgateway",
                    "LLM Gateway",
                    vec!["openai/gpt-4o", "anthropic/claude-sonnet-4", "google/gemini-2.5-flash", "deepseek/deepseek-v3"],
                    true,
                ),
                ("kimi-code", "Kimi Code", vec!["kimi-for-coding"], true),
                ("kimi", "Kimi", vec!["kimi-k2.5", "kimi-k2", "moonshot-v1-auto"], true),
                (
                    "openrouter",
                    "OpenRouter",
                    vec![
                        "deepseek/deepseek-v3",
                        "openai/gpt-4o",
                        "anthropic/claude-sonnet-4",
                        "moonshot/kimi-k2.5",
                        "google/gemini-flash-2.0",
                    ],
                    true,
                ),
                ("openai", "OpenAI", vec!["gpt-4o", "gpt-4o-mini", "o3-mini", "gpt-5.4-mini"], true),
                (
                    "anthropic",
                    "Anthropic",
                    vec!["claude-sonnet-4-20250514", "claude-opus-4-20250514", "claude-haiku-4-5-20251001"],
                    true,
                ),
                ("google", "Google AI", vec!["gemini-2.5-flash", "gemini-2.5-pro"], true),
                ("ollama", "Ollama (local)", vec!["llama3.3", "qwen3", "deepseek-r1"], false),
            ];

            // Step 1: Pick provider (interactive if not given)
            let (provider_id, models, needs_key) = if let Some(ref p) = provider {
                let entry = catalog.iter().find(|(id, ..)| *id == p.as_str());
                match entry {
                    Some((id, _, models, needs_key)) => (id.to_string(), models.clone(), *needs_key),
                    None => {
                        println!("Unknown provider: {p}");
                        println!("Available: {}", catalog.iter().map(|(id, ..)| *id).collect::<Vec<_>>().join(", "));
                        return Ok(());
                    }
                }
            } else {
                let display_names: Vec<&str> = catalog.iter().map(|(_, name, ..)| *name).collect();
                let selection = Select::with_theme(&ColorfulTheme::default())
                    .with_prompt("Select a provider")
                    .items(&display_names)
                    .default(0)
                    .interact()?;
                let (id, _, models, needs_key) = &catalog[selection];
                (id.to_string(), models.clone(), *needs_key)
            };

            // Step 2: Get API key FIRST (needed before fetching models)
            let api_key = if !needs_key {
                String::new()
            } else if let Some(k) = api_key {
                k
            } else {
                Password::with_theme(&ColorfulTheme::default()).with_prompt("API key").interact()?
            };

            // Step 3: Choose a preset or single model
            // For providers that support presets (openrouter, llmgateway), offer
            // "Apply a preset" as the first option before individual model selection.

            let provider_presets: Vec<(&str, &str, &str)> = smooth_operator::providers::Preset::ALL
                .iter()
                .filter(|(name, _, _)| {
                    name.starts_with(&provider_id)
                        || smooth_operator::providers::Preset::from_name(name)
                            .map(|p| p.provider_id() == provider_id)
                            .unwrap_or(false)
                })
                .copied()
                .collect();

            // Ask: preset or single model?
            let use_preset = if !provider_presets.is_empty() {
                let choices = vec![
                    format!(
                        "Apply a routing preset ({})",
                        provider_presets.iter().map(|(n, _, _)| *n).collect::<Vec<_>>().join(", ")
                    ),
                    "Select a single model".to_string(),
                ];
                let selection = Select::with_theme(&ColorfulTheme::default())
                    .with_prompt("Setup mode")
                    .items(&choices)
                    .default(0)
                    .interact()?;
                selection == 0
            } else {
                false
            };

            if use_preset {
                // Apply preset — save and done
                let preset_choice = if provider_presets.len() == 1 {
                    0
                } else {
                    let names: Vec<&str> = provider_presets.iter().map(|(_, title, _)| *title).collect();
                    Select::with_theme(&ColorfulTheme::default())
                        .with_prompt("Select a preset")
                        .items(&names)
                        .default(0)
                        .interact()?
                };

                let preset_name = provider_presets[preset_choice].0;
                let preset = smooth_operator::providers::Preset::from_name(preset_name).ok_or_else(|| anyhow::anyhow!("unknown preset"))?;

                let registry = smooth_operator::providers::ProviderRegistry::from_preset(preset, &api_key);
                registry.save_to_file(path)?;

                println!("\n  {} {} with {} preset", "✓".green().bold(), provider_id.green().bold(), preset_name.cyan());
                println!("  Saved to: {}\n", path.display().to_string().dimmed());

                // Show routing
                Box::pin(cmd_routing(RoutingCommands::Show)).await?;
                return Ok(());
            }

            // Single model selection
            let model = if models.len() == 1 {
                models[0].to_string()
            } else {
                let live_models = if matches!(provider_id.as_str(), "llmgateway" | "openrouter" | "ollama") {
                    let api_url = match provider_id.as_str() {
                        "llmgateway" => "https://api.llmgateway.io/v1/models",
                        "openrouter" => "https://openrouter.ai/api/v1/models",
                        "ollama" => "http://localhost:11434/v1/models",
                        _ => "",
                    };
                    if !api_url.is_empty() {
                        print!("  Fetching models... ");
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                        match reqwest::blocking::get(api_url) {
                            Ok(resp) => match resp.json::<serde_json::Value>() {
                                Ok(body) => {
                                    let ids: Vec<String> = body
                                        .get("data")
                                        .and_then(|d| d.as_array())
                                        .map(|arr| arr.iter().filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from)).collect())
                                        .unwrap_or_default();
                                    println!("{} models available", ids.len());
                                    ids
                                }
                                Err(_) => {
                                    println!("failed to parse");
                                    Vec::new()
                                }
                            },
                            Err(_) => {
                                println!("unavailable");
                                Vec::new()
                            }
                        }
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };

                let all_models: Vec<String> = if live_models.is_empty() {
                    models.iter().map(|s| s.to_string()).collect()
                } else {
                    live_models
                };

                if all_models.len() > 20 {
                    let selection = dialoguer::FuzzySelect::with_theme(&ColorfulTheme::default())
                        .with_prompt("Search and select a model")
                        .items(&all_models)
                        .default(0)
                        .interact()?;
                    all_models[selection].clone()
                } else {
                    let selection = Select::with_theme(&ColorfulTheme::default())
                        .with_prompt("Select a model")
                        .items(&all_models)
                        .default(0)
                        .interact()?;
                    all_models[selection].clone()
                }
            };

            // Step 4: Test the connection
            print!("Testing connection... ");
            let config = match provider_id.as_str() {
                "openrouter" => smooth_operator::providers::ProviderConfig::openrouter(&api_key),
                "openai" => smooth_operator::providers::ProviderConfig::openai(&api_key),
                "anthropic" => smooth_operator::providers::ProviderConfig::anthropic(&api_key),
                "kimi" => smooth_operator::providers::ProviderConfig::kimi(&api_key),
                "kimi-code" => smooth_operator::providers::ProviderConfig::kimi_code(&api_key),
                "llmgateway" => smooth_operator::providers::ProviderConfig::llmgateway(&api_key),
                "ollama" => smooth_operator::providers::ProviderConfig::ollama(),
                "google" => smooth_operator::providers::ProviderConfig::google(&api_key),
                _ => unreachable!(),
            };

            // Quick test: send a tiny request
            let test_llm = smooth_operator::llm::LlmClient::new(smooth_operator::llm::LlmConfig {
                api_url: config.api_url.clone(),
                api_key: config.api_key.clone(),
                model: model.clone(),
                max_tokens: 32,
                temperature: 0.0,
                retry_policy: smooth_operator::llm::RetryPolicy::default(),
                api_format: config.api_format.clone(),
            });
            let test_msg = smooth_operator::conversation::Message::user("Say 'ok' and nothing else.");
            match test_llm.chat(&[&test_msg], &[]).await {
                Ok(resp) => println!("{} ({})", "connected ✓".green(), resp.content.trim().chars().take(20).collect::<String>()),
                Err(e) => {
                    println!("{}", "failed ✗".red());
                    println!("  Error: {e}");
                    let proceed: bool = Input::with_theme(&ColorfulTheme::default())
                        .with_prompt("Save anyway? (y/n)")
                        .default("n".into())
                        .interact_text()
                        .map(|s: String| s.starts_with('y'))
                        .unwrap_or(false);
                    if !proceed {
                        return Ok(());
                    }
                }
            }

            // Step 5: Save
            let mut registry = if path.exists() {
                smooth_operator::providers::ProviderRegistry::load_from_file(path).unwrap_or_default()
            } else {
                smooth_operator::providers::ProviderRegistry::default()
            };

            let mut provider_config = config;
            provider_config.default_model = model;
            registry.register_provider(provider_config);

            let current_default_works = registry.default_llm_config().is_ok();
            if !current_default_works || registry.list_providers().len() == 1 {
                registry.set_default_provider(&provider_id);
            }

            registry.save_to_file(path)?;

            println!("{}: configured ✓", provider_id.green().bold());
            println!("  Saved to: {}", path.display());
        }
        AuthCommands::Providers => {
            if let Some(ref path) = providers_path {
                if path.exists() {
                    match smooth_operator::providers::ProviderRegistry::load_from_file(path) {
                        Ok(registry) => {
                            let providers = registry.list_providers();
                            if providers.is_empty() {
                                println!("No providers configured. Run: th auth login <provider>");
                            } else {
                                for id in &providers {
                                    println!("{id}: configured");
                                }
                            }
                        }
                        Err(e) => {
                            println!("Error reading providers.json: {e}");
                        }
                    }
                } else {
                    println!("No providers configured. Run: th auth login <provider>");
                }
            }
        }
        AuthCommands::Default { provider } => {
            let path = providers_path.as_ref().context("cannot determine home directory")?;
            if let Some(p) = provider {
                if !path.exists() {
                    println!("No providers configured. Run: th auth login {p} --api-key YOUR_KEY");
                    return Ok(());
                }
                let mut registry = smooth_operator::providers::ProviderRegistry::load_from_file(path)?;
                if registry.get_provider(&p).is_none() {
                    println!("Provider {p} not configured. Run: th auth login {p} --api-key YOUR_KEY");
                    return Ok(());
                }
                registry.set_default_provider(&p);
                registry.save_to_file(path)?;
                println!("Default provider set to: {}", p.green().bold());
            } else if path.exists() {
                let registry = smooth_operator::providers::ProviderRegistry::load_from_file(path)?;
                match registry.default_llm_config() {
                    Ok(config) => println!("Default: {} ({})", config.model, config.api_url),
                    Err(_) => println!("No default configured"),
                }
            } else {
                println!("No providers configured. Run: th auth login <provider> --api-key YOUR_KEY");
            }
        }
        AuthCommands::Remove { provider } => {
            let path = providers_path.as_ref().context("cannot determine home directory")?;
            if !path.exists() {
                println!("No providers configured.");
                return Ok(());
            }
            let mut registry = smooth_operator::providers::ProviderRegistry::load_from_file(path)?;
            registry.remove_provider(&provider);
            registry.save_to_file(path)?;
            println!("Removed: {}", provider.red().bold());
        }
    }
    Ok(())
}

async fn cmd_operators() -> Result<()> {
    match reqwest::get("http://localhost:4400/api/workers").await {
        Ok(resp) => {
            let json: serde_json::Value = resp.json().await?;
            let workers = json["data"].as_array();
            if workers.is_none_or(Vec::is_empty) {
                println!("No active Smooth Operators.");
            } else {
                for w in workers.unwrap_or(&vec![]) {
                    println!("{}", serde_json::to_string_pretty(w)?);
                }
            }
        }
        Err(_) => println!("Cannot reach leader. Run: th up"),
    }
    Ok(())
}

async fn cmd_inbox() -> Result<()> {
    match reqwest::get("http://localhost:4400/api/messages/inbox").await {
        Ok(resp) => {
            let json: serde_json::Value = resp.json().await?;
            let msgs = json["data"].as_array();
            if msgs.is_none_or(Vec::is_empty) {
                println!("No messages.");
            } else {
                for m in msgs.unwrap_or(&vec![]) {
                    println!("{}", serde_json::to_string_pretty(m)?);
                }
            }
        }
        Err(_) => println!("Cannot reach leader. Run: th up"),
    }
    Ok(())
}

async fn cmd_run(bead_id: &str) -> Result<()> {
    println!("Running bead {bead_id}...");
    println!("Operator creation coming in next phase.");
    Ok(())
}

async fn cmd_approve(bead_id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    match client.post(format!("http://localhost:4400/api/reviews/{bead_id}/approve")).send().await {
        Ok(_) => println!("Approved: {bead_id}"),
        Err(e) => println!("Error: {e}"),
    }
    Ok(())
}

async fn cmd_steer(bead_id: &str, action: &str, message: Option<&str>) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("http://localhost:4400/api/steering/{bead_id}/{action}");
    let body = message.map_or(serde_json::json!({}), |m| serde_json::json!({"message": m}));
    match client.post(&url).json(&body).send().await {
        Ok(resp) => {
            let json: serde_json::Value = resp.json().await?;
            println!("{}: {}", action, json["data"].as_str().unwrap_or("ok"));
        }
        Err(e) => println!("Error: {e}"),
    }
    Ok(())
}

fn cmd_audit(cmd: AuditCommands) -> Result<()> {
    let dir = smooth_bigsmooth::audit::get_audit_dir();
    match cmd {
        AuditCommands::Path => println!("{}", dir.display()),
        AuditCommands::List => {
            if !dir.exists() {
                println!("No audit logs yet.");
                return Ok(());
            }
            for entry in std::fs::read_dir(&dir)? {
                let e = entry?;
                if e.path().extension().is_some_and(|x| x == "log") {
                    let name = e.file_name().to_string_lossy().replace(".log", "");
                    println!("  {name:<24} {:.1} KB", e.metadata()?.len() as f64 / 1024.0);
                }
            }
        }
        AuditCommands::Tail { actor, lines } => {
            let actor = actor.unwrap_or_else(|| "leader".into());
            let path = dir.join(format!("{actor}.log"));
            if !path.exists() {
                println!("No audit log for {actor}");
                return Ok(());
            }
            let content = std::fs::read_to_string(&path)?;
            let all: Vec<&str> = content.lines().collect();
            for line in &all[all.len().saturating_sub(lines)..] {
                println!("{line}");
            }
        }
    }
    Ok(())
}

fn cmd_worktree(cmd: WorktreeCommands) -> Result<()> {
    use std::process::Command;
    match cmd {
        WorktreeCommands::Create { branch } => {
            Command::new("git")
                .args(["worktree", "add", &format!("../smooth-{branch}"), "-b", &branch, "main"])
                .status()?;
        }
        WorktreeCommands::List => {
            Command::new("git").args(["worktree", "list"]).status()?;
        }
        WorktreeCommands::Remove { branch } => {
            Command::new("git").args(["worktree", "remove", &format!("../smooth-{branch}")]).status()?;
        }
        WorktreeCommands::Merge { branch } => {
            for args in [vec!["checkout", "main"], vec!["pull", "--rebase"], vec!["merge", &branch, "--no-ff"]] {
                if !Command::new("git").args(&args).status()?.success() {
                    anyhow::bail!("git {} failed", args.join(" "));
                }
            }
            println!("Merged {branch} to main");
        }
    }
    Ok(())
}

async fn cmd_access(cmd: AccessCommands) -> Result<()> {
    let client = reqwest::Client::new();
    let base = "http://localhost:4400/api/access";

    match cmd {
        AccessCommands::Pending => {
            let resp = client.get(format!("{base}/pending")).send().await?;
            let body: serde_json::Value = resp.json().await?;
            if let Some(requests) = body.as_array() {
                if requests.is_empty() {
                    println!("No pending access requests.");
                } else {
                    println!("{:<12} {:<20} {:<30} Reason", "Bead", "Operator", "Resource");
                    println!("{}", "-".repeat(80));
                    for req in requests {
                        println!(
                            "{:<12} {:<20} {:<30} {}",
                            req["bead_id"].as_str().unwrap_or("-"),
                            req["operator_id"].as_str().unwrap_or("-"),
                            req["resource"].as_str().unwrap_or("-"),
                            req["reason"].as_str().unwrap_or("-"),
                        );
                    }
                }
            }
        }
        AccessCommands::Approve { bead, domain } => {
            let resp = client
                .post(format!("{base}/approve"))
                .json(&serde_json::json!({"bead_id": bead, "domain": domain}))
                .send()
                .await?;
            if resp.status().is_success() {
                println!("Approved {domain} for {bead}");
            } else {
                println!("Failed: {}", resp.text().await?);
            }
        }
        AccessCommands::Deny { bead, domain } => {
            let resp = client
                .post(format!("{base}/deny"))
                .json(&serde_json::json!({"bead_id": bead, "domain": domain}))
                .send()
                .await?;
            if resp.status().is_success() {
                println!("Denied {domain} for {bead}");
            } else {
                println!("Failed: {}", resp.text().await?);
            }
        }
        AccessCommands::Policy { operator_id } => {
            let resp = client.get(format!("http://localhost:4400/api/operators/{operator_id}/policy")).send().await?;
            if resp.status().is_success() {
                let body: serde_json::Value = resp.json().await?;
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                println!("Operator {operator_id} not found or no policy set");
            }
        }
    }
    Ok(())
}

/// Read all bytes from stdin if data is available (piped input).
fn read_stdin() -> Option<String> {
    use std::io::Read;
    // Only read if stdin is not a terminal (i.e. data is piped in)
    if atty::is(atty::Stream::Stdin) {
        return None;
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok()?;
    if buf.trim().is_empty() {
        None
    } else {
        Some(buf)
    }
}

/// Launch smooth-code — THE Smooth experience.
/// Auto-starts Big Smooth if not running.
async fn cmd_code(headless: bool, message: Option<String>, file: Option<String>, model: Option<String>, budget: Option<f64>, json: bool) -> Result<()> {
    if headless {
        let working_dir = std::env::current_dir()?;
        let msg = message
            .or_else(|| file.and_then(|f| std::fs::read_to_string(f).ok()))
            .or_else(read_stdin)
            .ok_or_else(|| anyhow::anyhow!("--message, --file, or stdin required for headless mode"))?;
        return smooth_code::headless::run_headless(working_dir, msg, model, budget, json).await;
    }

    // Quick startup checks (non-blocking warnings)
    {
        let providers_path = dirs_next::home_dir().map(|h| h.join(".smooth/providers.json"));
        if let Some(ref path) = providers_path {
            if !path.exists() {
                println!("  {} {}", "\u{26a0}".yellow().bold(), "No providers configured. Run: th auth login".yellow());
            }
        }
        let dolt_on_path = std::process::Command::new("smooth-dolt")
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !dolt_on_path {
            let in_target = std::env::current_dir()
                .ok()
                .map(|d| d.join("target/release/smooth-dolt").exists())
                .unwrap_or(false);
            if !in_target {
                println!(
                    "  {} {}",
                    "\u{26a0}".yellow().bold(),
                    "smooth-dolt binary not found. Pearl sync may not work. Run: scripts/build-smooth-dolt.sh".yellow()
                );
            }
        }
    }

    // Check if Big Smooth is running
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build()?;
    let health = client.get("http://localhost:4400/health").send().await;

    if health.is_err() || !health.as_ref().is_ok_and(|r| r.status().is_success()) {
        println!("Starting Smooth...");

        // Start Big Smooth in background
        let db_path = smooth_bigsmooth::db::default_db_path();
        let db = smooth_bigsmooth::db::Database::open(&db_path)?;
        let pearl_store = match find_dolt_dir() {
            Ok(dolt_dir) => smooth_pearls::PearlStore::open(&dolt_dir)?,
            Err(_) => {
                let cwd = std::env::current_dir()?;
                let dolt_dir = cwd.join(".smooth").join("dolt");
                smooth_pearls::PearlStore::init(&dolt_dir)?
            }
        };
        let state = smooth_bigsmooth::server::AppState::new(db, pearl_store);
        let addr: SocketAddr = "127.0.0.1:4400".parse()?;

        tokio::spawn(async move {
            if let Err(e) = smooth_bigsmooth::server::start(state, addr).await {
                tracing::error!(error = %e, "Big Smooth failed");
            }
        });

        // Wait for health check (up to 5s)
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if client.get("http://localhost:4400/health").send().await.is_ok_and(|r| r.status().is_success()) {
                break;
            }
        }
    }

    // Launch smooth-code TUI
    let working_dir = std::env::current_dir()?;
    smooth_code::app::run(working_dir).await
}

fn cmd_hooks(cmd: HooksCommands) -> Result<()> {
    match cmd {
        HooksCommands::Install => {
            let hooks_dir = hooks::install(None)?;
            hooks::print_install_result(&hooks_dir);
        }
        HooksCommands::Run { hook, args } => {
            hooks::run_hook(&hook, &args)?;
        }
        HooksCommands::Status => {
            let status = hooks::check(None);
            hooks::print_doctor_status(&status);
        }
    }
    Ok(())
}

/// System health check and auto-fix.
async fn cmd_doctor() -> Result<()> {
    println!("{}", "Smooth Doctor".bold().cyan());
    println!("{}", "checking system health...\n".dimmed());

    let mut issues = 0;

    // 1. Check Big Smooth API
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build()?;
    match client.get("http://localhost:4400/health").send().await {
        Ok(r) if r.status().is_success() => {
            println!("  {} Big Smooth API: {}", "✓".green().bold(), "healthy".green());
        }
        Ok(r) => {
            println!("  {} Big Smooth API: {}", "✗".red().bold(), format!("unhealthy (status {})", r.status()).red());
            issues += 1;
        }
        Err(_) => {
            println!("  {} Big Smooth API: {}", "✗".red().bold(), "not running (start with: th up)".red());
            issues += 1;
        }
    }

    // 2. Check database
    let db_path = smooth_bigsmooth::db::default_db_path();
    if db_path.exists() {
        match smooth_bigsmooth::db::Database::open(&db_path) {
            Ok(_) => println!("  {} Database: {}", "✓".green().bold(), format!("OK ({})", db_path.display()).green()),
            Err(e) => {
                println!("  {} Database: {}", "✗".red().bold(), format!("error ({e})").red());
                issues += 1;
            }
        }
    } else {
        println!("  {} Database: {}", "○".dimmed(), "not created yet (will be created on first run)".dimmed());
    }

    // 3. Check providers
    let providers_path = dirs_next::home_dir().map(|h| h.join(".smooth/providers.json"));
    if let Some(ref path) = providers_path {
        if path.exists() {
            println!("  {} Providers: {}", "✓".green().bold(), format!("configured ({})", path.display()).green());
        } else {
            println!("  {} Providers: {}", "✗".red().bold(), "not configured (run: th auth login <provider>)".red());
            issues += 1;
        }
    }

    // 4. Check smooth home dir
    let smooth_home = dirs_next::home_dir().map(|h| h.join(".smooth"));
    if let Some(ref dir) = smooth_home {
        if dir.exists() {
            println!("  {} Smooth home: {}", "✓".green().bold(), format!("{}", dir.display()).green());
        } else {
            println!("  {} Smooth home: {}", "○".dimmed(), format!("will be created at {}", dir.display()).dimmed());
        }
    }

    // 5. Check pearl store (Dolt)
    let pearl_store = find_dolt_dir().and_then(|d| smooth_pearls::PearlStore::open(&d));
    match pearl_store {
        Ok(store) => {
            let stats = store.stats();
            match stats {
                Ok(s) => {
                    println!(
                        "  {} Pearls: {} open, {} in progress, {} closed",
                        "✓".green().bold(),
                        s.open,
                        s.in_progress,
                        s.closed
                    );
                }
                Err(_) => {
                    println!("  {} Pearls: {}", "○".dimmed(), "run: th pearls init".dimmed());
                }
            }
        }
        Err(_) => println!("  {} Issues: {}", "○".dimmed(), "will initialize on first use".dimmed()),
    }

    // 6. Check ~/.smooth is a git repo (for backup)
    if let Some(ref dir) = smooth_home {
        if dir.exists() {
            let git_dir = dir.join(".git");
            if git_dir.exists() {
                // Check if remote is configured
                let remote = std::process::Command::new("git")
                    .args(["remote", "-v"])
                    .current_dir(dir)
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string());
                if remote.as_ref().is_some_and(|r| !r.trim().is_empty()) {
                    println!("  {} Backup: {}", "✓".green().bold(), "~/.smooth is git repo with remote".green());
                } else {
                    println!(
                        "  {} Backup: {}",
                        "○".dimmed(),
                        "~/.smooth is git repo but no remote (run: cd ~/.smooth && git remote add origin <url>)".dimmed()
                    );
                }
            } else {
                println!(
                    "  {} Backup: {}",
                    "○".dimmed(),
                    "~/.smooth is not a git repo (run: cd ~/.smooth && git init)".dimmed()
                );
            }
        }
    }

    // 7. Check for stale SQLite pearls that could be migrated
    let sqlite_path = dirs_next::home_dir().map(|h| h.join(".smooth/smooth.db"));
    if let Some(ref path) = sqlite_path {
        if path.exists() && find_dolt_dir().is_ok() {
            println!(
                "  {} SQLite: {}",
                "○".dimmed(),
                "legacy smooth.db found — run: th pearls migrate-from-sqlite (to migrate to Dolt)".dimmed()
            );
        }
    }

    // 8. Sandboxes (built-in via microsandbox crate)
    println!("  {} Sandboxes: {}", "✓".green().bold(), "built-in (microsandbox)".green());

    // 9. Git hooks
    let hooks_status = hooks::check(None);
    if !hooks::print_doctor_status(&hooks_status) {
        issues += 1;
        // Auto-fix: install hooks
        println!("    {} installing hooks...", "→".cyan());
        match hooks::install(None) {
            Ok(hooks_dir) => {
                println!("    {} fixed: hooks installed at {}", "✓".green().bold(), hooks_dir.display());
                issues -= 1;
            }
            Err(e) => {
                println!("    {} could not auto-install hooks: {e}", "✗".red().bold());
            }
        }
    }

    println!();
    if issues == 0 {
        println!("{}", "All checks passed. Smooth is ready.".green().bold());
    } else {
        println!("{}", format!("{issues} issue(s) found. Fix them and run: th doctor").yellow().bold());
    }

    Ok(())
}

// ── Jira ──────────────────────────────────────────────────────────

async fn cmd_jira(cmd: JiraCommands) -> Result<()> {
    match cmd {
        JiraCommands::Status => cmd_jira_status().await,
        JiraCommands::Sync => cmd_jira_sync().await,
    }
}

async fn cmd_jira_status() -> Result<()> {
    let Some(config) = smooth_diver::jira::JiraConfig::from_env() else {
        println!("{} Jira not configured", "✗".red().bold());
        println!("  Set these env vars (in .envrc or .envrc.local):");
        println!("    JIRA_URL=https://yourcompany.atlassian.net");
        println!("    JIRA_PROJECT=PROJ");
        println!("    JIRA_EMAIL=you@company.com");
        println!("    JIRA_API_TOKEN=<your-api-token>");
        return Ok(());
    };

    println!("{}", "Jira Integration Status".bold().cyan());
    println!("  URL:     {}", config.url);
    println!("  Project: {}", config.project);
    println!("  Email:   {}", config.email);
    println!("  Token:   {}...", &config.api_token[..8.min(config.api_token.len())]);

    let client = smooth_diver::jira::JiraClient::new(config.clone());
    if client.check_connection().await {
        println!("  Status:  {}", "connected".green().bold());
    } else {
        println!("  Status:  {}", "cannot connect (check credentials)".red().bold());
        return Ok(());
    }

    // Count open Jira tickets by paginating the /search/jql endpoint
    // (the new API doesn't return a `total` — we must count issues).
    let http = reqwest::Client::new();
    let mut jira_count = 0u64;
    let mut next_page: Option<String> = None;
    loop {
        let mut url = format!(
            "{}/rest/api/3/search/jql?jql=project%3D{}+AND+status+!%3D+Done&maxResults=100",
            config.url, config.project
        );
        if let Some(ref token) = next_page {
            url.push_str(&format!("&nextPageToken={token}"));
        }
        match http.get(&url).basic_auth(&config.email, Some(&config.api_token)).send().await {
            Ok(resp) if resp.status().is_success() => {
                let body: serde_json::Value = resp.json().await.unwrap_or_default();
                jira_count += body["issues"].as_array().map_or(0, |a| a.len() as u64);
                if body["isLast"].as_bool().unwrap_or(true) {
                    break;
                }
                next_page = body["nextPageToken"].as_str().map(String::from);
            }
            _ => break,
        }
    }
    println!("  Open:    {} ticket(s) in {}", jira_count, config.project);

    // Count local pearls
    if let Ok(store) = open_pearl_store() {
        if let Ok(stats) = store.stats() {
            println!("  Pearls:  {} open, {} in progress, {} closed", stats.open, stats.in_progress, stats.closed);
        }
    }

    Ok(())
}

async fn cmd_jira_sync() -> Result<()> {
    let Some(config) = smooth_diver::jira::JiraConfig::from_env() else {
        anyhow::bail!("Jira not configured. Set JIRA_URL, JIRA_PROJECT, JIRA_EMAIL, JIRA_API_TOKEN env vars.");
    };

    let client = smooth_diver::jira::JiraClient::new(config.clone());
    if !client.check_connection().await {
        anyhow::bail!("Cannot connect to Jira. Check your credentials.");
    }

    let store = open_pearl_store()?;
    println!("{}", "Syncing pearls ↔ Jira...".bold().cyan());

    // --- Pull: Jira → Pearls (create local pearls for Jira tickets) ---
    let http = reqwest::Client::new();
    let mut jira_issues: Vec<serde_json::Value> = Vec::new();
    let mut next_page: Option<String> = None;
    loop {
        let mut url = format!(
            "{}/rest/api/3/search/jql?jql=project%3D{}+AND+status+!%3D+Done+ORDER+BY+key+DESC&maxResults=100&fields=key,summary,status,description",
            config.url, config.project
        );
        if let Some(ref token) = next_page {
            url.push_str(&format!("&nextPageToken={token}"));
        }
        let resp = http.get(&url).basic_auth(&config.email, Some(&config.api_token)).send().await?;
        let body: serde_json::Value = resp.json().await?;
        if let Some(issues) = body["issues"].as_array() {
            jira_issues.extend(issues.iter().cloned());
        }
        if body["isLast"].as_bool().unwrap_or(true) {
            break;
        }
        next_page = body["nextPageToken"].as_str().map(String::from);
    }

    // Get all open pearls
    let open_pearls = store.list(&smooth_pearls::PearlQuery::new())?;

    // Find Jira tickets not yet tracked as pearls (by title prefix match)
    let mut pulled = 0u32;
    for issue in &jira_issues {
        let key = issue["key"].as_str().unwrap_or("");
        let summary = issue["fields"]["summary"].as_str().unwrap_or("");

        // Check if any pearl already has this Jira key in its title
        let already_tracked = open_pearls.iter().any(|p| p.title.contains(key));
        if already_tracked {
            continue;
        }

        // Create a pearl for this Jira ticket
        let title = format!("{key}: {summary}");
        let desc = issue["fields"]["description"]
            .as_object()
            .and_then(|d| d["content"].as_array())
            .and_then(|a| a.first())
            .and_then(|p| p["content"].as_array())
            .and_then(|a| a.first())
            .and_then(|t| t["text"].as_str())
            .unwrap_or("")
            .to_string();

        let new = smooth_pearls::NewPearl {
            title,
            description: desc,
            pearl_type: smooth_pearls::PearlType::Task,
            priority: smooth_pearls::Priority::Medium,
            assigned_to: None,
            parent_id: None,
            labels: vec!["jira".to_string()],
        };
        match store.create(&new) {
            Ok(pearl) => {
                println!("  {} {} → {}", "↓".cyan(), key, pearl.id);
                pulled += 1;
            }
            Err(e) => {
                eprintln!("  {} {} failed: {e}", "✗".red(), key);
            }
        }
    }

    // --- Push: Pearls → Jira (create Jira tickets for pearls without SMOODEV prefix) ---
    let mut pushed = 0u32;
    for pearl in &open_pearls {
        // Skip if already has a Jira key in title
        if pearl.title.starts_with("SMOODEV-") {
            continue;
        }

        match client.create_ticket(&pearl.title, &pearl.description).await {
            Ok(ticket) => {
                // Update pearl title with Jira key
                let new_title = format!("{}: {}", ticket.key, pearl.title);
                let update = smooth_pearls::PearlUpdate {
                    title: Some(new_title),
                    ..Default::default()
                };
                let _ = store.update(&pearl.id, &update);
                println!("  {} {} → {}", "↑".green(), pearl.id, ticket.key);
                pushed += 1;
            }
            Err(e) => {
                eprintln!("  {} {} failed: {e}", "✗".red(), pearl.id);
            }
        }
    }

    // --- Close: Transition Jira tickets to Done for closed pearls ---
    let closed_pearls = store.list(&smooth_pearls::PearlQuery::new().with_status(smooth_pearls::PearlStatus::Closed))?;
    let mut transitioned = 0u32;
    for pearl in &closed_pearls {
        // Extract SMOODEV-XXX from title
        let jira_key = pearl.title.split(':').next().filter(|k| k.starts_with("SMOODEV-")).map(str::trim);

        let Some(key) = jira_key else { continue };

        // Check if Jira ticket is still open
        let is_open = jira_issues.iter().any(|i| i["key"].as_str() == Some(key));
        if !is_open {
            continue;
        }

        match client.transition_ticket(key, "done").await {
            Ok(()) => {
                println!("  {} {} → Done", "✓".green(), key);
                transitioned += 1;
            }
            Err(e) => {
                eprintln!("  {} {} transition failed: {e}", "✗".red(), key);
            }
        }
    }

    println!();
    println!(
        "{} pulled, {} pushed, {} transitioned",
        pulled.to_string().cyan(),
        pushed.to_string().green(),
        transitioned.to_string().green()
    );

    Ok(())
}

// ── Pearls ─────────────────────────────────────────────────────────

fn open_pearl_store() -> Result<smooth_pearls::PearlStore> {
    let dolt_dir = find_dolt_dir()?;
    smooth_pearls::PearlStore::open(&dolt_dir)
}

fn format_pearl_line(issue: &smooth_pearls::Pearl) -> String {
    let labels_str = if issue.labels.is_empty() {
        String::new()
    } else {
        format!(" [{}]", issue.labels.join(", "))
    };
    format!(
        "{} {} {} P{} {}{}",
        issue.status,
        issue.id.dimmed(),
        "\u{25CF}".dimmed(),
        issue.priority.as_u8(),
        issue.title,
        labels_str.dimmed()
    )
}

async fn cmd_pearls(cmd: PearlCommands) -> Result<()> {
    let store = open_pearl_store()?;

    match cmd {
        PearlCommands::Create {
            title,
            description,
            r#type,
            priority,
            label,
        } => {
            let pearl_type = smooth_pearls::PearlType::from_str_loose(&r#type).unwrap_or(smooth_pearls::PearlType::Task);
            let prio = smooth_pearls::Priority::from_u8(priority).unwrap_or(smooth_pearls::Priority::Medium);

            let new = smooth_pearls::NewPearl {
                title,
                description: description.unwrap_or_default(),
                pearl_type,
                priority: prio,
                assigned_to: None,
                parent_id: None,
                labels: label,
            };
            let issue = store.create(&new)?;
            println!("{} Created {}", "✓".green().bold(), issue.id.green().bold());
            println!("  {}", format_pearl_line(&issue));
        }

        PearlCommands::List { status } => {
            let query = if let Some(ref s) = status {
                let st = smooth_pearls::PearlStatus::from_str_loose(s).ok_or_else(|| anyhow::anyhow!("unknown status: {s}"))?;
                smooth_pearls::PearlQuery::new().with_status(st)
            } else {
                smooth_pearls::PearlQuery::new()
            };
            let issues = store.list(&query)?;
            if issues.is_empty() {
                println!("No pearls found.");
            } else {
                for issue in &issues {
                    println!("{}", format_pearl_line(issue));
                }
                println!("\n{} issue(s)", issues.len());
            }
        }

        PearlCommands::Show { id } => {
            let issue = store.get(&id)?.ok_or_else(|| anyhow::anyhow!("issue not found: {id}"))?;
            println!("{} {}", issue.status, issue.title.bold());
            println!("  {} {} | {} | {}", "ID:".dimmed(), issue.id, issue.priority, issue.pearl_type);
            if let Some(ref assignee) = issue.assigned_to {
                println!("  {} {assignee}", "Assigned:".dimmed());
            }
            if !issue.labels.is_empty() {
                println!("  {} {}", "Labels:".dimmed(), issue.labels.join(", "));
            }
            if !issue.description.is_empty() {
                println!("\n{}", issue.description);
            }

            // Show dependencies
            let deps = store.get_deps(&issue.id)?;
            if !deps.is_empty() {
                println!("\n{}", "Dependencies:".dimmed());
                for dep in &deps {
                    if let Ok(Some(blocker)) = store.get(&dep.depends_on) {
                        println!("  {} {}: {}", dep.dep_type.as_str(), blocker.id, blocker.title);
                    }
                }
            }

            // Show comments
            let comments = store.get_comments(&issue.id)?;
            if !comments.is_empty() {
                println!("\n{}", "Comments:".dimmed());
                for c in &comments {
                    println!("  {} {}", c.created_at.format("%Y-%m-%d %H:%M").to_string().dimmed(), c.content);
                }
            }

            // Show history
            let history = store.get_history(&issue.id)?;
            if !history.is_empty() {
                println!("\n{}", "History:".dimmed());
                for h in &history {
                    println!(
                        "  {} {} {} → {}",
                        h.changed_at.format("%Y-%m-%d %H:%M").to_string().dimmed(),
                        h.field,
                        h.old_value.as_deref().unwrap_or("-").dimmed(),
                        h.new_value.as_deref().unwrap_or("-")
                    );
                }
            }
        }

        PearlCommands::Update {
            id,
            status,
            title,
            description,
            priority,
            assign,
        } => {
            let updates = smooth_pearls::PearlUpdate {
                title,
                description,
                status: status.as_deref().and_then(smooth_pearls::PearlStatus::from_str_loose),
                priority: priority.and_then(smooth_pearls::Priority::from_u8),
                assigned_to: assign.map(|a| if a.is_empty() { None } else { Some(a) }),
                ..Default::default()
            };
            let updated = store.update(&id, &updates)?;
            println!("{} Updated {}", "✓".green().bold(), updated.id);
            println!("  {}", format_pearl_line(&updated));
        }

        PearlCommands::Close { ids } => {
            let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            let count = store.close(&id_refs)?;
            println!("{} Closed {count} issue(s)", "✓".green().bold());
        }

        PearlCommands::Reopen { id } => {
            let issue = store.reopen(&id)?;
            println!("{} Reopened {}", "✓".green().bold(), issue.id);
            println!("  {}", format_pearl_line(&issue));
        }

        PearlCommands::Dep { cmd } => match cmd {
            DepCommands::Add { issue, depends_on } => {
                store.add_dep(&issue, &depends_on)?;
                println!("{} {issue} now depends on {depends_on}", "✓".green().bold());
            }
            DepCommands::Remove { issue, depends_on } => {
                store.remove_dep(&issue, &depends_on)?;
                println!("{} Removed dependency {issue} → {depends_on}", "✓".green().bold());
            }
        },

        PearlCommands::Comment { id, content } => {
            let comment = store.add_comment(&id, &content)?;
            println!("{} Comment added ({})", "✓".green().bold(), comment.id.dimmed());
        }

        PearlCommands::Search { query } => {
            let results = store.search(&query)?;
            if results.is_empty() {
                println!("No issues matching \"{query}\".");
            } else {
                for issue in &results {
                    println!("{}", format_pearl_line(issue));
                }
                println!("\n{} result(s)", results.len());
            }
        }

        PearlCommands::Stats => {
            let stats = store.stats()?;
            println!("{}", "Issue Statistics".bold().cyan());
            println!("  {} Open:        {}", "\u{25CB}".dimmed(), stats.open);
            println!("  {} In Progress: {}", "\u{25D0}".yellow(), stats.in_progress);
            println!("  {} Closed:      {}", "\u{2713}".green(), stats.closed);
            println!("  {} Deferred:    {}", "\u{2744}".blue(), stats.deferred);
            println!("  ─────────────────");
            println!("  Total:         {}", stats.total);
        }

        PearlCommands::Ready => {
            let issues = store.ready()?;
            if issues.is_empty() {
                println!("No ready issues.");
            } else {
                println!("{}", "Ready Issues (open, no blockers):".bold().cyan());
                for issue in &issues {
                    println!("  {}", format_pearl_line(issue));
                }
                println!("\n{} issue(s)", issues.len());
            }
        }

        PearlCommands::Blocked => {
            let issues = store.blocked()?;
            if issues.is_empty() {
                println!("No blocked issues.");
            } else {
                println!("{}", "Blocked Issues:".bold().red());
                for issue in &issues {
                    let blockers = store.get_blockers(&issue.id)?;
                    let blocker_ids: Vec<&str> = blockers.iter().map(|b| b.id.as_str()).collect();
                    println!("  {} (blocked by: {})", format_pearl_line(issue), blocker_ids.join(", ").dimmed());
                }
                println!("\n{} issue(s)", issues.len());
            }
        }

        PearlCommands::Label { id, cmd } => match cmd {
            LabelCommands::Add { label } => {
                store.add_label(&id, &label)?;
                println!("{} Added label \"{label}\" to {id}", "✓".green().bold());
            }
            LabelCommands::Remove { label } => {
                store.remove_label(&id, &label)?;
                println!("{} Removed label \"{label}\" from {id}", "✓".green().bold());
            }
        },

        PearlCommands::MigrateFromBeads => {
            cmd_migrate_from_beads(&store)?;
        }

        PearlCommands::MigrateFromSqlite => {
            cmd_migrate_from_sqlite(&store)?;
        }

        PearlCommands::Projects => {
            let registry = smooth_pearls::Registry::load()?;
            let projects = registry.list();
            if projects.is_empty() {
                println!("No pearl projects registered yet.");
                println!("Run {} in a project to register it.", "th pearls init".bold());
            } else {
                println!("{}", "Registered Pearl Projects".bold().cyan());
                println!();
                for entry in &projects {
                    let exists = entry.path.join(".smooth").join("dolt").exists();
                    let status = if exists {
                        "✓".green().bold().to_string()
                    } else {
                        "✗".red().bold().to_string()
                    };
                    println!("  {} {} {}", status, entry.name.bold(), entry.path.display().to_string().dimmed());
                    println!("    Last accessed: {}", entry.last_accessed.format("%Y-%m-%d %H:%M").to_string().dimmed());
                }
                println!("\n{} project(s)", projects.len());
            }
        }

        // ── Dolt commands ────────────────────────────────────────────
        PearlCommands::Init => {
            let cwd = std::env::current_dir()?;
            let dolt_dir = cwd.join(".smooth").join("dolt");
            if dolt_dir.exists() {
                println!("Pearl database already initialized at {}", dolt_dir.display());
            } else {
                smooth_pearls::PearlStore::init(&dolt_dir)?;
                println!("{} Pearl database initialized at {}", "✓".green().bold(), dolt_dir.display());
                println!("  Tables: pearls, pearl_dependencies, pearl_labels, pearl_comments, pearl_history, sessions, memories");
                println!("  Run: th pearls remote add origin <git-remote-url>");
                println!("  Then: th pearls push");
            }

            // Install git hooks if not already present
            let hooks_status = hooks::check(None);
            if !hooks_status.is_ok() {
                println!();
                match hooks::install(None) {
                    Ok(hooks_dir) => hooks::print_install_result(&hooks_dir),
                    Err(e) => eprintln!("  Could not install git hooks: {e}"),
                }
            }
        }

        PearlCommands::Log { n } => {
            let dolt_dir = find_dolt_dir()?;
            let dolt = smooth_pearls::SmoothDolt::new(&dolt_dir)?;
            let entries = dolt.log(n)?;
            if entries.is_empty() {
                println!("No commits yet.");
            } else {
                for (line, _, _, _) in &entries {
                    println!("{line}");
                }
            }
        }

        PearlCommands::Push => {
            let dolt_dir = find_dolt_dir()?;
            let dolt = smooth_pearls::SmoothDolt::new(&dolt_dir)?;
            let output = dolt.push()?;
            println!("{output}");
        }

        PearlCommands::Pull => {
            let dolt_dir = find_dolt_dir()?;
            let dolt = smooth_pearls::SmoothDolt::new(&dolt_dir)?;
            let output = dolt.pull()?;
            println!("{output}");
        }

        PearlCommands::Remote { cmd } => {
            let dolt_dir = find_dolt_dir()?;
            let dolt = smooth_pearls::SmoothDolt::new(&dolt_dir)?;
            match cmd {
                RemoteCommands::Add { name, url } => {
                    let output = dolt.remote_add(&name, &url)?;
                    println!("{output}");
                }
                RemoteCommands::List => {
                    let output = dolt.remote_list()?;
                    if output.is_empty() {
                        println!("No remotes configured. Run: th pearls remote add origin <url>");
                    } else {
                        println!("{output}");
                    }
                }
                RemoteCommands::Remove { name } => {
                    // Remove via SQL: CALL DOLT_REMOTE('remove', ?)
                    let output = dolt.exec(&format!("CALL DOLT_REMOTE('remove', '{name}')"))?;
                    println!("removed remote {name}");
                    let _ = output;
                }
            }
        }

        PearlCommands::Gc => {
            let dolt_dir = find_dolt_dir()?;
            let dolt = smooth_pearls::SmoothDolt::new(&dolt_dir)?;
            let output = dolt.gc()?;
            println!("{output}");
        }
    }

    Ok(())
}

/// Find the .smooth/dolt/ directory by walking up from cwd.
fn find_dolt_dir() -> Result<std::path::PathBuf> {
    let cwd = std::env::current_dir()?;
    smooth_pearls::dolt::find_repo_dolt_dir(&cwd).ok_or_else(|| anyhow::anyhow!("no .smooth/dolt/ found. Run: th pearls init"))
}

fn cmd_migrate_from_beads(store: &smooth_pearls::PearlStore) -> Result<()> {
    println!("{}", "Migrating from Beads...".bold().cyan());

    let mut total = 0;
    let mut migrated = 0;
    let mut skipped = 0;

    // Try to get beads issues as JSON
    for status in &["open", "in_progress", "closed", "deferred"] {
        let output = std::process::Command::new("bd")
            .args(["list", &format!("--status={status}"), "--json"])
            .output();

        let output = match output {
            Ok(o) if o.status.success() => o,
            Ok(_) => continue,
            Err(e) => {
                if status == &"open" {
                    // First try — bd might not be installed
                    println!("  {} Cannot run bd: {e}", "✗".red().bold());
                    println!("  beads not installed (migration requires bd CLI)");
                    return Ok(());
                }
                continue;
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let beads: Vec<serde_json::Value> = match serde_json::from_str(&stdout) {
            Ok(v) => v,
            Err(_) => continue,
        };

        for bead in &beads {
            total += 1;
            let bead_title = bead["title"].as_str().unwrap_or("Untitled");
            let bead_desc = bead["description"].as_str().unwrap_or("");
            let bead_type = bead["type"].as_str().unwrap_or("task");
            let bead_priority = bead["priority"].as_u64().unwrap_or(2);
            let bead_labels: Vec<String> = bead["labels"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();

            let pearl_type = smooth_pearls::PearlType::from_str_loose(bead_type).unwrap_or(smooth_pearls::PearlType::Task);
            #[allow(clippy::cast_possible_truncation)]
            let priority = smooth_pearls::Priority::from_u8(bead_priority as u8).unwrap_or(smooth_pearls::Priority::Medium);

            let new = smooth_pearls::NewPearl {
                title: bead_title.to_string(),
                description: bead_desc.to_string(),
                pearl_type,
                priority,
                assigned_to: bead["assigned_to"].as_str().map(String::from),
                parent_id: None,
                labels: bead_labels,
            };

            match store.create(&new) {
                Ok(issue) => {
                    // If the bead was closed/in_progress/deferred, update status
                    let target_status = smooth_pearls::PearlStatus::from_str_loose(status);
                    if let Some(st) = target_status {
                        if st != smooth_pearls::PearlStatus::Open {
                            let _ = store.update(
                                &issue.id,
                                &smooth_pearls::PearlUpdate {
                                    status: Some(st),
                                    ..Default::default()
                                },
                            );
                        }
                    }
                    migrated += 1;
                    println!("  {} {} ← {}", "✓".green(), issue.id, bead_title.dimmed());
                }
                Err(e) => {
                    skipped += 1;
                    println!("  {} {}: {e}", "✗".red(), bead_title);
                }
            }
        }
    }

    println!();
    println!("{}", "Migration Summary".bold());
    println!("  Total beads found: {total}");
    println!("  Migrated:          {}", format!("{migrated}").green());
    if skipped > 0 {
        println!("  Skipped/errors:    {}", format!("{skipped}").red());
    }

    Ok(())
}

fn cmd_migrate_from_sqlite(store: &smooth_pearls::PearlStore) -> Result<()> {
    println!("{}", "Migrating pearls from SQLite to Dolt...".bold().cyan());

    let db_path = smooth_bigsmooth::db::default_db_path();
    if !db_path.exists() {
        println!("  {} No SQLite database found at {}", "○".dimmed(), db_path.display());
        return Ok(());
    }

    let conn = rusqlite::Connection::open(&db_path)?;

    // Check if the pearls table exists in SQLite
    let has_pearls: bool = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pearls'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0)
        > 0;
    if !has_pearls {
        println!("  {} No pearls table in SQLite database", "○".dimmed());
        return Ok(());
    }

    let mut stmt = conn.prepare("SELECT id, title, description, status, priority, pearl_type, assigned_to, parent_id FROM pearls")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, u8>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;

    let mut total = 0;
    let mut migrated = 0;
    let mut skipped = 0;

    for row in rows {
        let (old_id, title, description, status_str, priority_val, type_str, assigned_to, parent_id) = row?;
        total += 1;

        // Check if already exists in Dolt
        if store.get(&old_id)?.is_some() {
            skipped += 1;
            println!("  {} {} already exists in Dolt", "○".dimmed(), old_id.dimmed());
            continue;
        }

        let pearl_type = smooth_pearls::PearlType::from_str_loose(&type_str).unwrap_or(smooth_pearls::PearlType::Task);
        let priority = smooth_pearls::Priority::from_u8(priority_val).unwrap_or(smooth_pearls::Priority::Medium);

        // Load labels from SQLite
        let labels: Vec<String> = if let Ok(mut label_stmt) = conn.prepare("SELECT label FROM labels WHERE pearl_id = ?1") {
            label_stmt
                .query_map(rusqlite::params![&old_id], |r| r.get(0))
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let new = smooth_pearls::NewPearl {
            title,
            description,
            pearl_type,
            priority,
            assigned_to,
            parent_id,
            labels,
        };

        match store.create(&new) {
            Ok(pearl) => {
                // Update status if not open
                let target_status = smooth_pearls::PearlStatus::from_str_loose(&status_str);
                if let Some(st) = target_status {
                    if st != smooth_pearls::PearlStatus::Open {
                        let _ = store.update(
                            &pearl.id,
                            &smooth_pearls::PearlUpdate {
                                status: Some(st),
                                ..Default::default()
                            },
                        );
                    }
                }
                migrated += 1;
                println!("  {} {} ← {} ({})", "✓".green(), pearl.id, old_id.dimmed(), new.title.dimmed());
            }
            Err(e) => {
                skipped += 1;
                println!("  {} {}: {e}", "✗".red(), old_id);
            }
        }
    }

    // Migrate dependencies
    let mut dep_count = 0;
    if let Ok(mut stmt) = conn.prepare("SELECT pearl_id, depends_on FROM dependencies") {
        let deps: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        for (pearl_id, depends_on) in &deps {
            let _ = store.add_dep(pearl_id, depends_on);
            dep_count += 1;
        }
    }

    // Migrate comments
    let mut comment_count = 0;
    if let Ok(mut stmt) = conn.prepare("SELECT pearl_id, content FROM comments ORDER BY created_at ASC") {
        let comments: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        for (pearl_id, content) in &comments {
            let _ = store.add_comment(pearl_id, content);
            comment_count += 1;
        }
    }

    println!();
    println!("{}", "Migration Summary".bold());
    println!("  Total SQLite pearls: {total}");
    println!("  Migrated:            {}", format!("{migrated}").green());
    if skipped > 0 {
        println!("  Skipped/existing:    {}", format!("{skipped}").dimmed());
    }
    if dep_count > 0 {
        println!("  Dependencies:        {dep_count}");
    }
    if comment_count > 0 {
        println!("  Comments:            {comment_count}");
    }

    Ok(())
}

fn cmd_tailscale(cmd: TailscaleCommands) -> Result<()> {
    match cmd {
        TailscaleCommands::Status => {
            let s = smooth_bigsmooth::tailscale::get_status();
            println!("Tailscale: {}", if s.connected { "connected" } else { "disconnected" });
            if let Some(h) = &s.hostname {
                println!("  Hostname: {h}");
            }
            if let Some(ip) = &s.ip {
                println!("  IP: {ip}");
            }
            if let Some(t) = &s.tailnet {
                println!("  Tailnet: {t}");
            }
        }
    }
    Ok(())
}

async fn cmd_routing(cmd: RoutingCommands) -> Result<()> {
    let providers_path = dirs_next::home_dir()
        .map(|h| h.join(".smooth/providers.json"))
        .context("cannot determine home directory")?;

    match cmd {
        RoutingCommands::Show => {
            if !providers_path.exists() {
                println!("  {} No providers configured. Run: th auth login", "✗".red().bold());
                return Ok(());
            }
            let registry = smooth_operator::providers::ProviderRegistry::load_from_file(&providers_path)?;

            println!("\n  {}\n", "Model Routing".cyan().bold());

            use smooth_operator::providers::Activity;
            let activities = [
                (Activity::Thinking, "Thinking", "deep reasoning, chain-of-thought"),
                (Activity::Coding, "Coding", "code generation, edits, refactoring"),
                (Activity::Planning, "Planning", "task decomposition, architecture"),
                (Activity::Reviewing, "Reviewing", "code review, adversarial checks"),
                (Activity::Judge, "Judge", "evaluation, scoring, pass/fail"),
                (Activity::Summarize, "Summarize", "summaries, compression"),
            ];

            for (activity, label, desc) in &activities {
                match registry.llm_config_for(*activity) {
                    Ok(config) => {
                        println!("  {} {:<12} {} {}", "✓".green().bold(), label.bold(), config.model.cyan(), desc.dimmed());
                    }
                    Err(_) => {
                        println!("  {} {:<12} {}", "✗".red().bold(), label, "not configured".red());
                    }
                }
            }
            println!();
        }

        RoutingCommands::Preset { name } => {
            let all_presets = smooth_operator::providers::Preset::ALL;

            let preset_name = if let Some(n) = name {
                n
            } else {
                println!("\n  {}\n", "Routing Presets".cyan().bold());
                for (name, title, desc) in all_presets {
                    println!("  {} {}", name.bold(), format!("— {title}").dimmed());
                    println!("    {}", desc.dimmed());
                    println!();
                }

                let names: Vec<&str> = all_presets.iter().map(|(_, title, _)| *title).collect();
                let selection = Select::with_theme(&ColorfulTheme::default())
                    .with_prompt("Select a preset")
                    .items(&names)
                    .default(0)
                    .interact()?;
                all_presets[selection].0.to_string()
            };

            let preset = match smooth_operator::providers::Preset::from_name(&preset_name) {
                Some(p) => p,
                None => {
                    let names: Vec<&str> = all_presets.iter().map(|(n, _, _)| *n).collect();
                    println!("Unknown preset: {preset_name}");
                    println!("Available: {}", names.join(", "));
                    return Ok(());
                }
            };

            let required_provider = preset.provider_id();

            // Try to get key from existing config
            let api_key = if providers_path.exists() {
                let registry = smooth_operator::providers::ProviderRegistry::load_from_file(&providers_path)?;
                registry.get_provider(required_provider).map(|p| p.api_key.clone())
            } else {
                None
            };

            let api_key = match api_key {
                Some(k) => k,
                None => {
                    println!("  {} requires {} provider. Enter API key:", "⚠".yellow(), required_provider.bold());
                    Password::with_theme(&ColorfulTheme::default()).with_prompt("API key").interact()?
                }
            };

            let registry = smooth_operator::providers::ProviderRegistry::from_preset(preset, &api_key);
            registry.save_to_file(&providers_path)?;

            println!("\n  {} Preset {} applied\n", "✓".green().bold(), preset_name.green().bold());

            // Recurse into Show to display the new routing
            return Box::pin(cmd_routing(RoutingCommands::Show)).await;
        }

        RoutingCommands::Set { activity, model } => {
            if !providers_path.exists() {
                println!("  {} No providers configured. Run: th auth login", "✗".red().bold());
                return Ok(());
            }

            let mut registry = smooth_operator::providers::ProviderRegistry::load_from_file(&providers_path)?;

            // Parse model as "provider/model" or just "model" (uses first provider)
            let (provider_id, model_name) = if let Some(slash_pos) = model.find('/') {
                let p = &model[..slash_pos];
                let m = &model[slash_pos + 1..];
                (p.to_string(), m.to_string())
            } else {
                let providers = registry.list_providers();
                if providers.is_empty() {
                    println!("  {} No providers configured", "✗".red().bold());
                    return Ok(());
                }
                (providers[0].to_string(), model.clone())
            };

            let slot = smooth_operator::providers::ModelSlot::new(&provider_id, &model_name);

            match activity.as_str() {
                "thinking" => registry.routing.thinking = slot,
                "coding" => registry.routing.coding = slot,
                "planning" => registry.routing.planning = slot,
                "reviewing" => registry.routing.reviewing = slot,
                "judge" => registry.routing.judge = slot,
                "summarize" => registry.routing.summarize = slot,
                other => {
                    println!("Unknown activity: {other}");
                    println!("Available: thinking, coding, planning, reviewing, judge, summarize");
                    return Ok(());
                }
            }

            registry.save_to_file(&providers_path)?;
            println!("  {} {} → {}", "✓".green().bold(), activity.bold(), model.cyan());
        }
    }

    Ok(())
}

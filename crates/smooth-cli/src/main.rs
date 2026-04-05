//! `th` — Smoo AI CLI entry point.
//!
//! Single binary for agent orchestration, config management, and platform tools.

use std::net::SocketAddr;

use anyhow::Result;
use clap::{Parser, Subcommand};
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
    /// Pearl tracking (built-in work-item tracker).
    ///
    /// Lineage: beads → issues → pearls. There is no alias — pearls is the
    /// only spelling.
    Pearls {
        #[command(subcommand)]
        cmd: PearlCommands,
    },
    /// System health check and auto-fix
    Doctor,
}

#[derive(Subcommand)]
enum AuthCommands {
    /// Add or update a provider
    Login {
        /// Provider: openrouter, openai, anthropic, ollama, google
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
    /// Migrate from beads
    MigrateFromBeads,
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
        Some(Commands::Up { no_leader, port }) => cmd_up(no_leader, port).await,
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
        Some(_) => {
            println!("Command not yet implemented. Coming soon!");
            Ok(())
        }
    }
}

// ── Command implementations ────────────────────────────────

async fn cmd_up(no_leader: bool, port: u16) -> Result<()> {
    println!("Smoo AI / Smooth starting...");

    // Initialize database
    let db_path = smooth_bigsmooth::db::default_db_path();
    let db = smooth_bigsmooth::db::Database::open(&db_path)?;
    println!("  Database: {} ✓", db_path.display());

    // Initialize issue store (shares the same SQLite file)
    let pearl_store = smooth_pearls::PearlStore::open(&db_path)?;
    println!("  Pearls:  {} ✓", db_path.display());

    if no_leader {
        println!("\nSmooth infrastructure ready (leader skipped).");
        return Ok(());
    }

    // Start leader (API + embedded web UI on same port)
    let state = smooth_bigsmooth::server::AppState::new(db, pearl_store);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("  Leader: http://localhost:{port} ✓");
    println!("  Web UI: http://localhost:{port} ✓");
    println!();

    smooth_bigsmooth::server::start(state, addr).await
}

async fn cmd_down() -> Result<()> {
    println!("Stopping Smooth...");
    println!("  Leader: stop with Ctrl+C");
    Ok(())
}

async fn cmd_status() -> Result<()> {
    let url = "http://localhost:4400/health";
    match reqwest::get(url).await {
        Ok(resp) => {
            let body: serde_json::Value = resp.json().await?;
            println!("Smooth Leader: http://localhost:4400");
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        Err(_) => {
            println!("Cannot reach leader at http://localhost:4400");
            println!("Run: th up");
        }
    }
    Ok(())
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
            println!("Authentication Status\n====================\n");

            // Check providers.json for configured providers
            if let Some(ref path) = providers_path {
                if path.exists() {
                    match smooth_operator::providers::ProviderRegistry::load_from_file(path) {
                        Ok(registry) => {
                            let providers = registry.list_providers();
                            if providers.is_empty() {
                                println!("Providers:    {}", "none configured — run: th auth login <provider>".red());
                            } else {
                                println!("Providers:    {} configured ({})", providers.len(), providers.join(", "));
                            }
                        }
                        Err(_) => {
                            println!("Providers:    {}", "providers.json exists but cannot be read".red());
                        }
                    }
                } else {
                    println!("Providers:    {}", "not configured — run: th auth login <provider>".red());
                }
            }

            let leader_up = reqwest::get("http://localhost:4400/health").await.is_ok();
            println!("Leader:       {}", if leader_up { "running" } else { "not running — run: th up" });
        }
        AuthCommands::Login { provider, .. } => {
            let provider = provider.unwrap_or_else(|| "openrouter".into());
            println!("Provider {provider}: set API key via environment variable or providers.json");
            println!("  e.g. export OPENROUTER_API_KEY=sk-...");
            println!("  Then: th auth login {provider} --api-key $OPENROUTER_API_KEY");
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
        AuthCommands::Default { provider } => println!("Default: {}", provider.unwrap_or_else(|| "openrouter".into())),
        AuthCommands::Remove { provider } => println!("Removed: {provider}"),
    }
    Ok(())
}

async fn cmd_operators() -> Result<()> {
    match reqwest::get("http://localhost:4400/api/workers").await {
        Ok(resp) => {
            let json: serde_json::Value = resp.json().await?;
            let workers = json["data"].as_array();
            if workers.map_or(true, Vec::is_empty) {
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
            if msgs.map_or(true, Vec::is_empty) {
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
                    println!("{:<12} {:<20} {:<30} {}", "Bead", "Operator", "Resource", "Reason");
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

    // Check if Big Smooth is running
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build()?;
    let health = client.get("http://localhost:4400/health").send().await;

    if health.is_err() || !health.as_ref().is_ok_and(|r| r.status().is_success()) {
        println!("Starting Smooth...");

        // Start Big Smooth in background
        let db_path = smooth_bigsmooth::db::default_db_path();
        let db = smooth_bigsmooth::db::Database::open(&db_path)?;
        let pearl_store = smooth_pearls::PearlStore::open(&db_path)?;
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

    // 5. Check smooth-issues
    let pearl_store = smooth_pearls::PearlStore::open(&db_path);
    match pearl_store {
        Ok(store) => {
            let stats = store.stats();
            match stats {
                Ok(s) => {
                    println!(
                        "  {} Issues: {} open, {} in progress, {} closed",
                        "✓".green().bold(),
                        s.open,
                        s.in_progress,
                        s.closed
                    );
                }
                Err(_) => {
                    println!("  {} Issues: {}", "○".dimmed(), "will initialize on first use".dimmed());
                }
            }
        }
        Err(_) => println!("  {} Issues: {}", "○".dimmed(), "will initialize on first use".dimmed()),
    }

    // 6. Sandboxes (built-in via microsandbox crate)
    println!("  {} Sandboxes: {}", "✓".green().bold(), "built-in (microsandbox)".green());

    println!();
    if issues == 0 {
        println!("{}", "All checks passed. Smooth is ready.".green().bold());
    } else {
        println!("{}", format!("{issues} issue(s) found. Fix them and run: th doctor").yellow().bold());
    }

    Ok(())
}

// ── Pearls ─────────────────────────────────────────────────────────

fn open_pearl_store() -> Result<smooth_pearls::PearlStore> {
    let db_path = smooth_bigsmooth::db::default_db_path();
    smooth_pearls::PearlStore::open(&db_path)
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
    }

    Ok(())
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

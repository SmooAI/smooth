//! `th` — Smoo AI CLI entry point.
//!
//! Single binary for agent orchestration, config management, and platform tools.

use std::net::SocketAddr;
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Smooth — AI agent orchestration platform.
/// Run with no arguments to launch the interactive TUI.
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
    /// Launch full terminal UI
    Tui {
        /// Leader server URL
        #[arg(long)]
        server: Option<String>,
    },
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
    Code,
    /// System health check and auto-fix
    Doctor,
}

#[derive(Subcommand)]
enum AuthCommands {
    /// Add or update a provider
    Login {
        /// Provider: opencode-zen, anthropic, openai, openrouter, groq, google
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

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("smooth=info".parse()?))
        .init();

    let cli = Cli::parse();

    match cli.command {
        // No subcommand = launch smooth-code (THE Smooth experience)
        None | Some(Commands::Code) => cmd_code().await,
        Some(Commands::Doctor) => cmd_doctor().await,
        Some(Commands::Up { no_leader, port }) => cmd_up(no_leader, port).await,
        Some(Commands::Down) => cmd_down().await,
        Some(Commands::Status) => cmd_status().await,
        Some(Commands::Tui { server }) => {
            let url = server.unwrap_or_else(|| "http://localhost:4400".into());
            smooth_tui::app::run(&url).await
        }
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

    // Check beads directory
    let beads_dir = dirs_next::home_dir().unwrap_or_default().join(".smooth").join(".beads");
    std::fs::create_dir_all(&beads_dir)?;
    println!("  Beads: {} ✓", beads_dir.display());

    if no_leader {
        println!("\nSmooth infrastructure ready (leader skipped).");
        return Ok(());
    }

    // Start leader (API + embedded web UI on same port)
    let state = smooth_bigsmooth::server::AppState {
        db,
        start_time: Instant::now(),
    };

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
    match cmd {
        AuthCommands::Status => {
            println!("Authentication Status\n====================\n");
            let has_zen = smooth_bigsmooth::chat::is_authenticated();
            println!(
                "OpenCode Zen: {}",
                if has_zen {
                    "authenticated"
                } else {
                    "not authenticated — run: th auth login"
                }
            );
            let leader_up = reqwest::get("http://localhost:4400/health").await.is_ok();
            println!("Leader:       {}", if leader_up { "running" } else { "not running — run: th up" });
        }
        AuthCommands::Login { provider, .. } => {
            let provider = provider.unwrap_or_else(|| "opencode-zen".into());
            if provider == "opencode-zen" {
                println!("Run: opencode providers login -p opencode");
                let _ = std::process::Command::new("opencode").args(["providers", "login", "-p", "opencode"]).status();
            } else {
                println!("Provider {provider}: set API key via environment variable");
            }
        }
        AuthCommands::Providers => {
            if smooth_bigsmooth::chat::is_authenticated() {
                println!("opencode-zen: authenticated (default)");
            } else {
                println!("No providers configured. Run: th auth login");
            }
        }
        AuthCommands::Default { provider } => println!("Default: {}", provider.unwrap_or_else(|| "opencode-zen".into())),
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

/// Launch smooth-code — THE Smooth experience.
/// Auto-starts Big Smooth if not running.
async fn cmd_code() -> Result<()> {
    // Check if Big Smooth is running
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build()?;
    let health = client.get("http://localhost:4400/health").send().await;

    if health.is_err() || !health.as_ref().is_ok_and(|r| r.status().is_success()) {
        println!("Starting Smooth...");

        // Start Big Smooth in background
        let db_path = smooth_bigsmooth::db::default_db_path();
        let db = smooth_bigsmooth::db::Database::open(&db_path)?;
        let state = smooth_bigsmooth::server::AppState {
            db,
            start_time: std::time::Instant::now(),
        };
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
    println!("Smooth Doctor — checking system health...\n");

    let mut issues = 0;

    // 1. Check Big Smooth API
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build()?;
    match client.get("http://localhost:4400/health").send().await {
        Ok(r) if r.status().is_success() => println!("  ✓ Big Smooth API: healthy"),
        Ok(r) => {
            println!("  ✗ Big Smooth API: unhealthy (status {})", r.status());
            issues += 1;
        }
        Err(_) => {
            println!("  ✗ Big Smooth API: not running (start with: th up)");
            issues += 1;
        }
    }

    // 2. Check database
    let db_path = smooth_bigsmooth::db::default_db_path();
    if db_path.exists() {
        match smooth_bigsmooth::db::Database::open(&db_path) {
            Ok(_) => println!("  ✓ Database: OK ({})", db_path.display()),
            Err(e) => {
                println!("  ✗ Database: error ({e})");
                issues += 1;
            }
        }
    } else {
        println!("  ○ Database: not created yet (will be created on first run)");
    }

    // 3. Check providers
    let providers_path = dirs_next::home_dir().map(|h| h.join(".smooth/providers.json"));
    if let Some(ref path) = providers_path {
        if path.exists() {
            println!("  ✓ Providers: configured ({})", path.display());
        } else {
            println!("  ✗ Providers: not configured (run: th auth login <provider>)");
            issues += 1;
        }
    }

    // 4. Check smooth home dir
    let smooth_home = dirs_next::home_dir().map(|h| h.join(".smooth"));
    if let Some(ref dir) = smooth_home {
        if dir.exists() {
            println!("  ✓ Smooth home: {}", dir.display());
        } else {
            println!("  ○ Smooth home: will be created at {}", dir.display());
        }
    }

    // 5. Check Beads
    match std::process::Command::new("bd").arg("stats").output() {
        Ok(output) if output.status.success() => println!("  ✓ Beads: available"),
        _ => {
            println!("  ✗ Beads: not found (install from https://github.com/SmooAI/beads)");
            issues += 1;
        }
    }

    // 6. Check Microsandbox
    match std::process::Command::new("msb").arg("version").output() {
        Ok(output) if output.status.success() => println!("  ✓ Microsandbox: available"),
        _ => println!("  ○ Microsandbox: not found (optional, needed for operators)"),
    }

    println!();
    if issues == 0 {
        println!("All checks passed. Smooth is ready.");
    } else {
        println!("{issues} issue(s) found. Fix them and run: th doctor");
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

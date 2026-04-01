//! `th` — Smoo AI CLI entry point.
//!
//! Single binary for agent orchestration, config management, and platform tools.

use std::net::SocketAddr;
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Smoo AI CLI — agent orchestration, config management, and platform tools.
#[derive(Parser)]
#[command(name = "th", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
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
    Run {
        bead_id: String,
    },
    /// Pause a running Smooth Operator
    Pause {
        bead_id: String,
    },
    /// Resume a paused Smooth Operator
    Resume {
        bead_id: String,
    },
    /// Send guidance to a running Smooth Operator
    Steer {
        bead_id: String,
        message: String,
    },
    /// Cancel a running Smooth Operator
    Cancel {
        bead_id: String,
    },
    /// Approve a pending review
    Approve {
        bead_id: String,
    },
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
    Default {
        provider: Option<String>,
    },
    /// Remove a provider
    Remove {
        provider: String,
    },
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

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("smooth=info".parse()?)).init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Up { no_leader, port } => cmd_up(no_leader, port).await,
        Commands::Down => cmd_down().await,
        Commands::Status => cmd_status().await,
        Commands::Db { cmd } => cmd_db(cmd),
        _ => {
            tracing::warn!("Command not yet implemented in Rust version");
            Ok(())
        }
    }
}

// ── Command implementations ────────────────────────────────

async fn cmd_up(no_leader: bool, port: u16) -> Result<()> {
    println!("Smoo AI / Smooth starting...");

    // Initialize database
    let db_path = smooth_leader::db::default_db_path();
    let db = smooth_leader::db::Database::open(&db_path)?;
    println!("  Database: {} ✓", db_path.display());

    // Check beads directory
    let beads_dir = dirs_next::home_dir().unwrap_or_default().join(".smooth").join(".beads");
    std::fs::create_dir_all(&beads_dir)?;
    println!("  Beads: {} ✓", beads_dir.display());

    if no_leader {
        println!("\nSmooth infrastructure ready (leader skipped).");
        return Ok(());
    }

    // Start leader
    let state = smooth_leader::server::AppState {
        db,
        start_time: Instant::now(),
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("  Leader: http://localhost:{port} ✓");
    println!();

    smooth_leader::server::start(state, addr).await
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
    let db_path = smooth_leader::db::default_db_path();
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

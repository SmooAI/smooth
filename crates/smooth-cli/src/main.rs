//! `th` — Smoo AI CLI entry point.
//!
//! Single binary for agent orchestration, config management, and platform tools.

mod active_org;
mod admin;
mod auth;
mod config;
mod daemon_launcher;
mod gradient;
mod hooks;
mod mcp_config;
mod service;
mod smooai;
mod tailscale;

use smooai::{cmd_login, cmd_logout, cmd_orgs, cmd_whoami};


use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dialoguer::{theme::ColorfulTheme, Input, Password, Select};
use owo_colors::OwoColorize;

/// Smooth — AI agent orchestration platform.
/// Run with no arguments to launch the interactive coding assistant.
#[derive(Parser)]
#[command(name = "th", version = env!("TH_VERSION"), about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Resume a saved session. With no value, picks the most recently
    /// updated one. With a value, matches by id prefix or title
    /// substring. Pair with `--list` to inspect saved sessions first.
    /// Only takes effect when no subcommand is given (top-level `th`
    /// launches the TUI). Same as `th code --resume`. Pearl
    /// th-resume-top-level (2026-05-12).
    #[arg(long, value_name = "QUERY", num_args = 0..=1, default_missing_value = "")]
    resume: Option<String>,

    /// List saved sessions and exit. Only takes effect when no
    /// subcommand is given. Same as `th code --list`.
    #[arg(long)]
    list: bool,

    /// Pin the lead role for this session (fixer / oracle / mapper /
    /// scout / heckler). Same as `th code --agent <name>`.
    #[arg(long, value_name = "NAME")]
    agent: Option<String>,

    /// Auth profile to use for this command (overrides the active profile
    /// and `SMOOAI_PROFILE`). Profiles bundle a user + M2M session under
    /// `~/.config/smooth/auth/profiles/<name>/`. See `th auth profile`.
    #[arg(long, global = true, value_name = "NAME")]
    profile: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run / control the always-on Smooth daemon (EPIC th-c89c2a).
    ///
    /// A thin **passthrough** to the standalone `smooth-daemon` binary —
    /// resolved locally or downloaded on first use, so `th` itself doesn't
    /// statically link the operator runtime. `th daemon --help` shows the full
    /// daemon CLI: `run` (foreground) / `operator` / `status` / `audit` /
    /// `schedule`.
    Daemon {
        /// Args forwarded verbatim to the `smooth-daemon` binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// LLM provider credential management (Anthropic, Smoo AI Gateway,
    /// OpenRouter, OpenAI, …). Edits `~/.smooth/providers.json`.
    ///
    /// Was `th auth` before 2026-05 — that name now belongs to Smoo
    /// AI identity (`th auth login` for user/email-password, `th auth
    /// login --m2m` for service accounts). LLM-provider config moved
    /// here so the two concerns don't share a verb.
    Model {
        #[command(subcommand)]
        cmd: ModelCommands,
    },
    /// Smoo AI identity — log in to the Smoo AI platform as a user
    /// (email + password) or service account (M2M client_credentials).
    /// Used by `th admin *`, `th api *`, and (soon) llm.smoo.ai's
    /// user-attributed LLM session exchange.
    Auth {
        #[command(subcommand)]
        cmd: auth::AuthCommands,
    },
    /// Smoo AI superadmin operations against the /admin/* endpoints
    /// on api.smoo.ai. Requires a `th auth login` user session whose
    /// account has the requireSuperAdmin role (403 otherwise).
    Admin {
        #[command(subcommand)]
        cmd: admin::AdminCommands,
    },
    /// Smoo AI platform API — REST-style verbs backed by `api.smoo.ai`.
    /// Login + orgs + agents + keys + members + knowledge + jobs +
    /// products + profile + testing live under here. Config has its
    /// own top-level subcommand (`th config`) for the daily surface,
    /// `th admin config` for the platform-admin surface.
    Api {
        #[command(subcommand)]
        cmd: ApiCommands,
    },
    /// Smoo AI `@smooai/config` — the daily-developer config surface.
    /// `get` / `set` / `list` for single values; `feature-flag` to
    /// evaluate a flag; `push` / `pull` / `diff` to sync the
    /// `.smooai-config/schema.json` document with the org's remote
    /// schema; `init` to scaffold a fresh local schema package;
    /// `delete` to remove a value record.
    ///
    /// Prefers the user JWT at `~/.smooth/auth/smooai-user.json`;
    /// pass `--m2m` to use the M2M session instead.
    ///
    /// Platform-admin verbs (schemas CRUD, environments CRUD,
    /// bulk-set) live under `th admin config`. Pearl `th-9c0c34`.
    Config {
        #[command(subcommand)]
        cmd: config::Cmd,
    },
    /// Show messages requiring attention
    Inbox,
    /// Project management
    Project {
        #[command(subcommand)]
        cmd: ProjectCommands,
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
    /// View audit logs
    Audit {
        #[command(subcommand)]
        cmd: AuditCommands,
    },
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
        /// Resume a previous session. Pass a query (matched against
        /// title or id prefix) to pick a specific one, or leave empty
        /// to resume the most recently updated session. Pair with
        /// `--list` to see what's available.
        #[arg(long, value_name = "QUERY", num_args = 0..=1, default_missing_value = "")]
        resume: Option<String>,
        /// List saved sessions (id, title, updated) and exit without
        /// launching the TUI.
        #[arg(long)]
        list: bool,
        /// Lead role to run under: `fixer` (default, full tools),
        /// `mapper` (read-only, decomposes), `oracle` (read-only, reasons),
        /// or `heckler` (read-only, critiques). Unknown names error
        /// out with the list above.
        #[arg(long)]
        agent: Option<String>,
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
    /// Agent registry — register this session as a named agent so other
    /// agents (any harness: claude-code, opencode, pi, a shell loop) can
    /// message it. Backed by the pearl Dolt store, synced via refs/dolt/data.
    Agent {
        #[command(subcommand)]
        cmd: AgentCommands,
    },
    /// Agent messaging — send and receive messages between agents.
    /// `th msg send --to <name|all>` to send, `th msg inbox` to read,
    /// `th msg watch` to continuously poll. Harness-agnostic: any process
    /// that can run `th` can participate.
    Msg {
        #[command(subcommand)]
        cmd: MsgCommands,
    },
    /// Configure per-activity model routing (which model for thinking, coding, etc.)
    Routing {
        #[command(subcommand)]
        cmd: RoutingCommands,
    },
    /// MCP server management (Playwright, GitHub, etc.)
    Mcp {
        #[command(subcommand)]
        cmd: McpCommands,
    },
    /// File-based CLI-wrapper plugins (~/.smooth/plugins/*/plugin.toml)
    Plugin {
        #[command(subcommand)]
        cmd: PluginCommands,
    },
    /// Run Smooth as a background service (launchd / systemd / Task Scheduler)
    Service {
        #[command(subcommand)]
        cmd: ServiceCommands,
    },
    /// Print workflow-rules + current-state context block (for Claude
    /// Code SessionStart / PreCompact hooks; the `th` equivalent of
    /// `bd prime`)
    Prime,
    /// System health check and auto-fix
    Doctor {
        /// Initialize ~/.smooth/ as a git repo (backup/sync config).
        /// Writes a .gitignore that excludes secrets and high-churn data,
        /// seeds an initial commit. Skips any config that's already
        /// tracked. Optionally takes a remote URL to set up push/pull.
        #[arg(long)]
        init_home_repo: bool,
        /// Optional git remote URL to add when --init-home-repo is set
        /// (e.g. git@github.com:you/smooth-config.git)
        #[arg(long)]
        remote: Option<String>,
    },
    /// List skills available in the current workspace. Reads
    /// `.smooth/skills/`, `~/.smooth/skills/`, `~/.claude/skills/`,
    /// and `~/.opencode/skills/` — first hit wins on name. Pearl
    /// th-e0f812.
    Skills {
        #[command(subcommand)]
        cmd: SkillsCommands,
    },
    /// Inspect the LLM cast — model aliases and the live model groups
    /// the configured provider exposes (e.g. llm.smoo.ai).
    Cast {
        #[command(subcommand)]
        cmd: CastCommands,
    },
}

#[derive(Subcommand)]
enum CastCommands {
    /// List live model groups exposed by the configured LiteLLM
    /// provider via `GET /v1/models`. Useful for confirming deploys,
    /// debugging routing, and copying alias names. Pearl th-2b5f63.
    Models {
        /// Provider id to query. Defaults to the provider backing the
        /// `default` routing slot (the one `th routing show` highlights).
        /// Pass an explicit id (e.g. `smooai-gateway`, `openrouter`)
        /// when multiple providers are configured.
        #[arg(long)]
        provider: Option<String>,
        /// Emit JSON `{"data":[{"id":...}]}` instead of the colorized
        /// list. Stable shape for scripts.
        #[arg(long)]
        json: bool,
        /// Case-insensitive substring filter applied to model ids.
        #[arg(long)]
        filter: Option<String>,
    },
}


#[derive(Subcommand)]
enum SkillsCommands {
    /// List all skills discovered from every source.
    List,
    /// Show the body + frontmatter of a specific skill.
    Show {
        /// Skill name.
        name: String,
    },
}




#[derive(Subcommand)]
enum OrgsCommands {
    /// List organizations the logged-in user belongs to.
    List,
    /// Show details of an organization. Defaults to the active org.
    Show {
        /// Org id (UUID). Omit to use the active org from
        /// `~/.smooth/auth/smooai.json`.
        org_id: Option<String>,
    },
    /// Switch the active org persisted in `~/.smooth/auth/smooai.json`.
    /// Subsequent commands default to this org unless `--org` is set.
    ///
    /// Omit the argument on a TTY to pick interactively from the orgs you
    /// belong to. A value is matched as a UUID first, then case-insensitively
    /// against org name / slug (substring) — so `th api orgs switch ats`
    /// works without copying a UUID.
    Switch {
        /// Org id (UUID) or a name/slug substring. Omit to pick from a list.
        org_id: Option<String>,
    },
}

#[derive(Subcommand)]
enum ApiCommands {
    /// Authenticate `th` against the Smoo AI platform API. Exchanges
    /// an OAuth2 client_credentials grant at `https://auth.smoo.ai/token`
    /// for a bearer JWT and stores it at `~/.smooth/auth/smooai.json`.
    ///
    /// Credential resolution order (first present wins):
    ///   1. `--client-id` + `--client-secret` flags
    ///   2. `SMOOAI_CLIENT_ID` + `SMOOAI_CLIENT_SECRET` env vars
    ///   3. Interactive prompt
    ///
    /// Create a client_id / client_secret pair in the smooai web app
    /// (Organization Settings → API Keys) before running this.
    Login {
        #[arg(long)]
        client_id: Option<String>,
        #[arg(long)]
        client_secret: Option<String>,
    },
    /// Forget the current Smoo AI platform session — deletes
    /// `~/.smooth/auth/smooai.json`. Idempotent.
    Logout,
    /// Print the currently-logged-in Smoo AI user + active org.
    Whoami,
    /// Smoo AI organization management.
    Orgs {
        #[command(subcommand)]
        cmd: OrgsCommands,
    },
    /// Smoo AI agents — list / show / create / update / delete + the
    /// regenerate-* and per-agent knowledge endpoints.
    Agents {
        #[command(subcommand)]
        cmd: smooai::agents::Cmd,
    },
    /// Smoo AI M2M auth clients ("API keys") — list / create /
    /// rotate / revoke.
    Keys {
        #[command(subcommand)]
        cmd: smooai::keys::Cmd,
    },
    /// Smoo AI org members + invitations.
    Members {
        #[command(subcommand)]
        cmd: smooai::members::Cmd,
    },
    /// Smoo AI CRM — contacts (list / get / create / update / import).
    /// Authenticates as the logged-in user (`th auth login`), so writes
    /// are attributed to a real person rather than an M2M client.
    Crm {
        #[command(subcommand)]
        cmd: smooai::crm::Cmd,
    },
    /// Smoo AI knowledge documents (text, websites, files).
    Knowledge {
        #[command(subcommand)]
        cmd: smooai::knowledge::Cmd,
    },
    /// Smoo AI async job queue.
    Jobs {
        #[command(subcommand)]
        cmd: smooai::jobs::Cmd,
    },
    /// Smoo AI billing products / plans.
    Products {
        #[command(subcommand)]
        cmd: smooai::products::Cmd,
    },
    /// Smoo AI profile (the currently-logged-in user).
    Profile {
        #[command(subcommand)]
        cmd: smooai::profile::Cmd,
    },
    /// Smoo AI testing platform — deployments, cases, environments,
    /// runs.
    Testing {
        #[command(subcommand)]
        cmd: smooai::testing::Cmd,
    },
    /// Smoo AI Observability — source maps, traces, LLM telemetry.
    /// SMOODEV-1164.
    Observability {
        #[command(subcommand)]
        cmd: smooai::observability::Cmd,
    },
}


#[derive(Subcommand)]
enum ServiceCommands {
    /// Install and enable the user-level service (LaunchAgent / systemd --user / logon task)
    Install {
        /// Print the system-level artifact instead of installing a user-level one
        #[arg(long)]
        system: bool,
        /// Run the always-on `th daemon` (EPIC th-c89c2a) instead of `th up`.
        #[arg(long)]
        daemon: bool,
    },
    /// Disable and remove the user-level service
    Uninstall,
    /// Start the installed service
    Start,
    /// Stop the installed service
    Stop,
    /// Restart the installed service
    Restart,
    /// Show the service manager's view of the service
    Status,
    /// Tail the service log files
    Logs {
        /// Follow new output (like `tail -f`)
        #[arg(short, long)]
        follow: bool,
    },
}

#[derive(Subcommand)]
enum PluginCommands {
    /// Scaffold a new plugin (default: ~/.smooth/plugins/<name>/plugin.toml)
    Init {
        /// Plugin name (becomes the tool name as `plugin.<name>`)
        name: String,
        /// Shell command template; use `{{param}}` placeholders for args
        #[arg(long)]
        command: Option<String>,
        /// Short description shown to the LLM
        #[arg(long)]
        description: Option<String>,
        /// Scaffold into the current project's `.smooth/plugins/` instead of `~/.smooth/plugins/`
        #[arg(long)]
        project: bool,
    },
    /// List installed plugins (global + project-scoped)
    List,
    /// Print the path of a plugin's manifest (or the plugins directory)
    Path {
        name: Option<String>,
        /// Print the project-scoped path instead of the global one
        #[arg(long)]
        project: bool,
    },
    /// Remove a plugin and its directory
    Remove {
        name: String,
        /// Only remove from the project directory
        #[arg(long)]
        project: bool,
    },
}

#[derive(Subcommand)]
enum McpCommands {
    /// Register an MCP server (default: ~/.smooth/mcp.toml)
    Add {
        /// Name used to prefix this server's tools (e.g. "playwright")
        name: String,
        /// Command to spawn (e.g. "npx", "docker", or an absolute path)
        command: String,
        /// Arguments passed to the command
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Per-server env var (KEY=VALUE; supports `${env:VAR}` substitution). Repeat for multiple.
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        /// Register but do not start until enabled
        #[arg(long)]
        disabled: bool,
        /// Write to the current project's `.smooth/mcp.toml` instead of `~/.smooth/mcp.toml`
        #[arg(long)]
        project: bool,
    },
    /// List configured MCP servers (global + project-scoped)
    List,
    /// Remove a server by name
    Remove {
        name: String,
        /// Only look in the project config
        #[arg(long)]
        project: bool,
    },
    /// Spawn a server's command and report whether it starts cleanly
    Test { name: String },
    /// Print the config file path
    Path {
        /// Print the project-scoped path instead of the global one
        #[arg(long)]
        project: bool,
    },
    /// List MCP servers Smooth ships as defaults
    Defaults,
    /// Register a shipped-default MCP server into `~/.smooth/mcp.toml`
    /// (idempotent — never touches an existing entry of the same name).
    Install {
        /// Default name (`budget-aware-mcp`, …). Omit to install every default.
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum RoutingCommands {
    /// Show current routing configuration
    Show,
    /// Ask the gateway what concrete upstream backs each alias.
    ///
    /// Hits `GET /model/info` on each configured provider that supports
    /// it (LiteLLM-backed gateways like llm.smoo.ai). Useful when your
    /// slots point at semantic aliases (`smooth-coding`, …) and you
    /// want to know what's actually running behind them today.
    Resolved,
    /// Apply a preset routing configuration
    Preset {
        /// Preset name: low-cost, codex, anthropic
        name: Option<String>,
    },
    /// Set routing for a specific activity
    Set {
        /// Activity: coding, reasoning, reviewing, judge, summarize, fast, default
        /// (legacy aliases `thinking` and `planning` route into `reasoning`)
        activity: String,
        /// Model in provider/model format (e.g. openrouter/deepseek/deepseek-v3.2)
        model: String,
    },
}

#[derive(Subcommand)]
enum ModelCommands {
    /// Add or update an LLM provider's API key in ~/.smooth/providers.json.
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
    /// Show LLM provider configuration status
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
    Push {
        /// Force-push, overwriting remote history. Useful when the
        /// remote has a stale `Initialize data repository` commit
        /// from an earlier `dolt init` that shares no ancestor with
        /// the local store.
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Pull pearl data from git remote
    Pull,
    /// Manage Dolt remotes for pearl sync
    Remote {
        #[command(subcommand)]
        cmd: RemoteCommands,
    },
    /// Garbage collect the pearl database (compact for git)
    Gc,
    /// Diagnose + (optionally) auto-repair the on-disk dolt state.
    ///
    /// Cold-loads the pearl DB through the CLI (not the running server)
    /// and reports whether the noms manifest reads cleanly. If it
    /// doesn't, `--auto-repair` snapshots the broken dir and re-clones
    /// from the configured `origin` remote.
    Doctor {
        /// Snapshot the broken dir and re-clone from `origin` if a
        /// corrupt manifest is found. Without this flag, `doctor` just
        /// reports — no destructive changes.
        #[arg(long)]
        auto_repair: bool,
        /// Repair even when a `smooth-dolt serve` is attached to this
        /// dir. Stops the server first. Without this flag, doctor
        /// refuses to repair when a server is running (in-memory state
        /// could differ from disk; you'd lose any unsaved work).
        #[arg(long)]
        force: bool,
    },
    /// Migrate from beads
    MigrateFromBeads,
    /// List all registered pearl projects
    Projects,
    /// Record a persistent project memory (an insight to recall later).
    /// Surfaced by `th pearls prime`. Pearl th-202885.
    Remember {
        /// The note to store.
        text: String,
        /// Origin tag (a pearl id, "manual", an agent name, …).
        #[arg(long, default_value = "manual")]
        source: String,
    },
    /// List recent project memories.
    Memories {
        /// How many to show (newest first).
        #[arg(long, default_value = "30")]
        limit: usize,
        /// Only memories from this source tag.
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Forget a single memory by id.
    Forget { id: String },
    /// Print a compact session-priming context: open/in-progress pearls
    /// plus recent memories. Agents load this at session start.
    Prime {
        /// Max memories to include.
        #[arg(long, default_value = "20")]
        memories: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Register this session as a named agent (idempotent). Other agents
    /// can then message it by name. Defaults: name from $SMOOTH_AGENT, else
    /// `user@host`; harness from $SMOOTH_HARNESS.
    Register {
        /// Agent handle. Falls back to $SMOOTH_AGENT, then `user@host`.
        #[arg(long)]
        name: Option<String>,
        /// Harness/tool tag (claude-code, opencode, pi, shell). Falls back
        /// to $SMOOTH_HARNESS, then "unknown".
        #[arg(long)]
        harness: Option<String>,
        /// Don't push to the repo's remote after registering (default
        /// pushes so other clones' `th agent list` sees you).
        #[arg(long)]
        no_push: bool,
    },
    /// List registered agents (most recently seen first).
    List {
        #[arg(long)]
        json: bool,
    },
    /// Mark this (or a named) agent offline.
    Offline {
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum MsgCommands {
    /// Send a message to an agent (or `all` for a broadcast).
    Send {
        /// Recipient agent name, or `all` to broadcast.
        #[arg(long)]
        to: String,
        /// Message body.
        #[arg(long)]
        body: String,
        /// Sender name. Falls back to $SMOOTH_AGENT, then `user@host`.
        #[arg(long)]
        from: Option<String>,
        /// Reply under an existing message's thread (its id or any id in
        /// the thread).
        #[arg(long)]
        re: Option<String>,
        /// Don't push to the repo's remote after sending (default pushes
        /// so other clones/machines on the same repo receive it).
        #[arg(long)]
        no_push: bool,
    },
    /// Show messages addressed to me (and broadcasts).
    Inbox {
        /// Whose inbox (defaults to $SMOOTH_AGENT / `user@host`).
        #[arg(long)]
        agent: Option<String>,
        /// Pull from the repo's remote first, so messages sent from other
        /// clones/machines show up.
        #[arg(long)]
        pull: bool,
        /// Only unread messages.
        #[arg(long)]
        unread: bool,
        /// Mark the listed messages read after showing them.
        #[arg(long)]
        mark_read: bool,
        #[arg(long, default_value = "50")]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Mark a message read.
    Read { id: String },
    /// Reply to a message (threads automatically).
    Reply {
        /// Message id being replied to.
        id: String,
        #[arg(long)]
        body: String,
        #[arg(long)]
        from: Option<String>,
    },
    /// Show a full thread (a root message + its replies).
    Thread { id: String },
    /// Continuously poll for new messages and print them as they arrive.
    /// The "continuously check messages" primitive — run it in the
    /// background of any agent session.
    Watch {
        /// Whose inbox (defaults to $SMOOTH_AGENT / `user@host`).
        #[arg(long)]
        agent: Option<String>,
        /// Seconds between polls.
        #[arg(long, default_value = "5")]
        interval: u64,
        /// Don't pull from the repo's remote each poll (default pulls so
        /// messages from other clones/machines arrive). Use for a purely
        /// local mailbox or when offline.
        #[arg(long)]
        no_pull: bool,
    },
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

/// Validate and canonicalize a `--agent` CLI argument against the
/// built-in cast. Returns the role name the rest of the CLI should
/// use.
///
/// - `None` → defaults to `"fixer"` (the full-tool lead role).
/// - `Some(name)` where `name` is a registered, non-hidden role →
///   returns `name.to_string()`.
/// - Any other input produces an error listing the available
///   visible roles, so a typo at the CLI fails loudly before a
///   runner spins up with the wrong clearance set.
fn resolve_primary_agent(name: Option<&str>) -> Result<String> {
    let cast = smooth_cast::cast::builtin();
    let available: Vec<String> = {
        let mut v: Vec<String> = cast.list_visible().map(|a| a.name.clone()).collect();
        v.sort();
        v
    };
    match name.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok("fixer".into()),
        Some(raw) => match cast.get(raw) {
            Some(role) if !role.hidden => Ok(role.name.clone()),
            _ => anyhow::bail!("unknown --agent '{raw}' — available: {}", available.join(" | ")),
        },
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // SMOODEV-1739: resolve the active auth profile (--profile flag →
    // SMOOAI_PROFILE → active-profile file) and export SMOOAI_USER_AUTH_FILE /
    // SMOOAI_AUTH_FILE so every credential-store call reads the right files.
    // Also migrates legacy ~/.smooth/auth → ~/.config/smooth/auth on first run.
    auth::paths::init(cli.profile.clone());

    // Pearl th-bench-loop iter 23 / user observation 2026-05-10:
    // tracing-to-stderr trampled the ratatui TUI render whenever
    // Big Smooth's server-side spans fired. Route to a log file
    // by default (`~/.smooth/log/th.log`) so the TUI stays clean.
    //
    // Two escape hatches:
    //   - `--headless` / `code --json` and `doctor` use stderr because
    //     they're CLI-only and the user is expecting structured output.
    //   - `SMOOTH_LOG=stderr` forces stderr regardless (useful for
    //     debugging the CLI itself).
    // Daemon subcommands are long-running services: log to stderr so the output
    // is visible in the foreground and captured by the service supervisor
    // (launchd/systemd via `th service`), instead of buried in the TUI's th.log.
    let log_to_stderr = std::env::var("SMOOTH_LOG").as_deref() == Ok("stderr")
        || matches!(
            &cli.command,
            Some(Commands::Code { headless: true, .. }) | Some(Commands::Doctor { .. }) | Some(Commands::Daemon { .. })
        );
    let env_filter = tracing_subscriber::EnvFilter::from_default_env().add_directive("smooth=info".parse()?);
    if log_to_stderr {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        // Best-effort file logger. If we can't open the file, fall
        // through to a no-op subscriber — the TUI is more important
        // than the log.
        let log_dir = dirs_next::home_dir().map(|h| h.join(".smooth").join("log"));
        let writer_pair = log_dir.and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            let log_path = dir.join("th.log");
            std::fs::OpenOptions::new().create(true).append(true).open(&log_path).ok()
        });
        if let Some(file) = writer_pair {
            let mutex_writer = std::sync::Mutex::new(file);
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_writer(move || mutex_writer.lock().expect("th.log writer poisoned").try_clone().expect("clone th.log handle"))
                .with_ansi(false)
                .init();
        } else {
            // No writable home dir — silence tracing so the TUI
            // doesn't get trampled.
            tracing_subscriber::fmt().with_env_filter(env_filter).with_writer(std::io::sink).init();
        }
    }

    match cli.command {
        // No subcommand = decide between explainer and the TUI.
        //
        // Bare `th` (no subcommand AND no resume/list/agent flags)
        // prints a short explainer so first-time users learn what
        // `th` is for instead of being dropped into a TUI cold.
        // Pearl th-91d8af (2026-05-20).
        //
        // `th --resume` / `th --list` / `th --agent X` continue to
        // forward into `cmd_code` so the top-level shortcuts from
        // pearl th-resume-top-level (2026-05-12) still work.
        None => {
            let any_code_flag = cli.resume.is_some() || cli.list || cli.agent.is_some();
            if any_code_flag {
                cmd_code(false, None, None, None, None, false, cli.resume.clone(), cli.list, cli.agent.clone()).await
            } else {
                print_explainer();
                Ok(())
            }
        }
        Some(Commands::Code {
            headless,
            message,
            file,
            model,
            budget,
            json,
            resume,
            list,
            agent,
        }) => cmd_code(headless, message, file, model, budget, json, resume, list, agent).await,
        Some(Commands::Doctor { init_home_repo, remote }) => {
            if init_home_repo {
                cmd_doctor_init_home_repo(remote.as_deref())
            } else {
                cmd_doctor().await
            }
        }
        Some(Commands::Daemon { args }) => daemon_launcher::run(args).await,
        Some(Commands::Db { cmd }) => cmd_db(cmd),
        Some(Commands::Model { cmd }) => cmd_model(cmd).await,
        Some(Commands::Auth { cmd }) => auth::dispatch(cmd).await,
        Some(Commands::Admin { cmd }) => admin::dispatch(cmd).await,
        Some(Commands::Api { cmd }) => match cmd {
            ApiCommands::Login { client_id, client_secret } => cmd_login(client_id, client_secret).await,
            ApiCommands::Logout => cmd_logout().await,
            ApiCommands::Whoami => cmd_whoami().await,
            ApiCommands::Orgs { cmd } => cmd_orgs(cmd).await,
            ApiCommands::Agents { cmd } => smooai::agents::cmd(cmd).await,
            ApiCommands::Keys { cmd } => smooai::keys::cmd(cmd).await,
            ApiCommands::Members { cmd } => smooai::members::cmd(cmd).await,
            ApiCommands::Crm { cmd } => smooai::crm::cmd(cmd).await,
            ApiCommands::Knowledge { cmd } => smooai::knowledge::cmd(cmd).await,
            ApiCommands::Jobs { cmd } => smooai::jobs::cmd(cmd).await,
            ApiCommands::Products { cmd } => smooai::products::cmd(cmd).await,
            ApiCommands::Profile { cmd } => smooai::profile::cmd(cmd).await,
            ApiCommands::Testing { cmd } => smooai::testing::cmd(cmd).await,
            ApiCommands::Observability { cmd } => smooai::observability::cmd(cmd).await,
        },
        Some(Commands::Config { cmd }) => config::cmd(cmd).await,
        Some(Commands::Inbox) => cmd_inbox().await,
        Some(Commands::Hooks { cmd }) => cmd_hooks(cmd),
        Some(Commands::Pearls { cmd }) => cmd_pearls(cmd).await,
        Some(Commands::Agent { cmd }) => cmd_agent(cmd).await,
        Some(Commands::Msg { cmd }) => cmd_msg(cmd).await,
        Some(Commands::Audit { cmd }) => cmd_audit(cmd),
        Some(Commands::Worktree { cmd }) => cmd_worktree(cmd),
        Some(Commands::Tailscale { cmd }) => cmd_tailscale(cmd),
        Some(Commands::Jira { cmd }) => cmd_jira(cmd).await,
        Some(Commands::Routing { cmd }) => cmd_routing(cmd).await,
        Some(Commands::Mcp { cmd }) => cmd_mcp(cmd),
        Some(Commands::Plugin { cmd }) => cmd_plugin(cmd),
        Some(Commands::Service { cmd }) => cmd_service(cmd),
        Some(Commands::Skills { cmd }) => cmd_skills(cmd),
        Some(Commands::Cast { cmd }) => cmd_cast(cmd).await,
        Some(Commands::Prime) => cmd_prime(),
        Some(_) => {
            println!("Command not yet implemented. Coming soon!");
            Ok(())
        }
    }
}

// ── Command implementations ────────────────────────────────










fn cmd_db(cmd: DbCommands) -> Result<()> {
    // Smooth retired SQLite; all durable state (pearls, sessions,
    // memories, config) now lives in the Dolt store at
    // ~/.smooth/dolt/ (home) or <repo>/.smooth/dolt/ (per-project).
    let dolt_dir = dirs_next::home_dir().unwrap_or_default().join(".smooth").join("dolt");
    match cmd {
        DbCommands::Status => {
            if dolt_dir.exists() {
                println!("Dolt store: {}", dolt_dir.display());
                println!("For per-project pearl counts: cd into a project and run `th pearls stats`.");
            } else {
                println!("Dolt store not created yet. Run: th up");
            }
        }
        DbCommands::Path => println!("{}", dolt_dir.display()),
        DbCommands::Backup => {
            println!("Backups go through Dolt's native push/pull. Run: `th pearls push` to a configured remote.");
        }
    }
    Ok(())
}

async fn cmd_model(cmd: ModelCommands) -> Result<()> {
    let providers_path = dirs_next::home_dir().map(|h| h.join(".smooth/providers.json"));

    match cmd {
        ModelCommands::Status => {
            println!();
            println!("  {}", "Auth Status".bold().cyan());
            println!();

            // Check providers.json for configured providers
            if let Some(ref path) = providers_path {
                if path.exists() {
                    match smooth_cast::provider_migration::load_providers_with_migration(path) {
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

            let leader_up = reqwest::get("http://localhost:8787/health").await.is_ok();
            // "Big Smooth" visible width = 10; the original `{:<12} ` formatter
            // added two trailing spaces + one literal separator (= 3 spaces).
            // Reproduce that by hand since the gradient escapes inflate byte
            // length and would confuse `{:<12}`.
            if leader_up {
                println!("  {} Big {}   {}", "\u{2713}".green().bold(), gradient::smooth(), "running".green());
            } else {
                println!(
                    "  {} Big {}   {}",
                    "\u{2717}".red().bold(),
                    gradient::smooth(),
                    "not running \u{2014} run: th up".red()
                );
            }
            println!();
        }
        ModelCommands::Login { provider, api_key } => {
            let path = providers_path.as_ref().context("cannot determine home directory")?;

            // Provider catalog: (id, display name, models, needs_key)
            // First entry is the recommended default — it's surfaced at the
            // top of the picker. Smoo AI Gateway is the hosted LiteLLM-backed
            // gateway run by Smoo AI with billing, moderation, governance,
            // and provider routing on the server side.
            // Display names are `String` so the recommended entry can carry
            // the gradient wordmark for "Smoo AI" alongside the rest of the
            // label.
            let smoo_ai_gateway_name = format!("{} Gateway (recommended)", gradient::smoo_ai());
            let catalog: Vec<(&str, String, Vec<&str>, bool)> = vec![
                (
                    "smooai-gateway",
                    smoo_ai_gateway_name,
                    // Concrete model names — the legacy `smooth-*` slot
                    // aliases were removed at the gateway under
                    // SMOODEV-1793. `smooth_policy::smooth_alias`
                    // holds the canonical mapping; see also the
                    // catalog in smooth-code/src/model_picker.rs.
                    vec![
                        "deepseek-v4-flash",     // coding + default
                        "deepseek-v4-pro",       // reasoning
                        "minimax-m2.7-direct",   // reviewing
                        "gemini-2.5-flash",      // judge + summarize
                        "gemini-2.5-flash-lite", // fast
                    ],
                    true,
                ),
                (
                    "llmgateway",
                    "LLM Gateway".to_string(),
                    vec!["openai/gpt-4o", "anthropic/claude-sonnet-4", "google/gemini-2.5-flash", "deepseek/deepseek-v3"],
                    true,
                ),
                ("kimi-code", "Kimi Code".to_string(), vec!["kimi-for-coding"], true),
                ("kimi", "Kimi".to_string(), vec!["kimi-k2.5", "kimi-k2", "moonshot-v1-auto"], true),
                (
                    "openrouter",
                    "OpenRouter".to_string(),
                    vec![
                        "deepseek/deepseek-v3",
                        "openai/gpt-4o",
                        "anthropic/claude-sonnet-4",
                        "moonshot/kimi-k2.5",
                        "google/gemini-flash-2.0",
                    ],
                    true,
                ),
                ("openai", "OpenAI".to_string(), vec!["gpt-4o", "gpt-4o-mini", "o3-mini", "gpt-5.4-mini"], true),
                (
                    "anthropic",
                    "Anthropic".to_string(),
                    vec!["claude-sonnet-4-20250514", "claude-opus-4-20250514", "claude-haiku-4-5-20251001"],
                    true,
                ),
                ("google", "Google AI".to_string(), vec!["gemini-2.5-flash", "gemini-2.5-pro"], true),
                ("ollama", "Ollama (local)".to_string(), vec!["llama3.3", "qwen3", "deepseek-r1"], false),
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
                let display_names: Vec<&str> = catalog.iter().map(|(_, name, ..)| name.as_str()).collect();
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
                smooth_cast::provider_migration::load_providers_with_migration(path).unwrap_or_default()
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
        ModelCommands::Providers => {
            if let Some(ref path) = providers_path {
                if path.exists() {
                    match smooth_cast::provider_migration::load_providers_with_migration(path) {
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
        ModelCommands::Default { provider } => {
            let path = providers_path.as_ref().context("cannot determine home directory")?;
            if let Some(p) = provider {
                if !path.exists() {
                    println!("No providers configured. Run: th auth login {p} --api-key YOUR_KEY");
                    return Ok(());
                }
                let mut registry = smooth_cast::provider_migration::load_providers_with_migration(path)?;
                if registry.get_provider(&p).is_none() {
                    println!("Provider {p} not configured. Run: th auth login {p} --api-key YOUR_KEY");
                    return Ok(());
                }
                registry.set_default_provider(&p);
                registry.save_to_file(path)?;
                println!("Default provider set to: {}", p.green().bold());
            } else if path.exists() {
                let registry = smooth_cast::provider_migration::load_providers_with_migration(path)?;
                match registry.default_llm_config() {
                    Ok(config) => println!("Default: {} ({})", config.model, config.api_url),
                    Err(_) => println!("No default configured"),
                }
            } else {
                println!("No providers configured. Run: th auth login <provider> --api-key YOUR_KEY");
            }
        }
        ModelCommands::Remove { provider } => {
            let path = providers_path.as_ref().context("cannot determine home directory")?;
            if !path.exists() {
                println!("No providers configured.");
                return Ok(());
            }
            let mut registry = smooth_cast::provider_migration::load_providers_with_migration(path)?;
            registry.remove_provider(&provider);
            registry.save_to_file(path)?;
            println!("Removed: {}", provider.red().bold());
        }
    }
    Ok(())
}


/// `th inbox` — convenience alias for `th msg inbox` against the local
/// pearl-store mailbox (pearl th-70aaef). Was a stub hitting Big Smooth's
/// `/api/messages/inbox` (which always returned `[]`); now it shows the
/// real agent mailbox for this session's default identity.
async fn cmd_inbox() -> Result<()> {
    cmd_msg(MsgCommands::Inbox {
        agent: None,
        pull: false,
        unread: false,
        mark_read: false,
        limit: 50,
        json: false,
    })
    .await
}





fn cmd_audit(cmd: AuditCommands) -> Result<()> {
    let dir = dirs_next::home_dir().unwrap_or_default().join(".smooth").join("audit");
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

/// Print a short, friendly explainer when the user runs bare `th`
/// with no subcommand and no top-level code flags. Pearl th-91d8af
/// — first-time users should see what `th` is for before getting
/// dropped into the TUI cold; explicit entry via `th code` (or any
/// of the top-level `--resume` / `--list` / `--agent` shortcuts)
/// still launches the TUI immediately.
fn print_explainer() {
    let version = env!("TH_VERSION");
    println!("{} {}", "th".bold().bright_cyan(), format!("v{version}").dimmed());
    println!(
        "{}{}",
        gradient::smooth(),
        format!("'s CLI for AI-driven coding, orchestration, and the {} platform.", gradient::smoo_ai()).bold()
    );
    println!();
    println!("{}", "What it does".bold().bright_yellow());
    println!("  • Interactive AI coding TUI                 {}", "th code".bright_cyan());
    println!(
        "  • microVM orchestration via Big {} + cast  {}",
        gradient::smooth(),
        "th up / th down / th status".bright_cyan()
    );
    println!("  • Pearl issue tracker                       {}", "th pearls".bright_cyan());
    println!("  • {} platform CLI                      {}", gradient::smoo_ai(), "th api".bright_cyan());
    println!("  • LLM gateway aliases (smooth-coding, …)    {}", "th cast".bright_cyan());
    println!("  • MCP server roster                         {}", "th mcp".bright_cyan());
    println!();
    println!("{}", "Get started".bold().bright_yellow());
    println!("  {}  {}", "th code".bright_cyan(), "— launch the interactive coding TUI".dimmed());
    println!("  {}  {}", "th pearls ready".bright_cyan(), "— show pearls ready to work on".dimmed());
    println!(
        "  {}  {} {} {}",
        "th up".bright_cyan(),
        "— start the".dimmed(),
        gradient::smooth(),
        "platform (sandboxed)".dimmed()
    );
    println!(
        "  {}  {} {} {}",
        "th api login".bright_cyan(),
        "— sign in to the".dimmed(),
        gradient::smoo_ai(),
        "platform".dimmed()
    );
    println!();
    println!("{}", "Help".bold().bright_yellow());
    println!("  {}                 list every subcommand", "th --help".bright_cyan());
    println!("  {}  drill into a subcommand", "th <subcommand> --help".bright_cyan());
}

/// Launch smooth-code — THE Smooth experience.
/// Auto-starts Big Smooth if not running.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
async fn cmd_code(
    headless: bool,
    message: Option<String>,
    file: Option<String>,
    model: Option<String>,
    budget: Option<f64>,
    json: bool,
    resume: Option<String>,
    list: bool,
    agent: Option<String>,
) -> Result<()> {
    // Validate the agent name at CLI time so a typo doesn't waste a
    // runner spin-up. The value flows into the TUI's status bar and
    // into dispatch's `agent` field when the user sends a message.
    let agent_name = resolve_primary_agent(agent.as_deref())?;
    // (The bench-based `--auto-approve` headless resolver POSTed to :4400 — gone
    // with the :4400 nuke. Operator HITL replaces it: th-1ea4f6.)
    // `--list` short-circuits everything else and prints a simple
    // table of saved sessions, newest first, then exits without
    // launching the TUI.
    if list {
        let mgr = smooth_code::session::SessionManager::new()?;
        let sessions = mgr.list()?;
        if sessions.is_empty() {
            println!("  {} No saved sessions yet. Start one with `th`.", "ℹ".cyan());
        } else {
            println!("\n  {}", "Saved sessions".cyan().bold());
            for s in &sessions {
                let label = s.display_label();
                let short_id: String = s.id.chars().take(8).collect();
                println!(
                    "  {} {:<34} {} {}",
                    "•".dimmed(),
                    label.bold(),
                    short_id.dimmed(),
                    s.updated_at.format("%Y-%m-%d %H:%M").to_string().dimmed()
                );
            }
            println!();
            println!("  {} {}", "↻".dimmed(), "th --resume                  resume most recent".dimmed());
            println!("  {} {}", "↻".dimmed(), "th --resume <id-prefix>      resume by id".dimmed());
            println!("  {} {}", "↻".dimmed(), "th --resume <title-substr>   resume by title match".dimmed());
            println!();
        }
        return Ok(());
    }

    // Resolve `--resume [query]` against the session store. None here
    // means "no --resume flag"; Some("") means "--resume with no
    // argument → pick most recently updated"; Some(q) means "match
    // this query".
    let resumed_session = if let Some(query) = resume.as_deref() {
        let mgr = smooth_code::session::SessionManager::new()?;
        let summary = if query.is_empty() { mgr.most_recent()? } else { mgr.find_by_query(query)? };
        match summary {
            Some(s) => {
                let loaded = mgr.load(&s.id)?;
                println!(
                    "  {} {} {}",
                    "↻".cyan(),
                    "Resuming".bold(),
                    loaded.title.as_deref().unwrap_or(&loaded.id).bold()
                );
                Some(loaded)
            }
            None => {
                let hint = if query.is_empty() {
                    "No saved sessions yet".to_string()
                } else {
                    format!("No session matched '{query}'. Run `th code --list` to see saved ones.")
                };
                anyhow::bail!(hint);
            }
        }
    } else {
        None
    };

    if headless {
        let working_dir = std::env::current_dir()?;
        let msg = message
            .or_else(|| file.and_then(|f| std::fs::read_to_string(f).ok()))
            .or_else(read_stdin)
            .ok_or_else(|| anyhow::anyhow!("--message, --file, or stdin required for headless mode"))?;
        // Pearl th-c39b9a: when --agent is not explicitly pinned,
        // run the intent classifier so headless mirrors the TUI's
        // routing behavior. Without this, the default `agent_name`
        // is "fixer" and a question like "what does this repo do"
        // dispatches into the coding workflow, write_files a fake
        // implementation, and burns a minute hallucinating. The
        // TUI's `run_agent_streaming` already does this; we just
        // missed wiring it on the headless path.
        // Pearl th-e0f812: when no agent is pinned, also let chief
        // pick a skill. If chief picks one, prepend its body to the
        // message so the agent follows the recipe verbatim. The
        // skill discovery happens BEFORE we hand off to the runner,
        // so this works for the headless path too.
        let (dispatch_agent, msg_with_skill) = if agent.is_some() {
            (agent_name, msg)
        } else {
            let (intent, skill_name) = smooth_code::intent::classify_with_skill(&msg).await;
            let role = intent.role().to_string();
            let composed = if let Some(name) = skill_name {
                let workspace = working_dir.clone();
                let skills = smooth_cast::skills::discover(&workspace);
                if let Some(skill) = skills.iter().find(|s| s.name == name) {
                    let source_label = match skill.source {
                        smooth_cast::skills::SkillSource::Project => "project",
                        smooth_cast::skills::SkillSource::UserSmooth => "user-smooth",
                        smooth_cast::skills::SkillSource::ClaudeCode => "claude-code",
                        smooth_cast::skills::SkillSource::OpenCode => "opencode",
                        smooth_cast::skills::SkillSource::Builtin => "builtin",
                    };
                    // Pearl th-e0f812: tell the headless caller a skill
                    // was picked. stderr so `--json` consumers parsing
                    // stdout don't get tripped.
                    eprintln!("✦ Using skill: {} (from {})", skill.name, source_label);
                    format!(
                        "## Skill: {} (from {})\n\n{}\n\n---\n\n## User request\n\n{}",
                        skill.name, source_label, skill.body, msg,
                    )
                } else {
                    msg
                }
            } else {
                msg
            };
            (role, composed)
        };
        return smooth_code::headless::run_headless(working_dir, msg_with_skill, model, budget, json, Some(dispatch_agent)).await;
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

    // EPIC th-c89c2a: `th code` no longer boots the bespoke :4400 Big Smooth
    // (the microVM Safehouse). The chat client (`OperatorClient`) lazily starts
    // the operator on :8787 via `ensure_server` on the first message, so there
    // is nothing to boot at startup — go straight to the TUI.

    // Launch smooth-code TUI — with a resumed session if one was picked.
    //
    // CRITICAL: pass the *original* `agent: Option<String>` here, not
    // the resolved `agent_name`. `agent_name` is non-optional (defaults
    // to "fixer" for the typo-validation call above), so passing
    // `Some(agent_name)` to run_with_session would PIN every fresh
    // session to fixer and bypass the intent classifier entirely.
    // Passing the original Option lets app::run_with_session see
    // `None` when the user didn't supply `--agent` and route through
    // the classifier per-message.
    let working_dir = std::env::current_dir()?;
    let _ = agent_name; // keep the typo-validation call; value isn't used in TUI mode
                        // Pearl th-20574a: thread the user's --model flag into the TUI
                        // path. Before this, `model` was parsed by clap then silently
                        // dropped here — every TaskStart picked the default smooth-coding
                        // alias regardless of what the user asked for.
    smooth_code::app::run_with_session(working_dir, resumed_session, agent, model).await
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
    println!("{} {}", gradient::smooth(), "Doctor".bold().cyan());
    println!("{}", "checking system health...\n".dimmed());

    let mut issues = 0;

    // 1. Check the operator daemon (`th daemon`, :8787).
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build()?;
    match client.get("http://localhost:8787/health").send().await {
        Ok(r) if r.status().is_success() => {
            println!("  {} Big {} API: {}", "✓".green().bold(), gradient::smooth(), "healthy".green());
        }
        Ok(r) => {
            println!(
                "  {} Big {} API: {}",
                "✗".red().bold(),
                gradient::smooth(),
                format!("unhealthy (status {})", r.status()).red()
            );
            issues += 1;
        }
        Err(_) => {
            println!(
                "  {} Big {} API: {}",
                "✗".red().bold(),
                gradient::smooth(),
                "not running (start with: th up)".red()
            );
            issues += 1;
        }
    }

    // 2. Check Dolt store
    let dolt_dir = dirs_next::home_dir().unwrap_or_default().join(".smooth").join("dolt");
    if dolt_dir.exists() {
        println!("  {} Dolt store: {}", "✓".green().bold(), format!("OK ({})", dolt_dir.display()).green());
    } else {
        println!("  {} Dolt store: {}", "○".dimmed(), "not created yet (will be created on first run)".dimmed());
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
            println!("  {} {} home: {}", "✓".green().bold(), gradient::smooth(), format!("{}", dir.display()).green());
        } else {
            println!(
                "  {} {} home: {}",
                "○".dimmed(),
                gradient::smooth(),
                format!("will be created at {}", dir.display()).dimmed()
            );
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
        println!("{} {} {}", "All checks passed.".green().bold(), gradient::smooth(), "is ready.".green().bold());
    } else {
        println!("{}", format!("{issues} issue(s) found. Fix them and run: th doctor").yellow().bold());
    }

    Ok(())
}













fn cmd_doctor_init_home_repo(remote: Option<&str>) -> Result<()> {
    let home = dirs_next::home_dir().context("cannot determine home directory")?;
    let smooth_home = home.join(".smooth");
    std::fs::create_dir_all(&smooth_home)?;

    println!(
        "\n  {} {} {}",
        gradient::smooth(),
        "home repo".bold().cyan(),
        smooth_home.display().to_string().dimmed()
    );

    let git = |args: &[&str]| -> Result<std::process::Output> {
        let out = std::process::Command::new("git")
            .current_dir(&smooth_home)
            .args(args)
            .output()
            .context("spawn git")?;
        Ok(out)
    };

    // Seed .gitignore before `git init` runs so the first status is clean.
    let gitignore_path = smooth_home.join(".gitignore");
    if !gitignore_path.exists() {
        std::fs::write(
            &gitignore_path,
            r"# Secrets — never commit LLM keys / Jira tokens
providers.json

# High-churn / ephemeral state
service.log
service.err
smooth.log
smooth.pid
smooth.db
smooth.db-journal
smooth.db-wal
smooth.db-shm

# Rotating audit logs
audit/

# Dolt store has its own push/pull via `th pearls push/pull`
dolt/

# Project-scoped sandbox caches — machine-local, large
project-cache/
pearl-env/

# Debug / session captures — ephemeral runtime artifacts
coding-sessions/
llm-errors/
",
        )?;
        println!("  {} wrote .gitignore", "✓".green().bold());
    } else {
        println!("  {} .gitignore already present — leaving as-is", "○".dimmed());
    }

    // Is this already a git repo?
    let is_repo = smooth_home.join(".git").exists();
    if !is_repo {
        let out = git(&["init", "-q"])?;
        if !out.status.success() {
            anyhow::bail!("git init failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        println!("  {} git init", "✓".green().bold());
    } else {
        println!("  {} already a git repo", "○".dimmed());
    }

    // Stage everything that survives .gitignore.
    let add = git(&["add", "-A"])?;
    if !add.status.success() {
        anyhow::bail!("git add failed: {}", String::from_utf8_lossy(&add.stderr).trim());
    }

    // Only commit if there's something to commit.
    let diff = git(&["diff", "--cached", "--quiet"])?;
    let anything_staged = !diff.status.success(); // non-zero = changes staged
    if anything_staged {
        let msg = if is_repo {
            "th doctor: sync Smooth home config"
        } else {
            "th doctor: initial Smooth home commit"
        };
        let commit = git(&["commit", "-q", "-m", msg])?;
        if !commit.status.success() {
            let stderr = String::from_utf8_lossy(&commit.stderr);
            if stderr.contains("user.email") || stderr.contains("user.name") {
                println!("  {} git has no user.email/user.name configured globally — commit skipped", "!".yellow().bold());
                println!("  {} set them with: git config --global user.email \"you@example.com\"", "→".dimmed());
            } else {
                anyhow::bail!("git commit failed: {}", stderr.trim());
            }
        } else {
            println!("  {} committed: {msg}", "✓".green().bold());
        }
    } else {
        println!("  {} nothing new to commit", "○".dimmed());
    }

    // Remote handling: add or replace.
    if let Some(url) = remote {
        let existing = git(&["remote", "get-url", "origin"])?;
        if existing.status.success() {
            let current = String::from_utf8_lossy(&existing.stdout).trim().to_string();
            if current == url {
                println!("  {} origin already set to {url}", "○".dimmed());
            } else {
                let set = git(&["remote", "set-url", "origin", url])?;
                if set.status.success() {
                    println!("  {} updated origin: {url}", "✓".green().bold());
                }
            }
        } else {
            let add_remote = git(&["remote", "add", "origin", url])?;
            if add_remote.status.success() {
                println!("  {} added origin: {url}", "✓".green().bold());
            } else {
                anyhow::bail!("git remote add failed: {}", String::from_utf8_lossy(&add_remote.stderr).trim());
            }
        }
        println!(
            "  {} push with: {}",
            "→".dimmed(),
            format!("git -C {} push -u origin main", smooth_home.display()).cyan()
        );
    } else if !is_repo {
        println!("  {} add a remote later: th doctor --init-home-repo --remote <git-url>", "→".dimmed());
    }

    println!();
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

/// Returns the pearl store along with the on-disk dolt_dir, so
/// callers that need both don't have to walk the tree twice. The
/// dolt_dir is what `auto_commit_pearl_state` needs to find the
/// enclosing git repo.
fn open_pearl_store_with_path() -> Result<(smooth_pearls::PearlStore, std::path::PathBuf)> {
    let dolt_dir = find_dolt_dir()?;
    let store = smooth_pearls::PearlStore::open(&dolt_dir)?;
    Ok((store, dolt_dir))
}

// ── Agent messaging (th agent / th msg) ─────────────────────────────
//
// Pearl th-70aaef. A harness-agnostic mailbox on top of the pearl Dolt
// store: any process that can run `th` registers an identity and polls
// for messages. Writes are committed + git-synced like pearl mutations
// so they reach other sessions/machines via refs/dolt/data.

/// This session's default agent handle: `$SMOOTH_AGENT`, else `user@host`.
fn default_agent_name() -> String {
    if let Ok(n) = std::env::var("SMOOTH_AGENT") {
        let n = n.trim();
        if !n.is_empty() {
            return n.to_string();
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
    // Keep the short hostname (strip any domain).
    let host = host.split('.').next().unwrap_or(&host);
    format!("{user}@{host}")
}

/// Harness tag: `$SMOOTH_HARNESS`, else `unknown`.
fn default_harness() -> String {
    std::env::var("SMOOTH_HARNESS")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Commit the messaging write to the Dolt store and git, best-effort,
/// so it syncs via refs/dolt/data. Mirrors what pearl mutations do.
fn commit_messaging_state(store: &smooth_pearls::PearlStore, dolt_dir: &std::path::Path, action: &str) {
    if let Err(e) = store.dolt().commit(action) {
        // "nothing to commit" is normal when the write was a no-op.
        tracing::debug!(error = %e, "messaging commit returned error (likely no-op)");
    }
    if let Err(e) = auto_commit_pearl_state(dolt_dir, action) {
        tracing::debug!(error = %e, "messaging git auto-commit skipped");
    }
}

/// Best-effort push of the messaging/pearl state to the repo's
/// `refs/dolt/data` remote so other clones/machines on the same repo see
/// it. Messages live in the pearl store, which syncs over the repo's git
/// origin — so a `send`/`register` that only commits locally won't reach a
/// teammate's clone until a push. Quiet by design: a missing remote (the
/// global `~/.smooth/dolt`, or a project with no origin) or being offline
/// is a silent no-op — never an error, and never a stray `fatal:` on
/// stderr (we drive only `dolt push`, which captures its own output; the
/// git-side `git_push_pearl_state` inherits git's stderr and is only for
/// the legacy tracked-store model, so it's not used here). Pearl th-bdaaa7.
fn sync_push_messaging(dolt_dir: &std::path::Path) {
    let Ok(dolt) = smooth_pearls::SmoothDolt::new(dolt_dir) else { return };
    match dolt.push_with(smooth_pearls::PushOpts {
        force: false,
        set_upstream: false,
    }) {
        Ok(_) => {}
        Err(e) if is_no_upstream_error(&e) => {
            // First push to a fresh remote — retry establishing upstream.
            let _ = dolt.push_with(smooth_pearls::PushOpts {
                force: false,
                set_upstream: true,
            });
        }
        Err(e) => tracing::debug!(error = %e, "messaging push skipped (no remote / offline)"),
    }
}

/// Best-effort pull so the local store reflects messages sent from other
/// clones/machines on the same repo. Quiet on no-remote/offline (drives
/// only `dolt pull`, which captures its own output).
fn sync_pull_messaging(dolt_dir: &std::path::Path) {
    if let Ok(dolt) = smooth_pearls::SmoothDolt::new(dolt_dir) {
        let _ = dolt.pull();
    }
}

fn print_message(m: &smooth_pearls::Message) {
    let when = m.created_at.format("%Y-%m-%d %H:%M").to_string();
    let unread = if m.read_at.is_none() {
        "●".yellow().to_string()
    } else {
        "○".dimmed().to_string()
    };
    let thread = m.thread_id.as_ref().map(|t| format!(" {}", format!("(re {t})").dimmed())).unwrap_or_default();
    println!(
        "{unread} {} {} → {}{thread}  {}",
        m.id.dimmed(),
        m.from_agent.cyan(),
        m.to_agent.green(),
        when.dimmed(),
    );
    for line in m.body.lines() {
        println!("    {line}");
    }
}

async fn cmd_agent(cmd: AgentCommands) -> Result<()> {
    let (store, dolt_dir) = open_pearl_store_with_path()?;
    let reg = smooth_pearls::AgentRegistry::new(store.dolt().clone());
    match cmd {
        AgentCommands::Register { name, harness, no_push } => {
            let name = name.unwrap_or_else(default_agent_name);
            let harness = harness.unwrap_or_else(default_harness);
            let pid = i64::from(std::process::id());
            reg.register(&name, &harness, Some(pid))?;
            commit_messaging_state(&store, &dolt_dir, &format!("agent register {name}"));
            if !no_push {
                sync_push_messaging(&dolt_dir);
            }
            println!("{} registered as {} ({})", "✓".green().bold(), name.green().bold(), harness.dimmed());
            println!("  {} continuously check: {}", "→".dimmed(), format!("th msg watch --agent {name}").cyan());
        }
        AgentCommands::List { json } => {
            let agents = reg.list()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&agents)?);
            } else if agents.is_empty() {
                println!("No agents registered. Run: {}", "th agent register".cyan());
            } else {
                println!("{}", format!("{} registered agent(s):", agents.len()).bold());
                for a in &agents {
                    let pid = a.pid.map(|p| format!(" pid={p}")).unwrap_or_default();
                    println!(
                        "  {} {}  {}{pid}  last-seen {}",
                        if a.status == "online" {
                            "●".green().to_string()
                        } else {
                            "○".dimmed().to_string()
                        },
                        a.name.bold(),
                        a.harness.dimmed(),
                        a.last_seen.format("%Y-%m-%d %H:%M").to_string().dimmed(),
                    );
                }
            }
        }
        AgentCommands::Offline { name } => {
            let name = name.unwrap_or_else(default_agent_name);
            reg.set_status(&name, "offline")?;
            commit_messaging_state(&store, &dolt_dir, &format!("agent offline {name}"));
            sync_push_messaging(&dolt_dir);
            println!("{} {} marked offline", "✓".green().bold(), name);
        }
    }
    Ok(())
}

async fn cmd_msg(cmd: MsgCommands) -> Result<()> {
    let (store, dolt_dir) = open_pearl_store_with_path()?;
    let mb = smooth_pearls::Mailbox::new(store.dolt().clone());
    let reg = smooth_pearls::AgentRegistry::new(store.dolt().clone());
    match cmd {
        MsgCommands::Send { to, body, from, re, no_push } => {
            let from = from.unwrap_or_else(default_agent_name);
            // Reply threads inherit the original message's thread root.
            let thread_id = match re {
                Some(ref rid) => mb.get(rid)?.map(|m| m.thread_root().to_string()),
                None => None,
            };
            let id = mb.send(&from, &to, &body, thread_id.as_deref())?;
            commit_messaging_state(&store, &dolt_dir, &format!("msg {id} {from}->{to}"));
            if !no_push {
                sync_push_messaging(&dolt_dir);
            }
            println!("{} sent {} to {}", "✓".green().bold(), id.dimmed(), to.green());
        }
        MsgCommands::Inbox {
            agent,
            pull,
            unread,
            mark_read,
            limit,
            json,
        } => {
            let who = agent.unwrap_or_else(default_agent_name);
            if pull {
                sync_pull_messaging(&dolt_dir);
            }
            let _ = reg.touch(&who); // heartbeat (best-effort)
            let msgs = mb.inbox(&who, unread, limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&msgs)?);
            } else if msgs.is_empty() {
                println!("{}", format!("Inbox for {who} is empty{}.", if unread { " (no unread)" } else { "" }).dimmed());
            } else {
                println!("{}", format!("{} message(s) for {who}:", msgs.len()).bold());
                for m in &msgs {
                    print_message(m);
                }
            }
            if mark_read && !msgs.is_empty() {
                for m in &msgs {
                    mb.mark_read(&m.id)?;
                }
                commit_messaging_state(&store, &dolt_dir, &format!("msg mark-read inbox {who}"));
            }
        }
        MsgCommands::Read { id } => {
            mb.mark_read(&id)?;
            commit_messaging_state(&store, &dolt_dir, &format!("msg read {id}"));
            println!("{} {} marked read", "✓".green().bold(), id.dimmed());
        }
        MsgCommands::Reply { id, body, from } => {
            let from = from.unwrap_or_else(default_agent_name);
            let Some(orig) = mb.get(&id)? else {
                anyhow::bail!("no message {id}");
            };
            let root = orig.thread_root().to_string();
            let to = orig.from_agent.clone();
            let new_id = mb.send(&from, &to, &body, Some(&root))?;
            commit_messaging_state(&store, &dolt_dir, &format!("msg reply {new_id} -> {to}"));
            sync_push_messaging(&dolt_dir);
            println!("{} replied {} to {}", "✓".green().bold(), new_id.dimmed(), to.green());
        }
        MsgCommands::Thread { id } => {
            let Some(m) = mb.get(&id)? else {
                anyhow::bail!("no message {id}");
            };
            let thread = mb.thread(m.thread_root())?;
            println!("{}", format!("Thread {} ({} message(s)):", m.thread_root(), thread.len()).bold());
            for msg in &thread {
                print_message(msg);
            }
        }
        MsgCommands::Watch { agent, interval, no_pull } => {
            let who = agent.unwrap_or_else(default_agent_name);
            println!(
                "👀 watching inbox for {} (every {interval}s{}). Ctrl-C to stop.",
                who.green().bold(),
                if no_pull { ", local only" } else { ", pulling remote" }
            );
            let interval = std::time::Duration::from_secs(interval.max(1));
            loop {
                if !no_pull {
                    sync_pull_messaging(&dolt_dir);
                }
                let _ = reg.touch(&who);
                match mb.inbox(&who, true, 200) {
                    Ok(msgs) => {
                        for m in &msgs {
                            print_message(m);
                            // Consume: mark read locally so it doesn't repeat.
                            let _ = mb.mark_read(&m.id);
                        }
                    }
                    Err(e) => eprintln!("{} inbox poll failed: {e}", "!".yellow()),
                }
                std::thread::sleep(interval);
            }
        }
    }
    Ok(())
}

/// Auto-commit the on-disk pearl store state to the enclosing git
/// repo, if there is one.
///
/// Pearl mutations write to `.smooth/dolt/<db>/.dolt/noms/...` files.
/// If those changes never make it into git, the working tree silently
/// accumulates drift forever — `git status` becomes noise, teammates
/// can't sync via `git pull`, and the only "source of truth" is the
/// one machine that ran `th pearls create`.
///
/// This wraps each mutating `th pearls` subcommand so the dolt state
/// lands in git automatically. Scoped strictly to `.smooth/dolt/` so
/// it never touches the user's index or in-progress code commits.
///
/// `--no-verify` is intentional: pearl commits aren't code, running
/// clippy/fmt/tests on a status change is pure overhead and would
/// regress the UX of `th pearls update <id> --status=in_progress`.
///
/// Silent no-ops when:
/// - the global `~/.smooth/dolt` store is used (no enclosing repo
///   expected; sessions/memories don't need cross-machine sync),
/// - the project isn't a git repo,
/// - **`.smooth/dolt/` is git-ignored** (pearl `th-975dfe` beads model
///   — sync moved to `refs/dolt/data` via `th pearls push`, no git
///   commit needed; pearl `th-016296` made this a quiet no-op
///   instead of erroring on the `use -f` hint),
/// - the call is from a linked worktree (SMOODEV-1836 — see below),
/// - nothing under `.smooth/dolt/` actually changed (idempotent).
///
/// True when `dolt_dir` (relative to `repo_root`) matches a
/// `.gitignore` rule. Implements pearl th-016296's beads-model skip:
/// when the user has untracked `.smooth/dolt/`, auto-committing it
/// back into the index errors with "use -f to force-add ignored files"
/// on every pearl mutation.
///
/// `git check-ignore -q <path>` exits 0 when the path is ignored, 1
/// when it's not, 128 on error (bad invocation, not a git repo). We
/// treat anything other than 0 as "not ignored / unknown" so the
/// caller falls through to the legacy auto-commit path — safer than
/// silently skipping if git is unhappy.
fn is_dolt_gitignored(repo_root: &std::path::Path, dolt_dir: &std::path::Path) -> bool {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["check-ignore", "-q", "--"])
        .arg(dolt_dir)
        .output()
    else {
        return false;
    };
    output.status.code() == Some(0)
}

fn auto_commit_pearl_state(dolt_dir: &std::path::Path, action: &str) -> Result<()> {
    if is_global_pearl_store(dolt_dir) {
        return Ok(());
    }

    let Some(repo_root) = git_toplevel(dolt_dir) else {
        return Ok(());
    };

    // SMOODEV-1836: never auto-commit the dolt store from a linked worktree.
    // Each worktree checks out its own copy of `.smooth/dolt/`, and Dolt
    // rewrites mutable pointer files (journal.idx, manifest, the journal
    // chunk) on every open — committing those onto a feature branch produces
    // binary pointer divergence that can't be merged back to main. Pearl
    // state belongs on the primary worktree's lineage; from a linked worktree
    // we skip the git commit (the dolt mutation + `th pearls push` to
    // refs/dolt/data still capture the change) and tell the user where to run.
    if is_linked_worktree(&repo_root) {
        tracing::warn!(
            "th pearls: skipping git auto-commit of pearl state — this is a linked \
             worktree. Run pearl mutations from the primary worktree so the dolt \
             store stays on one lineage; sync with `th pearls push`."
        );
        return Ok(());
    }

    // Pearl th-016296. Beads-model repos gitignore `.smooth/dolt/`; the
    // git add below would otherwise fail with "use -f to force-add ignored
    // files" on every pearl mutation. Check ahead of time with
    // `git check-ignore -q .smooth/dolt/` (exit 0 = ignored, 1 = not
    // ignored, 128 = error). Silent skip on the ignored case is correct:
    // sync happens via `th pearls push` to refs/dolt/data, not via git
    // commits of the on-disk files.
    if is_dolt_gitignored(&repo_root, dolt_dir) {
        return Ok(());
    }

    let canonical_repo = repo_root.canonicalize().unwrap_or_else(|_| repo_root.clone());
    let canonical_dolt = dolt_dir.canonicalize().unwrap_or_else(|_| dolt_dir.to_path_buf());
    let Ok(relative) = canonical_dolt.strip_prefix(&canonical_repo) else {
        // Symlink or unrelated layout: skip rather than committing
        // something the user wouldn't expect.
        return Ok(());
    };

    let add_status = std::process::Command::new("git")
        .arg("-C")
        .arg(&canonical_repo)
        .args(["add", "--"])
        .arg(relative)
        .status()
        .map_err(|e| anyhow::anyhow!("git add for pearl auto-commit failed to launch: {e}"))?;
    if !add_status.success() {
        anyhow::bail!("git add .smooth/dolt/ failed (exit {add_status})");
    }

    let diff_status = std::process::Command::new("git")
        .arg("-C")
        .arg(&canonical_repo)
        .args(["diff", "--cached", "--quiet", "--"])
        .arg(relative)
        .status()
        .map_err(|e| anyhow::anyhow!("git diff for pearl auto-commit failed to launch: {e}"))?;
    if diff_status.success() {
        // Exit 0 from --quiet means "no diff" → nothing to commit.
        return Ok(());
    }

    let msg = format!("pearl: {action}");
    let commit_status = std::process::Command::new("git")
        .arg("-C")
        .arg(&canonical_repo)
        .args(["commit", "--no-verify", "-m", &msg, "--"])
        .arg(relative)
        .status()
        .map_err(|e| anyhow::anyhow!("git commit for pearl auto-commit failed to launch: {e}"))?;
    if !commit_status.success() {
        anyhow::bail!("git commit for pearl auto-commit failed (exit {commit_status})");
    }
    Ok(())
}

/// `git rev-parse --show-toplevel` rooted at the given directory.
/// Returns `None` if not in a git repo (worktree-safe — works whether
/// `.git` is a directory or a worktree pointer file).
fn git_toplevel(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(trimmed))
}

/// True if `repo_root` is a *linked* git worktree (created by
/// `git worktree add`) rather than the repository's primary worktree.
///
/// Detection: in a linked worktree `git rev-parse --git-dir` resolves to
/// `<common>/.git/worktrees/<name>`, which differs from
/// `--git-common-dir` (`<common>/.git`). In the primary worktree the two
/// resolve to the same path. We canonicalize both before comparing so
/// relative-vs-absolute output doesn't produce a false positive. On any
/// git error we return `false` (fail toward the existing behaviour rather
/// than silently dropping a primary-worktree commit).
fn is_linked_worktree(repo_root: &std::path::Path) -> bool {
    let rev = |flag: &str| -> Option<std::path::PathBuf> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["rev-parse", flag])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8(out.stdout).ok()?;
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        // git prints paths relative to repo_root unless they're absolute.
        let p = std::path::Path::new(trimmed);
        let abs = if p.is_absolute() { p.to_path_buf() } else { repo_root.join(p) };
        Some(abs.canonicalize().unwrap_or(abs))
    };
    match (rev("--git-dir"), rev("--git-common-dir")) {
        (Some(git_dir), Some(common_dir)) => git_dir != common_dir,
        _ => false,
    }
}

/// Trim a pearl title down to a length that fits comfortably in a
/// one-line commit subject (keeps `git log --oneline` readable).
fn truncate_for_msg(s: &str) -> String {
    const MAX: usize = 72;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX - 1).collect();
    out.push('…');
    out
}

/// Run `git push` for the enclosing repo if there are pearl auto-commits
/// ahead of `@{u}`. Best-effort; returns Err with a short reason on
/// failure so the caller can log and continue with the dolt push.
fn git_push_pearl_state(dolt_dir: &std::path::Path) -> Result<()> {
    if is_global_pearl_store(dolt_dir) {
        return Ok(());
    }
    let Some(repo_root) = git_toplevel(dolt_dir) else {
        anyhow::bail!("not a git repo");
    };
    // Check whether there's anything ahead of the upstream. If
    // `@{u}` doesn't resolve (no upstream configured), just attempt
    // a `git push` which will produce its own clear error.
    let ahead = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["rev-list", "--count", "@{u}..HEAD"])
        .output();
    if let Ok(out) = ahead {
        if out.status.success() {
            let n: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
            if n == 0 {
                return Ok(());
            }
        }
    }
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .arg("push")
        .status()
        .map_err(|e| anyhow::anyhow!("failed to launch git push: {e}"))?;
    if !status.success() {
        anyhow::bail!("git push failed (exit {status})");
    }
    Ok(())
}

/// Run `git pull --rebase` for the enclosing repo. Best-effort — see
/// [`git_push_pearl_state`].
fn git_pull_pearl_state(dolt_dir: &std::path::Path) -> Result<()> {
    if is_global_pearl_store(dolt_dir) {
        return Ok(());
    }
    let Some(repo_root) = git_toplevel(dolt_dir) else {
        anyhow::bail!("not a git repo");
    };
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["pull", "--rebase"])
        .status()
        .map_err(|e| anyhow::anyhow!("failed to launch git pull: {e}"))?;
    if !status.success() {
        anyhow::bail!("git pull --rebase failed (exit {status})");
    }
    Ok(())
}

#[cfg(test)]
mod pearl_autocommit_tests {
    use super::*;
    use std::process::Command;

    fn git(args: &[&str], cwd: &std::path::Path) {
        let out = Command::new("git").arg("-C").arg(cwd).args(args).output().expect("git");
        assert!(out.status.success(), "git {args:?} in {cwd:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        git(&["init", "--initial-branch=main"], dir.path());
        git(&["config", "user.email", "test@example.com"], dir.path());
        git(&["config", "user.name", "Test"], dir.path());
        git(&["config", "commit.gpgsign", "false"], dir.path());
        std::fs::create_dir_all(dir.path().join(".smooth/dolt")).unwrap();
        std::fs::write(dir.path().join("README.md"), "init\n").unwrap();
        git(&["add", "."], dir.path());
        git(&["commit", "--no-verify", "-m", "initial"], dir.path());
        dir
    }

    #[test]
    fn truncate_for_msg_short_passes_through() {
        assert_eq!(truncate_for_msg("hello"), "hello");
    }

    #[test]
    fn truncate_for_msg_long_truncates_with_ellipsis() {
        let long: String = "x".repeat(100);
        let out = truncate_for_msg(&long);
        assert!(out.chars().count() <= 72);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn auto_commit_skips_outside_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        let dolt = dir.path().join(".smooth/dolt");
        std::fs::create_dir_all(&dolt).unwrap();
        std::fs::write(dolt.join("foo"), "bar").unwrap();
        // No git init — should be a silent no-op.
        auto_commit_pearl_state(&dolt, "test").expect("should not error outside git repo");
    }

    #[test]
    fn auto_commit_skips_when_nothing_changed() {
        let dir = init_repo();
        let dolt = dir.path().join(".smooth/dolt");
        let before = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        auto_commit_pearl_state(&dolt, "no-op").expect("idempotent");
        let after = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(before, after, "no commit should have been created");
    }

    #[test]
    fn auto_commit_creates_commit_on_change() {
        let dir = init_repo();
        let dolt = dir.path().join(".smooth/dolt");
        std::fs::write(dolt.join("new_file"), "pearl state").unwrap();
        auto_commit_pearl_state(&dolt, "create th-deadbe Test pearl").expect("commits");
        let log = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["log", "--oneline", "-1"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert!(log.contains("pearl: create th-deadbe Test pearl"), "got: {log}");
    }

    #[test]
    fn auto_commit_only_stages_smooth_dolt() {
        let dir = init_repo();
        let dolt = dir.path().join(".smooth/dolt");
        // User has unstaged code changes in their working tree.
        std::fs::write(dir.path().join("src.rs"), "user code").unwrap();
        // Pearl state changes too.
        std::fs::write(dolt.join("new_file"), "pearl state").unwrap();

        auto_commit_pearl_state(&dolt, "test scoped").expect("commits");

        // The user's `src.rs` should still be untracked — auto-commit
        // must not have swept up files outside `.smooth/dolt/`.
        let status = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["status", "--porcelain", "src.rs"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert!(status.contains("?? src.rs"), "expected src.rs to remain untracked, got: {status:?}");

        // Verify the pearl commit landed by name-pattern (the legacy
        // tracked-binary model). Continued below.
        let files = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["show", "--name-only", "--pretty=format:", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        for line in files.lines().filter(|l| !l.is_empty()) {
            assert!(line.starts_with(".smooth/dolt/"), "auto-commit included non-pearl path: {line}");
        }
    }

    /// Pearl th-016296: when `.smooth/dolt/` is gitignored (the
    /// beads-model repos after pearl `th-975dfe`), auto-commit must
    /// silently no-op. Previously the function ran `git add
    /// .smooth/dolt/` unconditionally and errored with "use -f to
    /// force-add ignored files" on every pearl mutation.
    #[test]
    fn auto_commit_silent_noop_when_dolt_gitignored() {
        let dir = init_repo();
        let dolt = dir.path().join(".smooth/dolt");
        // Add the gitignore entry the way pearl th-975dfe writes it.
        std::fs::write(dir.path().join(".gitignore"), ".smooth/dolt/\n").unwrap();
        git(&["add", ".gitignore"], dir.path());
        git(&["commit", "--no-verify", "-m", "gitignore"], dir.path());

        let head_before = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();

        // Touch the dolt store like a pearl mutation would.
        std::fs::write(dolt.join("noms_file"), "pearl state changed").unwrap();

        // Must not error, must not create a new commit.
        auto_commit_pearl_state(&dolt, "mutation that should not commit").expect("noop");

        let head_after = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(head_before, head_after, "beads-model repo must NOT create a pearl auto-commit");
    }

    #[test]
    fn is_dolt_gitignored_returns_true_when_ignored() {
        let dir = init_repo();
        let dolt = dir.path().join(".smooth/dolt");
        std::fs::write(dir.path().join(".gitignore"), ".smooth/dolt/\n").unwrap();
        git(&["add", ".gitignore"], dir.path());
        git(&["commit", "--no-verify", "-m", "gitignore"], dir.path());
        assert!(is_dolt_gitignored(dir.path(), &dolt));
    }

    #[test]
    fn is_dolt_gitignored_returns_false_when_not_ignored() {
        let dir = init_repo();
        let dolt = dir.path().join(".smooth/dolt");
        assert!(!is_dolt_gitignored(dir.path(), &dolt));
    }

    #[test]
    fn is_dolt_gitignored_returns_false_on_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let dolt = dir.path().join(".smooth/dolt");
        std::fs::create_dir_all(&dolt).unwrap();
        // git check-ignore returns 128 outside a repo; helper treats
        // that as "not ignored / unknown" so callers fall through to
        // the legacy auto-commit path.
        assert!(!is_dolt_gitignored(dir.path(), &dolt));
    }
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
    // `Init` runs *before* a store exists, so opening one here would
    // fail with "no .smooth/dolt/ found". Handle it up front; every
    // other subcommand needs an existing store.
    if matches!(cmd, PearlCommands::Init) {
        return cmd_pearls_init().await;
    }
    let (store, dolt_dir) = open_pearl_store_with_path()?;

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
            auto_commit_pearl_state(&dolt_dir, &format!("create {} {}", issue.id, truncate_for_msg(&issue.title)))?;
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
            auto_commit_pearl_state(&dolt_dir, &format!("update {}", updated.id))?;
        }

        PearlCommands::Close { ids } => {
            let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            let count = store.close(&id_refs)?;
            println!("{} Closed {count} issue(s)", "✓".green().bold());
            auto_commit_pearl_state(&dolt_dir, &format!("close {}", ids.join(", ")))?;
        }

        PearlCommands::Reopen { id } => {
            let issue = store.reopen(&id)?;
            println!("{} Reopened {}", "✓".green().bold(), issue.id);
            println!("  {}", format_pearl_line(&issue));
            auto_commit_pearl_state(&dolt_dir, &format!("reopen {}", issue.id))?;
        }

        PearlCommands::Dep { cmd } => match cmd {
            DepCommands::Add { issue, depends_on } => {
                store.add_dep(&issue, &depends_on)?;
                println!("{} {issue} now depends on {depends_on}", "✓".green().bold());
                auto_commit_pearl_state(&dolt_dir, &format!("dep add {issue} → {depends_on}"))?;
            }
            DepCommands::Remove { issue, depends_on } => {
                store.remove_dep(&issue, &depends_on)?;
                println!("{} Removed dependency {issue} → {depends_on}", "✓".green().bold());
                auto_commit_pearl_state(&dolt_dir, &format!("dep remove {issue} → {depends_on}"))?;
            }
        },

        PearlCommands::Comment { id, content } => {
            let comment = store.add_comment(&id, &content)?;
            println!("{} Comment added ({})", "✓".green().bold(), comment.id.dimmed());
            auto_commit_pearl_state(&dolt_dir, &format!("comment on {id}"))?;
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
                auto_commit_pearl_state(&dolt_dir, &format!("label add {id} +{label}"))?;
            }
            LabelCommands::Remove { label } => {
                store.remove_label(&id, &label)?;
                println!("{} Removed label \"{label}\" from {id}", "✓".green().bold());
                auto_commit_pearl_state(&dolt_dir, &format!("label remove {id} -{label}"))?;
            }
        },

        PearlCommands::MigrateFromBeads => {
            cmd_migrate_from_beads(&store)?;
            auto_commit_pearl_state(&dolt_dir, "migrate from beads")?;
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

        // ── Memory + prime (pearl th-202885) ─────────────────────────
        PearlCommands::Remember { text, source } => {
            let mem = smooth_pearls::MemoryStore::new(store.dolt().clone());
            let id = mem.append(&text, &source)?;
            commit_messaging_state(&store, &dolt_dir, &format!("remember {id}"));
            println!("{} remembered {} ({})", "✓".green().bold(), id.green().bold(), source.dimmed());
        }
        PearlCommands::Memories { limit, source, json } => {
            let mem = smooth_pearls::MemoryStore::new(store.dolt().clone());
            let items = match &source {
                Some(s) => mem.list_by_source(s, limit)?,
                None => mem.list_recent(limit)?,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else if items.is_empty() {
                println!("{}", "No memories yet. Record one: th pearls remember \"…\"".dimmed());
            } else {
                println!("{}", format!("{} memory(ies):", items.len()).bold());
                for m in &items {
                    println!(
                        "  {} {}  {}",
                        m.id.dimmed(),
                        m.content,
                        format!("[{}] {}", m.source, m.created_at.format("%Y-%m-%d")).dimmed()
                    );
                }
            }
        }
        PearlCommands::Forget { id } => {
            let mem = smooth_pearls::MemoryStore::new(store.dolt().clone());
            if mem.forget(&id)? {
                commit_messaging_state(&store, &dolt_dir, &format!("forget {id}"));
                println!("{} forgot {id}", "✓".green().bold());
            } else {
                println!("{} no memory with id {id}", "✗".red());
            }
        }
        PearlCommands::Prime { memories, json } => {
            let mem = smooth_pearls::MemoryStore::new(store.dolt().clone());
            let open = store.list(&smooth_pearls::PearlQuery::new().with_status(smooth_pearls::PearlStatus::Open))?;
            let in_progress = store.list(&smooth_pearls::PearlQuery::new().with_status(smooth_pearls::PearlStatus::InProgress))?;
            let notes = mem.list_recent(memories)?;
            if json {
                let payload = serde_json::json!({
                    "in_progress": in_progress,
                    "open": open,
                    "memories": notes,
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!("{}", "# Project priming context".bold());
                println!("\n{}", format!("In progress ({}):", in_progress.len()).bold());
                for p in in_progress.iter().take(20) {
                    println!("  {}", format_pearl_line(p));
                }
                println!("\n{}", format!("Open / ready ({}):", open.len()).bold());
                for p in open.iter().take(20) {
                    println!("  {}", format_pearl_line(p));
                }
                println!("\n{}", format!("Recent memories ({}):", notes.len()).bold());
                for m in &notes {
                    println!("  • {} {}", m.content, format!("[{}]", m.source).dimmed());
                }
            }
        }

        // ── Dolt commands ────────────────────────────────────────────
        // `Init` is handled before the match above (no store exists yet).
        PearlCommands::Init => unreachable!("Init is handled at the top of cmd_pearls"),

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

        PearlCommands::Push { force } => {
            // Before pushing dolt, push any pending git commits under
            // `.smooth/dolt/` so teammates' `git pull` brings the same
            // pearl state down. Best-effort: log and continue on a
            // git failure (e.g. no remote, detached HEAD) so the
            // dolt push still runs.
            if let Err(e) = git_push_pearl_state(&dolt_dir) {
                eprintln!("(git push for pearl state skipped: {e})");
            }
            // Global store at `~/.smooth/dolt` is intentionally
            // single-machine — sessions, memories, and personal-scope
            // pearls don't need cross-machine sync. Treat "no remote
            // configured" there as a no-op rather than an error so
            // `th pearls push` is safe to script unconditionally.
            // Project stores still surface the error so the user
            // notices a missing remote on a shared board.
            let dolt = smooth_pearls::SmoothDolt::new(&dolt_dir)?;

            // Try a plain push first. Two recoverable failures get
            // a friendlier outcome than the raw Dolt error:
            //   1. "no upstream branch" — first push to a fresh
            //      remote. Auto-retry with -u so the user doesn't
            //      need to know the flag exists.
            //   2. "no common ancestor" — the remote was init'd
            //      independently (typically by an earlier abandoned
            //      th pearls init somewhere else) and shares no
            //      history with the local store. The bare Dolt
            //      error is opaque; we surface a clear next step.
            let opts = smooth_pearls::PushOpts { force, set_upstream: false };
            match dolt.push_with(opts) {
                Ok(output) => println!("{output}"),
                Err(e) if is_global_pearl_store(&dolt_dir) && is_no_remote_error(&e) => {
                    println!("(global pearl store at {} has no remote — push skipped, this is expected)", dolt_dir.display());
                }
                Err(e) if is_no_upstream_error(&e) => {
                    println!("(no upstream — retrying with --set-upstream)");
                    let retry = smooth_pearls::PushOpts { force, set_upstream: true };
                    let output = dolt.push_with(retry)?;
                    println!("{output}");
                }
                Err(e) if is_no_common_ancestor_error(&e) && !force => {
                    anyhow::bail!(
                        "{e}\n\nThe remote `refs/dolt/data` was initialized independently and shares no \
                         ancestor with the local pearl store. Two ways to fix:\n\n  \
                         1. If the remote has no real pearl data (just a bare \"Initialize data \
                         repository\" commit from an earlier setup):\n     \
                         th pearls push --force\n\n  \
                         2. To wipe the remote ref and start clean:\n     \
                         git push origin --delete refs/dolt/data && th pearls push\n\n\
                         Inspect first with: smooth-dolt clone <remote-url> /tmp/check && \
                         smooth-dolt log /tmp/check"
                    );
                }
                Err(e) => return Err(e),
            }
        }

        PearlCommands::Pull => {
            // Pull git first so any auto-commits from teammates
            // (under `.smooth/dolt/`) land in the working tree before
            // the dolt layer reads it. Best-effort: failure to git
            // pull doesn't block the dolt pull (e.g. no remote, no
            // upstream branch).
            if let Err(e) = git_pull_pearl_state(&dolt_dir) {
                eprintln!("(git pull for pearl state skipped: {e})");
            }
            let dolt = smooth_pearls::SmoothDolt::new(&dolt_dir)?;
            match dolt.pull() {
                Ok(output) => println!("{output}"),
                Err(e) if is_global_pearl_store(&dolt_dir) && is_no_remote_error(&e) => {
                    println!("(global pearl store at {} has no remote — pull skipped, this is expected)", dolt_dir.display());
                }
                Err(e) => return Err(e),
            }
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

        PearlCommands::Doctor { auto_repair, force } => {
            use smooth_pearls::dolt::DoctorDiagnosis;

            let dolt_root = find_dolt_dir()?;
            // .smooth/dolt/ is a multi-db root — each subdir with its own
            // `.dolt/` is an independent dolt repo. Probe each.
            let db_dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&dolt_root)
                .with_context(|| format!("read {}", dolt_root.display()))?
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().join(".dolt").is_dir())
                .map(|entry| entry.path())
                .collect();
            if db_dirs.is_empty() {
                anyhow::bail!("no dolt dbs found under {} — is this an initialized pearl root?", dolt_root.display());
            }

            let mut any_corrupt = false;
            let mut any_failed_repair = false;
            for db_dir in &db_dirs {
                let name = db_dir.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                println!("probing db: {} at {}", name, db_dir.display());
                let diagnosis = smooth_pearls::SmoothDolt::diagnose(db_dir);

                match diagnosis {
                    DoctorDiagnosis::Healthy => {
                        println!("  ✓ healthy");
                    }
                    DoctorDiagnosis::NotInitialized { detail } => {
                        println!("  ✗ not a valid dolt dir: {detail}");
                        any_failed_repair = true;
                    }
                    DoctorDiagnosis::ConflictMarkers { candidates } => {
                        any_corrupt = true;
                        println!("  ✗ manifest has unresolved git merge-conflict markers ({} candidate lines)", candidates.len());
                        println!("    cause: git's text-merger ran on the binary noms/manifest file.");
                        println!("    fix:  pick the right pre-merge manifest line (the longest is usually the most-recent state).");
                        for (idx, line) in candidates.iter().enumerate() {
                            println!("      [{idx}] {} chars: {}…", line.len(), line.chars().take(60).collect::<String>());
                        }
                        if !auto_repair {
                            continue;
                        }
                        match smooth_pearls::SmoothDolt::repair_manifest_conflict(db_dir, &candidates) {
                            Ok(chosen) => {
                                println!(
                                    "  ✓ wrote chosen candidate ({} chars) — original kept at manifest.with-conflicts-<ts>",
                                    chosen.len()
                                );
                            }
                            Err(e) => {
                                println!("  ✗ manifest repair failed: {e:#}");
                                any_failed_repair = true;
                                continue;
                            }
                        }
                        match smooth_pearls::SmoothDolt::diagnose(db_dir) {
                            DoctorDiagnosis::Healthy => println!("  ✓ post-repair probe healthy"),
                            other => {
                                println!("  ✗ post-repair probe still unhealthy: {other:?}");
                                println!("    Try a different candidate by hand: copy a line from manifest.with-conflicts-<ts>");
                                println!("    into .dolt/noms/manifest (no trailing newline) and re-run doctor.");
                                any_failed_repair = true;
                            }
                        }
                    }
                    DoctorDiagnosis::Corrupt { detail } => {
                        any_corrupt = true;
                        println!("  ✗ corrupt: {detail}");
                        if !auto_repair {
                            continue;
                        }

                        // Auto-repair path
                        let server_attached = smooth_pearls::dolt_server::SmoothDoltServer::try_attach(db_dir).is_some();
                        if server_attached && !force {
                            println!(
                                "  ! a smooth-dolt server is attached to this db — skipping repair.\n    \
                                 • Run `th pearls push` first if you have local writes to preserve.\n    \
                                 • Then re-run with `--force` to stop the server and re-clone."
                            );
                            any_failed_repair = true;
                            continue;
                        }
                        if server_attached {
                            println!("  stopping attached smooth-dolt server...");
                            // Drop the attach handle so the socket is released.
                            drop(smooth_pearls::dolt_server::SmoothDoltServer::try_attach(db_dir));
                            std::thread::sleep(std::time::Duration::from_millis(500));
                        }

                        let cli = match smooth_pearls::SmoothDolt::new_cli_only(db_dir) {
                            Ok(c) => c,
                            Err(e) => {
                                println!("  ✗ couldn't construct CLI handle: {e:#}");
                                any_failed_repair = true;
                                continue;
                            }
                        };
                        match cli.recover_from_remote() {
                            Ok(broken) => {
                                println!("  ✓ snapshot at: {}", broken.display());
                                println!("    delete with `rm -rf {}` once verified", broken.display());
                            }
                            Err(e) => {
                                println!("  ✗ repair failed: {e:#}");
                                any_failed_repair = true;
                                continue;
                            }
                        }

                        // Re-probe
                        match smooth_pearls::SmoothDolt::diagnose(db_dir) {
                            DoctorDiagnosis::Healthy => println!("  ✓ post-repair probe healthy"),
                            other => {
                                println!("  ✗ post-repair probe still unhealthy: {other:?}");
                                any_failed_repair = true;
                            }
                        }
                    }
                }
            }

            if any_corrupt && !auto_repair {
                anyhow::bail!(
                    "one or more dbs are corrupt. Re-run with `--auto-repair` to snapshot + re-clone\n\
                     from the configured `origin` remote for each affected db."
                );
            }
            if any_failed_repair {
                anyhow::bail!("some repairs failed — see output above");
            }
        }
    }

    Ok(())
}

/// Find the .smooth/dolt/ directory by walking up from cwd.
fn find_dolt_dir() -> Result<std::path::PathBuf> {
    let cwd = std::env::current_dir()?;
    smooth_pearls::dolt::find_repo_dolt_dir(&cwd).ok_or_else(|| anyhow::anyhow!("no .smooth/dolt/ found. Run: th pearls init"))
}

/// `th pearls init` — set up a pearl board in the cwd repo.
///
/// **Beads model** (pearl `th-975dfe`, 2026-06-13): `.smooth/dolt/` is
/// **not git-tracked**. Sync happens via dolt's own `refs/dolt/data`
/// ref pushed alongside normal git refs, the same way beads uses
/// `.beads/embeddeddolt/` + `bd dolt push/pull`. Eliminates the
/// merge-conflict class we were paying down with PR #94 + smooai
/// #1513.
///
/// Logic:
/// 1. Ensure `.gitignore` has `.smooth/dolt/` so future mutations don't
///    sweep the noms binaries back into git.
/// 2. If `.smooth/dolt/` already exists, no-op (existing local store).
/// 3. If missing AND the enclosing git repo has an `origin` URL
///    AND `refs/dolt/data` exists on that remote, clone from it.
///    This is the post-`git clone` bootstrap path: a contributor
///    checks out the repo fresh, runs `th pearls init`, and gets the
///    project's pearl history without any prior setup.
/// 4. If missing AND no origin / no remote ref, create a fresh empty
///    store. Caller can wire a remote later with `th pearls remote add`.
async fn cmd_pearls_init() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let dolt_dir = cwd.join(".smooth").join("dolt");

    // Step 1: .gitignore protection. Idempotent.
    let repo_root = hooks::find_git_root(&cwd);
    if let Some(root) = repo_root.as_ref() {
        match ensure_dolt_gitignored(root) {
            Ok(true) => println!("{} Added `.smooth/dolt/` to {}/.gitignore", "✓".green().bold(), root.display()),
            Ok(false) => {} // already ignored, quiet
            Err(e) => eprintln!("  Could not update .gitignore: {e}"),
        }
    }

    if dolt_dir.exists() {
        println!("Pearl database already initialized at {}", dolt_dir.display());
    } else if let Some(remote_url) = repo_root.as_ref().and_then(|r| read_git_origin_url(r).ok().flatten()) {
        // Step 3: post-`git clone` bootstrap. The clone subprocess
        // succeeds even when the ref doesn't exist on the remote, but
        // produces an empty store — so we accept "empty" as a valid
        // outcome rather than treating it as failure.
        println!("Bootstrapping pearl database from {remote_url} (refs/dolt/data) …");
        match smooth_pearls::dolt::clone_from(&remote_url, &dolt_dir) {
            Ok(()) => {
                println!("{} Pearl database cloned to {}", "✓".green().bold(), dolt_dir.display());
            }
            Err(e) => {
                eprintln!("  smooth-dolt clone failed: {e}");
                eprintln!("  Falling back to fresh empty store.");
                smooth_pearls::PearlStore::init(&dolt_dir)?;
                println!("{} Pearl database initialized empty at {}", "✓".green().bold(), dolt_dir.display());
            }
        }
    } else {
        // Step 4: no remote to bootstrap from — create empty.
        smooth_pearls::PearlStore::init(&dolt_dir)?;
        println!("{} Pearl database initialized at {}", "✓".green().bold(), dolt_dir.display());
        println!("  Tables: pearls, pearl_dependencies, pearl_labels, pearl_comments, pearl_history, sessions, memories");
        println!("  Run: th pearls remote add origin <git-remote-url>");
        println!("  Then: th pearls push");
    }

    // Inject the agent-messaging protocol into AGENTS.md so any agent
    // (any harness) that reads it learns to register + poll. Idempotent.
    // Pearl th-70aaef.
    if let Some(root) = repo_root.as_ref() {
        match ensure_agents_md_messaging(root) {
            Ok(true) => println!("{} Added the Agent Messaging section to {}/AGENTS.md", "✓".green().bold(), root.display()),
            Ok(false) => {} // already present, quiet
            Err(e) => eprintln!("  Could not update AGENTS.md: {e}"),
        }
    }

    // Install git hooks if not already present.
    let hooks_status = hooks::check(None);
    if !hooks_status.is_ok() {
        println!();
        match hooks::install(None) {
            Ok(hooks_dir) => hooks::print_install_result(&hooks_dir),
            Err(e) => eprintln!("  Could not install git hooks: {e}"),
        }
    }
    Ok(())
}

/// Marker that bounds the injected messaging block so re-runs replace
/// rather than duplicate it, and humans can see it's tool-managed.
const AGENTS_MSG_BEGIN: &str = "<!-- th:agent-messaging:begin -->";
const AGENTS_MSG_END: &str = "<!-- th:agent-messaging:end -->";

/// The Agent Messaging protocol block injected into AGENTS.md. Harness-
/// agnostic: every instruction is a plain `th` call.
fn agents_md_messaging_block() -> String {
    format!(
        "{AGENTS_MSG_BEGIN}\n\
## Agent Messaging (`th agent` / `th msg`)\n\
\n\
You can talk to other agents — in other sessions, other harnesses, even other\n\
machines — through a shared Dolt-backed mailbox. It's all plain `th` calls, so\n\
it works the same whether you're Claude Code, opencode, pi, or a shell loop.\n\
\n\
**On session start:**\n\
```bash\n\
th agent register --name <your-handle>     # idempotent; pick a stable name\n\
```\n\
\n\
**Continuously check for messages** (do this every few turns, or run it in the\n\
background of your session):\n\
```bash\n\
th msg inbox --unread           # what's waiting for me\n\
th msg watch                    # blocking poll loop — prints messages as they land\n\
```\n\
\n\
**Send / reply:**\n\
```bash\n\
th agent list                   # who can I reach\n\
th msg send --to <name|all> --body \"…\"\n\
th msg reply <message-id> --body \"…\"   # threads automatically\n\
th msg thread <message-id>      # read a whole conversation\n\
```\n\
\n\
Identity defaults to `$SMOOTH_AGENT` (else `user@host`); set `$SMOOTH_HARNESS`\n\
so others can see what tool you are. Sync is automatic over the repo's git\n\
remote: `send`/`register` push and `watch` pulls each poll, so agents in\n\
different clones/machines of the same repo see each other. Pass `--no-push` /\n\
`--no-pull` for a purely local, offline mailbox.\n\
{AGENTS_MSG_END}\n"
    )
}

/// Idempotently ensure the agent-messaging block is present in
/// `<repo_root>/AGENTS.md`. Replaces an existing marked block (so the
/// docs evolve with the tool); appends if absent; creates the file if
/// missing. Returns `true` when the file changed.
fn ensure_agents_md_messaging(repo_root: &std::path::Path) -> Result<bool> {
    let path = repo_root.join("AGENTS.md");
    let block = agents_md_messaging_block();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    let next = if let (Some(start), Some(end)) = (existing.find(AGENTS_MSG_BEGIN), existing.find(AGENTS_MSG_END)) {
        // Replace the existing marked region (end..end+len) in place.
        let end = end + AGENTS_MSG_END.len();
        let mut s = String::with_capacity(existing.len());
        s.push_str(&existing[..start]);
        s.push_str(block.trim_end());
        s.push_str(&existing[end..]);
        s
    } else if existing.trim().is_empty() {
        format!("# AGENTS.md\n\n{block}")
    } else {
        let sep = if existing.ends_with('\n') { "\n" } else { "\n\n" };
        format!("{existing}{sep}{block}")
    };

    if next == existing {
        return Ok(false);
    }
    std::fs::write(&path, next).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod agents_md_tests {
    use super::*;

    #[test]
    fn injects_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure_agents_md_messaging(dir.path()).unwrap());
        let body = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(body.contains("## Agent Messaging"));
        assert!(body.contains(AGENTS_MSG_BEGIN) && body.contains(AGENTS_MSG_END));
    }

    #[test]
    fn injection_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure_agents_md_messaging(dir.path()).unwrap(), "first run writes");
        assert!(!ensure_agents_md_messaging(dir.path()).unwrap(), "second run is a no-op");
        let body = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(body.matches(AGENTS_MSG_BEGIN).count(), 1, "exactly one block");
    }

    #[test]
    fn preserves_existing_content_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# AGENTS.md\n\nExisting house rules.\n").unwrap();
        assert!(ensure_agents_md_messaging(dir.path()).unwrap());
        let body = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(body.contains("Existing house rules."), "keeps prior content");
        assert!(body.contains("## Agent Messaging"));
    }

    #[test]
    fn replaces_marked_block_in_place_on_change() {
        let dir = tempfile::tempdir().unwrap();
        // Seed a stale marked block; ensure it's replaced, not duplicated.
        let stale = format!("# AGENTS.md\n\n{AGENTS_MSG_BEGIN}\nOLD CONTENT\n{AGENTS_MSG_END}\n\ntail\n");
        std::fs::write(dir.path().join("AGENTS.md"), stale).unwrap();
        assert!(ensure_agents_md_messaging(dir.path()).unwrap());
        let body = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(!body.contains("OLD CONTENT"), "stale block replaced");
        assert_eq!(body.matches(AGENTS_MSG_BEGIN).count(), 1);
        assert!(body.contains("tail"), "content after the block preserved");
    }
}

/// Append `.smooth/dolt/` to `.gitignore` at `repo_root` if not already
/// present. Returns Ok(true) when the file was modified, Ok(false) when
/// the entry already existed.
///
/// Match is line-prefix based against `.smooth/dolt` so variants like
/// `.smooth/dolt/`, `.smooth/dolt/**`, or `/.smooth/dolt/` all count
/// as "already ignored." Avoids duplicating entries when init is
/// re-run.
fn ensure_dolt_gitignored(repo_root: &std::path::Path) -> Result<bool> {
    let path = repo_root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    for line in existing.lines() {
        let trimmed = line.trim().trim_start_matches('/');
        if trimmed.starts_with(".smooth/dolt") {
            return Ok(false);
        }
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n# Pearl Dolt store — beads model: synced via refs/dolt/data, not tracked.\n");
    out.push_str(".smooth/dolt/\n");
    std::fs::write(&path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

/// Read `git remote get-url origin` for the given repo root. Returns
/// Ok(None) when there is no `origin` remote configured. Used by
/// `cmd_pearls_init` to decide whether to bootstrap from a remote
/// (beads-model post-clone path) or initialize empty.
fn read_git_origin_url(repo_root: &std::path::Path) -> Result<Option<String>> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_root)
        .output()
        .context("exec git remote get-url origin")?;
    if !output.status.success() {
        // git prints "error: No such remote 'origin'" with exit 2 when
        // the remote isn't configured — that's a normal case, not an
        // error to bubble up.
        return Ok(None);
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() {
        Ok(None)
    } else {
        Ok(Some(url))
    }
}

/// True if `dolt_dir` resolves to the global `~/.smooth/dolt` store.
/// We treat the global store as single-machine: sessions, memories,
/// and personal pearls don't need cross-machine sync, so push/pull
/// without a configured remote is a no-op there rather than an error.
fn is_global_pearl_store(dolt_dir: &std::path::Path) -> bool {
    let Some(home) = dirs_next::home_dir() else { return false };
    let global = home.join(".smooth").join("dolt");
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(dolt_dir) == canon(&global)
}

/// Heuristic: dolt push/pull surfacing "no configured push destination"
/// (or the equivalent for pull) is what we want to swallow on the
/// global store. SQL/lock errors etc. should still propagate.
///
/// "No upstream" used to live here, but it's actually a recoverable
/// first-push case (auto-retry with `-u`), not a "no remote at all"
/// case — handled separately by [`is_no_upstream_error`].
fn is_no_remote_error(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    s.contains("no configured push destination") || s.contains("no configured pull destination") || s.contains("remote not found")
}

/// Heuristic: first push to a fresh remote without `-u` returns this.
/// The CLI auto-retries with `set_upstream = true`.
fn is_no_upstream_error(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    s.contains("no upstream branch") || s.contains("has no upstream")
}

/// Heuristic: the local store and remote `refs/dolt/data` share no
/// commit history. Typically because someone ran `dolt init` on the
/// remote independently of this machine. Recovery is force-push or
/// delete-the-ref; the CLI surfaces that as actionable text instead
/// of the bare Dolt error.
fn is_no_common_ancestor_error(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    s.contains("no common ancestor")
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
            let s = crate::tailscale::get_status();
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
            let registry = smooth_cast::provider_migration::load_providers_with_migration(&providers_path)?;

            println!("\n  {}\n", "Model Routing".cyan().bold());

            use smooth_operator::providers::Activity;
            let activities = [
                (Activity::Coding, "Coding", "code generation, edits, refactoring"),
                (Activity::Reasoning, "Reasoning", "deep reasoning, planning, chain-of-thought"),
                (Activity::Reviewing, "Reviewing", "code review, adversarial checks"),
                (Activity::Judge, "Judge", "evaluation, scoring, pass/fail"),
                (Activity::Summarize, "Summarize", "summaries, compression"),
                (Activity::Fast, "Fast", "session names, short titles, autocomplete"),
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

        RoutingCommands::Resolved => {
            if !providers_path.exists() {
                println!("  {} No providers configured. Run: th auth login", "✗".red().bold());
                return Ok(());
            }
            let registry = smooth_cast::provider_migration::load_providers_with_migration(&providers_path)?;

            println!("\n  {}\n", "Resolved Model Routing".cyan().bold());

            // Build the set of (provider, slot-alias) pairs we care about,
            // then fetch /model/info once per unique provider.
            use smooth_operator::providers::Activity;
            let activities = [
                (Activity::Coding, "Coding"),
                (Activity::Reasoning, "Reasoning"),
                (Activity::Reviewing, "Reviewing"),
                (Activity::Judge, "Judge"),
                (Activity::Summarize, "Summarize"),
                (Activity::Fast, "Fast"),
            ];

            // slot_for + default slot
            let mut slot_rows: Vec<(String, String, String)> = Vec::new(); // (label, provider, alias)
            for (activity, label) in &activities {
                let slot = registry.routing.slot_for(*activity);
                slot_rows.push(((*label).to_string(), slot.provider.clone(), slot.model.clone()));
            }
            slot_rows.push((
                "Default".to_string(),
                registry.routing.default.provider.clone(),
                registry.routing.default.model.clone(),
            ));

            // Unique providers we need to query.
            let mut providers_needed: Vec<String> = slot_rows.iter().map(|(_, p, _)| p.clone()).collect();
            providers_needed.sort();
            providers_needed.dedup();

            // Fetch per provider.
            let mut resolved: std::collections::HashMap<String, std::collections::BTreeMap<String, smooth_operator::resolution::ResolvedModel>> =
                std::collections::HashMap::new();
            let mut errors: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            for provider_id in &providers_needed {
                let Some(cfg) = registry.get_provider(provider_id) else {
                    errors.insert(provider_id.clone(), "provider not registered".into());
                    continue;
                };
                match smooth_operator::resolution::fetch_model_info(&cfg.api_url, &cfg.api_key).await {
                    Ok(map) => {
                        resolved.insert(provider_id.clone(), map);
                    }
                    Err(e) => {
                        errors.insert(provider_id.clone(), format!("{e}"));
                    }
                }
            }

            for (label, provider, alias) in &slot_rows {
                let upstream = resolved.get(provider).and_then(|m| m.get(alias)).and_then(|r| r.upstream.as_deref());
                match upstream {
                    Some(u) => {
                        println!("  {} {:<11} {} {} {}", "✓".green().bold(), label.bold(), alias.cyan(), "→".dimmed(), u.yellow());
                    }
                    None => {
                        let hint = errors
                            .get(provider)
                            .map(std::string::String::as_str)
                            .unwrap_or("gateway did not report an upstream for this alias");
                        println!(
                            "  {} {:<11} {} {} {}",
                            "?".yellow().bold(),
                            label.bold(),
                            alias.cyan(),
                            "→".dimmed(),
                            hint.dimmed()
                        );
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
                let registry = smooth_cast::provider_migration::load_providers_with_migration(&providers_path)?;
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

            let mut registry = smooth_cast::provider_migration::load_providers_with_migration(&providers_path)?;

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

            // `thinking` and `planning` are deprecated aliases that
            // map onto the merged `reasoning` slot — accepted for one
            // release for back-compat with old scripts and docs.
            match activity.as_str() {
                "coding" => registry.routing.coding = slot,
                "reasoning" | "thinking" | "planning" => registry.routing.reasoning = Some(slot),
                "reviewing" => registry.routing.reviewing = slot,
                "judge" => registry.routing.judge = slot,
                "summarize" => registry.routing.summarize = slot,
                "fast" => registry.routing.fast = Some(slot),
                "default" => registry.routing.default = slot,
                other => {
                    println!("Unknown activity: {other}");
                    println!("Available: coding, reasoning, reviewing, judge, summarize, fast, default");
                    return Ok(());
                }
            }

            registry.save_to_file(&providers_path)?;
            println!("  {} {} → {}", "✓".green().bold(), activity.bold(), model.cyan());
        }
    }

    Ok(())
}

// ── `th cast` — inspect the LLM cast ───────────────────────────────

/// `th cast models` — list live model groups from the configured
/// provider's `GET /v1/models` endpoint. Pearl th-2b5f63.
async fn cmd_cast(cmd: CastCommands) -> Result<()> {
    match cmd {
        CastCommands::Models { provider, json, filter } => {
            // `cmd_cast_models` uses `reqwest::blocking`, which panics
            // if dropped inside a tokio runtime context. Hop onto a
            // dedicated blocking thread to keep the runtime happy.
            tokio::task::spawn_blocking(move || cmd_cast_models(provider.as_deref(), json, filter.as_deref()))
                .await
                .context("cast models task panicked")?
        }
    }
}

/// Sniff out the LiteLLM-compatible `/v1/models` endpoint for a
/// `ProviderConfig`. Most provider URLs in the registry already end in
/// `/v1` (OpenAI-compatible), so we just append `/models`. If the URL
/// already ends in `/models` we leave it alone. Trailing slashes are
/// normalized so we don't produce `//models`.
fn models_url_for(api_url: &str) -> String {
    let trimmed = api_url.trim_end_matches('/');
    if trimmed.ends_with("/models") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/models")
    }
}

/// Strip ASCII control characters (0x00-0x1F) other than TAB / LF / CR
/// from `s`. LiteLLM occasionally returns responses with embedded
/// NULs / SOH bytes that break strict JSON parsers; tolerate them.
fn strip_control_chars(s: &str) -> String {
    s.chars().filter(|c| !matches!(*c as u32, 0..=8 | 11 | 12 | 14..=31)).collect()
}

/// Extract every `"id": "..."` substring from `body` as a fallback
/// when strict JSON parsing fails (e.g. truncated response). Returns
/// model ids in the order they appear, deduped. No regex crate — we
/// scan bytes for the `"id"` key followed by a string value.
fn extract_model_ids_lossy(body: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i + 4 < bytes.len() {
        // Look for `"id"` key — must be preceded by `{`, `,`, or whitespace,
        // and followed by optional whitespace, `:`, optional whitespace, `"`.
        if &bytes[i..i + 4] == b"\"id\"" {
            let mut j = i + 4;
            // skip whitespace
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'"' {
                    j += 1;
                    let start = j;
                    // read until closing `"` (no escape handling — model ids
                    // don't contain quotes in any provider we hit)
                    while j < bytes.len() && bytes[j] != b'"' {
                        if bytes[j] == b'\\' && j + 1 < bytes.len() {
                            j += 2;
                            continue;
                        }
                        j += 1;
                    }
                    // Only record if we actually saw a closing quote —
                    // an unterminated string at EOF means the response
                    // was truncated mid-value and we should NOT count it.
                    if j < bytes.len() && bytes[j] == b'"' && j > start {
                        if let Ok(id) = std::str::from_utf8(&bytes[start..j]) {
                            if !id.is_empty() && seen.insert(id.to_string()) {
                                ids.push(id.to_string());
                            }
                        }
                    }
                    i = j.saturating_add(1);
                    continue;
                }
            }
        }
        i += 1;
    }
    ids
}

/// Parse `"data": [{"id": ...}]` from a `/v1/models` response body.
/// Returns `(strict_ids, lossy_ids)`. `strict_ids` may be empty if the
/// body isn't valid JSON; `lossy_ids` is always best-effort from the
/// byte scan. Callers compare counts and surface a note if they differ.
fn parse_models_response(body: &str) -> (Vec<String>, Vec<String>) {
    let cleaned = strip_control_chars(body);
    let strict_ids: Vec<String> = serde_json::from_str::<serde_json::Value>(&cleaned)
        .ok()
        .as_ref()
        .and_then(|v| v.get("data"))
        .and_then(|d| d.as_array())
        .map(|arr| arr.iter().filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from)).collect())
        .unwrap_or_default();
    let lossy_ids = extract_model_ids_lossy(&cleaned);
    (strict_ids, lossy_ids)
}

/// Apply substring filter (case-insensitive) and sort alphabetically.
fn filter_and_sort(mut ids: Vec<String>, filter: Option<&str>) -> Vec<String> {
    if let Some(pat) = filter {
        let needle = pat.to_lowercase();
        ids.retain(|id| id.to_lowercase().contains(&needle));
    }
    ids.sort();
    ids.dedup();
    ids
}

#[allow(clippy::too_many_lines)]
fn cmd_cast_models(provider_override: Option<&str>, json_out: bool, filter: Option<&str>) -> Result<()> {
    let providers_path = dirs_next::home_dir()
        .map(|h| h.join(".smooth/providers.json"))
        .context("cannot determine home directory")?;

    if !providers_path.exists() {
        eprintln!("not authed \u{2014} run th auth login");
        std::process::exit(2);
    }

    let registry = smooth_cast::provider_migration::load_providers_with_migration(&providers_path)?;

    // Resolve provider id: explicit --provider wins, else the default
    // routing slot's provider, else the first registered provider.
    let provider_id = if let Some(p) = provider_override {
        p.to_string()
    } else {
        let default_id = registry.routing.default.provider.clone();
        if registry.get_provider(&default_id).is_some() {
            default_id
        } else if let Some(first) = registry.list_providers().first().map(|s| (*s).to_string()) {
            first
        } else {
            eprintln!("not authed \u{2014} run th auth login");
            std::process::exit(2);
        }
    };

    let Some(config) = registry.get_provider(&provider_id) else {
        eprintln!("provider '{provider_id}' not configured \u{2014} run th auth login");
        std::process::exit(2);
    };

    if config.api_key.is_empty() && provider_id != "ollama" {
        eprintln!("not authed \u{2014} run th auth login");
        std::process::exit(2);
    }

    let url = models_url_for(&config.api_url);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("building http client")?;

    let mut req = client.get(&url);
    if !config.api_key.is_empty() {
        req = req.bearer_auth(&config.api_key);
    }

    let resp = req.send().with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();

    if !status.is_success() {
        let snippet: String = body.chars().take(200).collect();
        eprintln!("GET {url} \u{2014} {status}");
        if !snippet.is_empty() {
            eprintln!("{snippet}");
        }
        std::process::exit(1);
    }

    let (strict_ids, lossy_ids) = parse_models_response(&body);

    // Prefer strict; fall back to lossy if strict came up empty.
    let chosen = if strict_ids.is_empty() { lossy_ids.clone() } else { strict_ids.clone() };
    let chosen = filter_and_sort(chosen, filter);

    if json_out {
        // Stable shape: `{"data": [{"id": "..."}]}`.
        let payload = serde_json::json!({
            "data": chosen.iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    // Colorized list output with the gradient wordmark header.
    println!();
    println!("  {} {}", gradient::smooth(), "cast \u{00b7} models".bold());
    println!("  {}", config.api_url.dimmed());
    println!();

    if chosen.is_empty() {
        println!("  {}", "no models returned".yellow());
    } else {
        for id in &chosen {
            println!("  {id}");
        }
    }

    println!();
    let display_url = config.api_url.trim_end_matches('/');
    println!("  {} models on {}", chosen.len().to_string().cyan().bold(), display_url.cyan());

    // Surface a discrepancy between strict + lossy counts — a sign the
    // response was truncated or malformed.
    if !strict_ids.is_empty() && lossy_ids.len() > strict_ids.len() {
        println!(
            "  {} strict-parsed {}, byte-scan found {} \u{2014} response may be truncated",
            "!".yellow().bold(),
            strict_ids.len(),
            lossy_ids.len()
        );
    } else if strict_ids.is_empty() && !lossy_ids.is_empty() {
        println!(
            "  {} strict JSON parse failed, fell back to byte scan ({} ids)",
            "!".yellow().bold(),
            lossy_ids.len()
        );
    }
    println!();

    Ok(())
}

#[cfg(test)]
mod cast_models_tests {
    use super::{extract_model_ids_lossy, filter_and_sort, models_url_for, parse_models_response, strip_control_chars};

    #[test]
    fn models_url_appends_models_when_missing() {
        assert_eq!(models_url_for("https://llm.smoo.ai/v1"), "https://llm.smoo.ai/v1/models");
        assert_eq!(models_url_for("https://llm.smoo.ai/v1/"), "https://llm.smoo.ai/v1/models");
    }

    #[test]
    fn models_url_leaves_already_models_alone() {
        assert_eq!(models_url_for("https://llm.smoo.ai/v1/models"), "https://llm.smoo.ai/v1/models");
        assert_eq!(models_url_for("https://llm.smoo.ai/v1/models/"), "https://llm.smoo.ai/v1/models");
    }

    #[test]
    fn strip_control_chars_removes_nuls_and_soh() {
        let s = "abc\x00def\x01ghi\njkl\t";
        let cleaned = strip_control_chars(s);
        // 0x00 and 0x01 stripped; \n and \t preserved.
        assert_eq!(cleaned, "abcdefghi\njkl\t");
    }

    #[test]
    fn extract_model_ids_lossy_picks_up_ids_in_truncated_json() {
        let body = r#"{"data":[{"id":"smooth-coding","object":"model"},{"id":"smooth-reasoning""#;
        let ids = extract_model_ids_lossy(body);
        assert_eq!(ids, vec!["smooth-coding".to_string(), "smooth-reasoning".to_string()]);
    }

    #[test]
    fn extract_model_ids_lossy_dedupes() {
        let body = r#"[{"id":"a"},{"id":"a"},{"id":"b"}]"#;
        let ids = extract_model_ids_lossy(body);
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn parse_models_response_strict_matches_lossy_on_clean_json() {
        let body = r#"{"data":[{"id":"smooth-coding"},{"id":"smooth-judge"}]}"#;
        let (strict, lossy) = parse_models_response(body);
        assert_eq!(strict.len(), 2);
        assert_eq!(lossy.len(), 2);
        assert!(strict.contains(&"smooth-coding".to_string()));
        assert!(strict.contains(&"smooth-judge".to_string()));
    }

    #[test]
    fn parse_models_response_recovers_from_control_chars() {
        // Embed a 0x00 in the middle — strict parse should still
        // succeed because we strip control chars before parsing.
        let body = "{\"data\":[{\"id\":\"smooth-coding\"}\x00,{\"id\":\"smooth-judge\"}]}";
        let (strict, _) = parse_models_response(body);
        assert_eq!(strict.len(), 2);
    }

    #[test]
    fn parse_models_response_lossy_when_strict_fails() {
        // Truncated body — strict parse fails, byte scan recovers.
        let body = r#"{"data":[{"id":"smooth-coding"},{"id":"smooth-rea"#;
        let (strict, lossy) = parse_models_response(body);
        assert!(strict.is_empty());
        assert_eq!(lossy, vec!["smooth-coding".to_string()]);
    }

    #[test]
    fn filter_and_sort_orders_alphabetically() {
        let ids = vec!["zebra".to_string(), "apple".to_string(), "mango".to_string()];
        let out = filter_and_sort(ids, None);
        assert_eq!(out, vec!["apple", "mango", "zebra"]);
    }

    #[test]
    fn filter_and_sort_substring_case_insensitive() {
        let ids = vec!["smooth-coding".to_string(), "smooth-judge".to_string(), "claude-sonnet-4".to_string()];
        let out = filter_and_sort(ids, Some("SMOOTH"));
        assert_eq!(out, vec!["smooth-coding", "smooth-judge"]);
    }

    #[test]
    fn filter_and_sort_dedupes_after_sort() {
        let ids = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        let out = filter_and_sort(ids, None);
        assert_eq!(out, vec!["a", "b"]);
    }

    /// End-to-end against a hand-rolled HTTP server: GET /v1/models
    /// returns a known body, we hit it with the same blocking reqwest
    /// client used by the real command, then run the response through
    /// parse_models_response + filter_and_sort. Verifies the wire
    /// path, sort, filter, and JSON shape all line up.
    #[test]
    fn end_to_end_against_mock_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let url = format!("http://{addr}/v1/models");

        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf);
            let body = r#"{"data":[{"id":"smooth-judge"},{"id":"smooth-coding"},{"id":"claude-sonnet-4"}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).expect("write");
        });

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("client");
        let resp = client.get(&url).bearer_auth("test-key").send().expect("send");
        assert!(resp.status().is_success());
        let body = resp.text().expect("body");
        let (strict, _) = parse_models_response(&body);
        let sorted = filter_and_sort(strict, Some("smooth"));
        assert_eq!(sorted, vec!["smooth-coding", "smooth-judge"]);

        // JSON shape: `{"data":[{"id":...}]}`
        let json = serde_json::json!({
            "data": sorted.iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>(),
        });
        let out = serde_json::to_string(&json).expect("json");
        assert_eq!(out, r#"{"data":[{"id":"smooth-coding"},{"id":"smooth-judge"}]}"#);

        handle.join().expect("server thread");
    }
}

#[allow(clippy::too_many_lines)]
fn cmd_mcp(cmd: McpCommands) -> Result<()> {
    use mcp_config::{expand_env, McpConfig, McpServerConfig};

    let global_path = McpConfig::default_path().context("cannot determine ~/.smooth/mcp.toml path")?;

    match cmd {
        McpCommands::Path { project } => {
            let p = if project { McpConfig::project_path()? } else { global_path };
            println!("{}", p.display());
            Ok(())
        }

        McpCommands::List => {
            let project_path = McpConfig::project_path().ok();
            let global = McpConfig::load(&global_path).unwrap_or_default();
            let project = project_path.as_ref().and_then(|p| McpConfig::load(p).ok()).unwrap_or_default();

            if global.servers.is_empty() && project.servers.is_empty() {
                println!("\n  {} No MCP servers configured.", "ℹ".cyan());
                println!("  {} {}\n", "Add one:".dimmed(), "th mcp add <name> <command> [args...]".cyan());
                return Ok(());
            }

            // Project overrides: a name present in project shadows
            // the global entry.
            let project_names: std::collections::HashSet<&str> = project.servers.iter().map(|s| s.name.as_str()).collect();

            println!("\n  {} {}\n", "MCP Servers".cyan().bold(), format!("({})", global_path.display()).dimmed());
            for s in &global.servers {
                let shadowed = project_names.contains(s.name.as_str());
                let marker = if shadowed {
                    "↑".yellow().bold().to_string()
                } else if s.disabled {
                    "○".dimmed().to_string()
                } else {
                    "✓".green().bold().to_string()
                };
                let cmdline = if s.args.is_empty() {
                    s.command.clone()
                } else {
                    format!("{} {}", s.command, s.args.join(" "))
                };
                let tag = if shadowed {
                    "[shadowed by project]".yellow().to_string()
                } else {
                    "[global]".dimmed().to_string()
                };
                println!("  {} {:<16} {}  {}", marker, s.name.bold(), cmdline.cyan(), tag);
                print_env(&s.env);
            }

            if !project.servers.is_empty() {
                if let Some(ref p) = project_path {
                    println!("\n  {} {}\n", "Project".cyan().bold(), format!("({})", p.display()).dimmed());
                }
                for s in &project.servers {
                    let marker = if s.disabled {
                        "○".dimmed().to_string()
                    } else {
                        "✓".green().bold().to_string()
                    };
                    let cmdline = if s.args.is_empty() {
                        s.command.clone()
                    } else {
                        format!("{} {}", s.command, s.args.join(" "))
                    };
                    println!("  {} {:<16} {}  {}", marker, s.name.bold(), cmdline.cyan(), "[project]".dimmed());
                    print_env(&s.env);
                }
            }
            println!();
            Ok(())
        }

        McpCommands::Add {
            name,
            command,
            args,
            env,
            disabled,
            project,
        } => {
            let path = if project { McpConfig::project_path()? } else { global_path };
            let mut cfg = McpConfig::load(&path)?;
            if cfg.find(&name).is_some() {
                anyhow::bail!(
                    "server `{name}` already exists in {}; remove it first with `th mcp remove {name}`",
                    path.display()
                );
            }
            let mut env_map = std::collections::HashMap::new();
            for entry in env {
                let (k, v) = entry
                    .split_once('=')
                    .with_context(|| format!("--env value `{entry}` must be in KEY=VALUE form"))?;
                env_map.insert(k.to_string(), v.to_string());
            }
            cfg.servers.push(McpServerConfig {
                name: name.clone(),
                command: command.clone(),
                args: args.clone(),
                env: env_map,
                disabled,
            });
            cfg.save(&path)?;
            let scope_label = if project { "project" } else { "global" };
            let cmdline = if args.is_empty() { command } else { format!("{command} {}", args.join(" ")) };
            println!(
                "\n  {} Added MCP server {} ({}) → {}\n",
                "✓".green().bold(),
                name.bold(),
                scope_label.dimmed(),
                cmdline.cyan()
            );
            Ok(())
        }

        McpCommands::Remove { name, project } => {
            // If --project is passed, only touch the project config.
            // Otherwise try project first (it's usually what the user
            // means for an in-repo entry), then global.
            let project_path = McpConfig::project_path().ok();

            let try_remove = |p: &std::path::Path| -> Result<bool> {
                let mut cfg = McpConfig::load(p)?;
                if cfg.remove(&name) {
                    cfg.save(p)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            };

            let removed_from = if project {
                let Some(pp) = project_path else {
                    anyhow::bail!("no project config found; run from a repo with `.smooth/` or `.git/`");
                };
                if try_remove(&pp)? {
                    Some(pp)
                } else {
                    None
                }
            } else {
                let mut hit: Option<std::path::PathBuf> = None;
                if let Some(pp) = &project_path {
                    if try_remove(pp)? {
                        hit = Some(pp.clone());
                    }
                }
                if hit.is_none() && try_remove(&global_path)? {
                    hit = Some(global_path.clone());
                }
                hit
            };

            match removed_from {
                Some(p) => {
                    println!(
                        "\n  {} Removed MCP server {} from {}\n",
                        "✓".green().bold(),
                        name.bold(),
                        p.display().to_string().dimmed()
                    );
                    Ok(())
                }
                None => anyhow::bail!("no MCP server named `{name}` in project or global config"),
            }
        }

        McpCommands::Test { name } => {
            // Look in both scopes; project wins.
            let project_path = McpConfig::project_path().ok();
            let project_cfg = project_path.as_ref().and_then(|p| McpConfig::load(p).ok()).unwrap_or_default();
            let global_cfg = McpConfig::load(&global_path).unwrap_or_default();
            let server = project_cfg
                .find(&name)
                .or_else(|| global_cfg.find(&name))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no MCP server named `{name}`"))?;

            println!("\n  {} Testing MCP server {}", "▶".cyan().bold(), name.bold());
            println!("  {} {} {}", "$".dimmed(), server.command.cyan(), server.args.join(" ").cyan());

            // Spawn the process. A healthy stdio MCP server stays alive
            // waiting for JSON-RPC on stdin; if it exits within 1s with
            // a non-zero status, treat that as a failure.
            let mut cmd = std::process::Command::new(&server.command);
            cmd.args(&server.args);
            for (k, v) in &server.env {
                cmd.env(k, expand_env(v));
            }
            cmd.stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    println!("  {} spawn failed: {e}", "✗".red().bold());
                    println!("  {} command not found on PATH? install it or use an absolute path.\n", "hint:".yellow());
                    return Err(anyhow::anyhow!("spawn failed"));
                }
            };

            // Give it a moment to crash if it's going to.
            std::thread::sleep(std::time::Duration::from_millis(1000));
            match child.try_wait() {
                Ok(None) => {
                    // Still running — that's healthy for an MCP stdio server.
                    let _ = child.kill();
                    let _ = child.wait();
                    println!(
                        "  {} Server starts cleanly. Runner will complete the MCP handshake on `th up`.\n",
                        "✓".green().bold()
                    );
                    Ok(())
                }
                Ok(Some(status)) => {
                    let mut stderr_out = String::new();
                    if let Some(mut stderr) = child.stderr.take() {
                        use std::io::Read;
                        let _ = stderr.read_to_string(&mut stderr_out);
                    }
                    println!("  {} Process exited early ({status})", "✗".red().bold());
                    if !stderr_out.trim().is_empty() {
                        println!("  {} stderr:\n{}", "↳".dimmed(), stderr_out.trim().red());
                    }
                    println!();
                    Err(anyhow::anyhow!("server exited early"))
                }
                Err(e) => {
                    let _ = child.kill();
                    Err(anyhow::anyhow!("wait failed: {e}"))
                }
            }
        }

        McpCommands::Defaults => {
            use mcp_config::{default_mcp_servers, host_probe_on_path, McpConfig};
            let global = McpConfig::load(&global_path).unwrap_or_default();
            println!("\n  {}\n", "Shipped MCP defaults".cyan().bold());
            for d in default_mcp_servers() {
                let installed = global.find(d.name).is_some();
                let probe_ok = host_probe_on_path(d.host_probe);
                let status = if installed {
                    "✓ registered".green().bold().to_string()
                } else {
                    "○ not registered".dimmed().to_string()
                };
                let probe = if probe_ok {
                    format!("{} on PATH", d.host_probe).green().to_string()
                } else {
                    format!("{} NOT on PATH", d.host_probe).yellow().to_string()
                };
                println!("  {}  {}  [{}]", d.name.bold(), status, probe);
                println!("    {} {}", "▸".dimmed(), d.description.dimmed());
                if !probe_ok {
                    println!("    {} install hint: {}", "↳".dimmed(), d.install_hint.cyan());
                }
                println!();
            }
            println!("  {} Add them all: {}\n", "→".dimmed(), "th mcp install".cyan());
            Ok(())
        }

        McpCommands::Install { name } => {
            use mcp_config::{default_mcp_servers, ensure_default_mcp_servers, host_probe_on_path, DefaultOutcome, McpConfig, McpServerConfig};
            // Targeted install: only one default by name. Implement as a
            // pre-filter on the shared `ensure_default_mcp_servers` helper.
            if let Some(ref n) = name {
                let Some(target) = default_mcp_servers().iter().find(|d| d.name == n) else {
                    anyhow::bail!("no shipped default named `{n}` — run `th mcp defaults` to see the list");
                };
                let mut cfg = McpConfig::load(&global_path).unwrap_or_default();
                if cfg.find(target.name).is_some() {
                    println!(
                        "\n  {} `{}` already registered (left as-is) → {}\n",
                        "ℹ".cyan(),
                        target.name.bold(),
                        global_path.display().to_string().dimmed()
                    );
                } else {
                    cfg.servers.push(McpServerConfig {
                        name: target.name.to_string(),
                        command: target.command.to_string(),
                        args: target.args.iter().map(|s| (*s).to_string()).collect(),
                        env: std::collections::HashMap::new(),
                        disabled: false,
                    });
                    cfg.save(&global_path)?;
                    println!(
                        "\n  {} Installed default MCP server {} → {}\n",
                        "✓".green().bold(),
                        target.name.bold(),
                        global_path.display().to_string().dimmed()
                    );
                }
                if !host_probe_on_path(target.host_probe) {
                    println!(
                        "  {} `{}` is not on PATH — install it to actually run the server:",
                        "!".yellow().bold(),
                        target.host_probe
                    );
                    println!("    {}\n", target.install_hint.cyan());
                }
                return Ok(());
            }

            // No name → install every missing default.
            let report = ensure_default_mcp_servers(&global_path)?;
            println!("\n  {} → {}\n", "Defaults".cyan().bold(), global_path.display().to_string().dimmed());
            for (name, outcome) in &report {
                let line = match outcome {
                    DefaultOutcome::Added => format!("  {} {} (added)", "✓".green().bold(), name.bold()),
                    DefaultOutcome::AlreadyPresent => format!("  {} {} (already present, left as-is)", "·".dimmed(), name.bold()),
                    DefaultOutcome::SkippedByUser => format!("  {} {} (skipped — user-disabled)", "○".dimmed(), name.bold()),
                };
                println!("{line}");
            }
            // Surface any missing host probes so the user knows what to install.
            let mut warned = false;
            for d in default_mcp_servers() {
                if !host_probe_on_path(d.host_probe) {
                    if !warned {
                        println!();
                        warned = true;
                    }
                    println!(
                        "  {} `{}` is not on PATH for `{}` — {}",
                        "!".yellow().bold(),
                        d.host_probe,
                        d.name.bold(),
                        d.install_hint.cyan()
                    );
                }
            }
            println!();
            Ok(())
        }
    }
}

fn plugins_dir() -> Result<std::path::PathBuf> {
    if let Ok(home) = std::env::var("SMOOTH_HOME") {
        return Ok(std::path::PathBuf::from(home).join("plugins"));
    }
    let h = dirs_next::home_dir().context("cannot determine home directory")?;
    Ok(h.join(".smooth").join("plugins"))
}

fn project_plugins_dir() -> Result<std::path::PathBuf> {
    let cwd = std::env::current_dir()?;
    let root = mcp_config::find_project_root(&cwd).unwrap_or(cwd);
    Ok(root.join(".smooth").join("plugins"))
}

fn list_plugins_in(dir: &std::path::Path) -> Vec<(String, String)> {
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        if !(entry.path().is_dir() && entry.path().join("plugin.toml").exists()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let summary = std::fs::read_to_string(entry.path().join("plugin.toml"))
            .ok()
            .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
            .and_then(|v| v.get("description").and_then(|d| d.as_str()).map(str::to_string))
            .unwrap_or_default();
        out.push((name, summary));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[allow(clippy::too_many_lines)]
fn cmd_plugin(cmd: PluginCommands) -> Result<()> {
    let global_dir = plugins_dir()?;

    match cmd {
        PluginCommands::Path { name, project } => {
            let dir = if project { project_plugins_dir()? } else { global_dir };
            match name {
                Some(n) => println!("{}", dir.join(&n).join("plugin.toml").display()),
                None => println!("{}", dir.display()),
            }
            Ok(())
        }

        PluginCommands::List => {
            let project_dir = project_plugins_dir().ok();
            let global_plugins = list_plugins_in(&global_dir);
            let project_plugins = project_dir.as_deref().map(list_plugins_in).unwrap_or_default();

            if global_plugins.is_empty() && project_plugins.is_empty() {
                println!("\n  {} No plugins installed.", "ℹ".cyan());
                println!("  {} {}\n", "Create one:".dimmed(), "th plugin init <name>".cyan());
                return Ok(());
            }

            let project_names: std::collections::HashSet<&str> = project_plugins.iter().map(|(n, _)| n.as_str()).collect();

            if !global_plugins.is_empty() {
                println!("\n  {} {}\n", "Plugins".cyan().bold(), format!("({})", global_dir.display()).dimmed());
                for (n, desc) in &global_plugins {
                    let shadowed = project_names.contains(n.as_str());
                    let (marker, tag) = if shadowed {
                        ("↑".yellow().bold().to_string(), "[shadowed by project]".yellow().to_string())
                    } else {
                        ("✓".green().bold().to_string(), "[global]".dimmed().to_string())
                    };
                    println!("  {} plugin.{:<14} {}  {}", marker, n.bold(), desc.dimmed(), tag);
                }
            }

            if !project_plugins.is_empty() {
                if let Some(ref pd) = project_dir {
                    println!("\n  {} {}\n", "Project".cyan().bold(), format!("({})", pd.display()).dimmed());
                }
                for (n, desc) in &project_plugins {
                    println!("  {} plugin.{:<14} {}  {}", "✓".green().bold(), n.bold(), desc.dimmed(), "[project]".dimmed());
                }
            }
            println!();
            Ok(())
        }

        PluginCommands::Init {
            name,
            command,
            description,
            project,
        } => {
            let base = if project { project_plugins_dir()? } else { global_dir };
            let plugin_dir = base.join(&name);
            let manifest_path = plugin_dir.join("plugin.toml");
            if manifest_path.exists() {
                anyhow::bail!("plugin `{name}` already exists at {}", manifest_path.display());
            }
            std::fs::create_dir_all(&plugin_dir)?;
            let cmd_str = command.unwrap_or_else(|| "echo {{message}}".to_string());
            let desc = description.unwrap_or_else(|| format!("Custom CLI tool `{name}`."));

            // Extract `{{name}}` placeholders from the command so the
            // generated schema matches it out of the box.
            let placeholders = extract_placeholders(&cmd_str);
            let required = placeholders.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ");
            let mut props = String::new();
            for n in &placeholders {
                props.push_str(&format!(
                    "\n[parameters.properties.{n}]\ntype = \"string\"\ndescription = \"TODO: describe `{n}` for the LLM.\"\n"
                ));
            }
            let template = format!(
                r#"name = "{name}"
description = "{desc}"

# Hint shown to the LLM about when to pick this tool. Optional.
prompt_hint = ""

# Shell command run via `bash -lc`. `{{{{param}}}}` placeholders are
# substituted with values from the agent's tool args.
command = "{cmd_str}"

# Per-call env vars. `${{env:VAR}}` references resolve from the runner's env.
[env]

# JSON Schema for the tool's parameters. Shown to the LLM verbatim.
[parameters]
type = "object"
required = [{required}]
{props}"#
            );
            std::fs::write(&manifest_path, template)?;
            println!(
                "\n  {} Created plugin {} at {}",
                "✓".green().bold(),
                name.bold(),
                manifest_path.display().to_string().dimmed()
            );
            println!("  {} Edit the manifest, then it'll be loaded next `th up`.\n", "→".dimmed());
            Ok(())
        }

        PluginCommands::Remove { name, project } => {
            // If --project, only look in project dir. Else try project
            // first, then global (matches cmd_mcp remove semantics).
            let project_dir = project_plugins_dir().ok();

            let attempt = |dir: &std::path::Path| -> Result<bool> {
                let plugin_dir = dir.join(&name);
                if !plugin_dir.is_dir() {
                    return Ok(false);
                }
                std::fs::remove_dir_all(&plugin_dir)?;
                Ok(true)
            };

            let removed_from = if project {
                let Some(pd) = project_dir else {
                    anyhow::bail!("no project plugins directory found; run from a repo with `.smooth/` or `.git/`");
                };
                attempt(&pd)?.then_some(pd)
            } else {
                let mut hit: Option<std::path::PathBuf> = None;
                if let Some(pd) = &project_dir {
                    if attempt(pd)? {
                        hit = Some(pd.clone());
                    }
                }
                if hit.is_none() && attempt(&global_dir)? {
                    hit = Some(global_dir.clone());
                }
                hit
            };

            match removed_from {
                Some(dir) => {
                    println!(
                        "\n  {} Removed plugin {} from {}\n",
                        "✓".green().bold(),
                        name.bold(),
                        dir.display().to_string().dimmed()
                    );
                    Ok(())
                }
                None => anyhow::bail!("no plugin named `{name}` in project or global directory"),
            }
        }
    }
}

/// Shared helper: print sorted env map entries under a table row.
fn print_env(env: &std::collections::HashMap<String, String>) {
    if env.is_empty() {
        return;
    }
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    for k in keys {
        println!("    {} {}={}", "env".dimmed(), k, env[k].dimmed());
    }
}

/// Extract `{{name}}` placeholders from a command template (deduplicated,
/// preserving first-seen order).
fn extract_placeholders(template: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(idx) = rest.find("{{") {
        let after = &rest[idx + 2..];
        if let Some(end) = after.find("}}") {
            let name = after[..end].trim().to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
            rest = &after[end + 2..];
        } else {
            break;
        }
    }
    out
}

/// Print a markdown context block for Claude Code SessionStart /
/// PreCompact hooks. Mirrors what `bd prime` did for beads.
///
/// Output = the embedded workflow primer + a live "Ready to work"
/// section populated from `th pearls ready`. If pearls isn't available
/// (first run in a repo, Dolt not initialized, etc.), the live section
/// is silently omitted — the static primer alone still gives Claude
/// enough to operate.
fn cmd_prime() -> Result<()> {
    // Static rules primer.
    print!("{}", include_str!("../prompts/prime.md"));

    // Live snapshot — best effort. Use the current `th` executable so
    // we stay consistent even when multiple `th` copies are on PATH.
    let exe = std::env::current_exe().ok();
    if let Some(exe) = exe {
        let output = std::process::Command::new(&exe)
            .args(["pearls", "ready"])
            .env("NO_COLOR", "1")
            .env("CLICOLOR", "0")
            .output();
        if let Ok(out) = output {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    println!("\n## Ready to work\n");
                    println!("```");
                    // Cap to ~40 lines so we don't bloat the hook output.
                    for (i, line) in trimmed.lines().enumerate() {
                        if i >= 40 {
                            println!("... (truncated; run `th pearls ready` for the full list)");
                            break;
                        }
                        println!("{line}");
                    }
                    println!("```");
                }
            }
        }
    }

    Ok(())
}


/// `th skills` — list / show skills discovered from every source.
/// Pearl th-e0f812. Walks the project's `.smooth/skills/` first,
/// then the user-level Smooth / Claude Code / opencode skill dirs.
fn cmd_skills(cmd: SkillsCommands) -> Result<()> {
    use owo_colors::OwoColorize;
    use smooth_cast::skills::{discover, discover_with_overrides, Skill, SkillSource};

    let workspace = std::env::current_dir().context("current directory")?;

    fn source_label(src: &SkillSource) -> &'static str {
        match src {
            SkillSource::Project => "project",
            SkillSource::UserSmooth => "user-smooth",
            SkillSource::ClaudeCode => "claude-code",
            SkillSource::OpenCode => "opencode",
            SkillSource::Builtin => "builtin",
        }
    }

    match cmd {
        SkillsCommands::List => {
            let visible = discover(&workspace);
            let all = discover_with_overrides(&workspace);
            let mut overridden: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            for s in &all {
                *overridden.entry(s.name.as_str()).or_default() += 1;
            }
            if visible.is_empty() {
                println!(
                    "  {} {}",
                    "ℹ".cyan(),
                    "No skills discovered. Add one at .smooth/skills/<name>/SKILL.md or ~/.smooth/skills/<name>/SKILL.md".dimmed()
                );
                return Ok(());
            }
            println!("\n  {}", "Skills".cyan().bold());
            for skill in &visible {
                let count = overridden.get(skill.name.as_str()).copied().unwrap_or(0);
                let suffix = if count > 1 {
                    format!(" {}", format!("(overrides {} other source(s))", count - 1).dimmed())
                } else {
                    String::new()
                };
                let scope_label = match skill.scope {
                    smooth_cast::skills::SkillScope::Sandbox => "sandbox".green().to_string(),
                    smooth_cast::skills::SkillScope::Host => "host".yellow().to_string(),
                };
                println!(
                    "  {} {:<28} {:>12}  {}{}",
                    "•".dimmed(),
                    skill.name.bold(),
                    format!("[{}]", source_label(&skill.source)).dimmed(),
                    skill.description,
                    suffix,
                );
                println!("    {:<28} {} {}", "", "scope:".dimmed(), scope_label);
                if !skill.allowed_hosts.is_empty() {
                    println!("    {:<28} {} {}", "", "hosts:".dimmed(), skill.allowed_hosts.join(", "));
                }
            }
            println!();
            Ok(())
        }
        SkillsCommands::Show { name } => {
            let all: Vec<Skill> = discover_with_overrides(&workspace).into_iter().filter(|s| s.name == name).collect();
            if all.is_empty() {
                anyhow::bail!("no skill named {name:?} found in any source");
            }
            for (i, skill) in all.iter().enumerate() {
                if i > 0 {
                    println!("\n{}\n", "─".repeat(64).dimmed());
                    println!(
                        "  {} {} {}",
                        "↳".dimmed(),
                        "shadowed by higher-precedence source".dimmed(),
                        format!("[{}]", source_label(&skill.source)).dimmed()
                    );
                }
                println!("\n  {}  {}", "name:".dimmed(), skill.name.bold());
                println!(
                    "  {}  {}",
                    "source:".dimmed(),
                    format!("[{}] {}", source_label(&skill.source), skill.path.display()).dimmed()
                );
                println!(
                    "  {}  {}",
                    "scope:".dimmed(),
                    match skill.scope {
                        smooth_cast::skills::SkillScope::Sandbox => "sandbox",
                        smooth_cast::skills::SkillScope::Host => "host",
                    }
                );
                println!("  {}  {}", "description:".dimmed(), skill.description);
                if !skill.triggers.is_empty() {
                    println!("  {}  {}", "triggers:".dimmed(), skill.triggers.join(", "));
                }
                if !skill.allowed_hosts.is_empty() {
                    println!("  {}  {}", "allowed_hosts:".dimmed(), skill.allowed_hosts.join(", "));
                }
                if !skill.allowed_tools.is_empty() {
                    println!("  {}  {}", "allowed_tools:".dimmed(), skill.allowed_tools.join(", "));
                }
                println!("\n{}\n", "─".repeat(64).dimmed());
                println!("{}", skill.body);
            }
            Ok(())
        }
    }
}


fn cmd_service(cmd: ServiceCommands) -> Result<()> {
    match cmd {
        ServiceCommands::Install { system, daemon } => service::install(system, daemon),
        ServiceCommands::Uninstall => service::uninstall(),
        ServiceCommands::Start => service::start(),
        ServiceCommands::Stop => service::stop(),
        ServiceCommands::Restart => service::restart(),
        ServiceCommands::Status => service::status(),
        ServiceCommands::Logs { follow } => service::logs(follow),
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::extract_placeholders;

    #[test]
    fn extract_placeholders_dedups_and_orders() {
        assert_eq!(extract_placeholders("echo {{a}} {{b}} {{a}}"), vec!["a", "b"]);
        assert_eq!(extract_placeholders("plain"), Vec::<String>::new());
        assert_eq!(extract_placeholders("{{ a }}-{{b}}"), vec!["a", "b"]);
        assert_eq!(extract_placeholders("dangle {{ unterminated"), Vec::<String>::new());
    }
}


#[cfg(test)]
mod worktree_guard_tests {
    use super::is_linked_worktree;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git launches")
            .status
            .success();
        assert!(ok, "git {args:?} failed in {dir:?}");
    }

    /// SMOODEV-1836: the primary worktree must NOT be treated as linked
    /// (so pearl auto-commit keeps working there), while a worktree created
    /// by `git worktree add` MUST be (so it's skipped).
    #[test]
    fn distinguishes_primary_from_linked_worktree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let primary = tmp.path().join("primary");
        std::fs::create_dir(&primary).unwrap();

        git(&primary, &["init", "-q", "-b", "main"]);
        git(&primary, &["config", "user.email", "t@t.test"]);
        git(&primary, &["config", "user.name", "Test"]);
        std::fs::write(primary.join("f.txt"), "x").unwrap();
        git(&primary, &["add", "."]);
        git(&primary, &["commit", "-q", "-m", "init"]);

        // Primary worktree: not linked.
        assert!(!is_linked_worktree(&primary), "primary worktree should not be detected as linked");

        // Linked worktree via `git worktree add`.
        let linked = tmp.path().join("linked");
        git(&primary, &["worktree", "add", "-q", linked.to_str().unwrap(), "-b", "feat"]);
        assert!(is_linked_worktree(&linked), "git-worktree-add tree should be detected as linked");
    }

    /// A non-git directory must fail toward `false` (preserve existing
    /// behaviour rather than silently dropping a commit).
    #[test]
    fn non_git_dir_is_not_linked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!is_linked_worktree(tmp.path()));
    }
}

#[cfg(test)]
mod beads_model_tests {
    //! Pearl th-975dfe: `.smooth/dolt/` is gitignored under the beads
    //! model; `cmd_pearls_init` ensures the entry exists and (on fresh
    //! clones) bootstraps from `refs/dolt/data` via the git origin URL.

    use super::{ensure_dolt_gitignored, read_git_origin_url};
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git launches")
            .status
            .success();
        assert!(ok, "git {args:?} failed in {dir:?}");
    }

    #[test]
    fn ensure_dolt_gitignored_creates_file_when_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let added = ensure_dolt_gitignored(tmp.path()).expect("ensure ok");
        assert!(added, "should report change when file did not exist");
        let contents = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(contents.contains(".smooth/dolt/"), "missing entry: {contents}");
    }

    #[test]
    fn ensure_dolt_gitignored_appends_when_unrelated_entries_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(".gitignore"), "target/\nnode_modules/\n").unwrap();
        let added = ensure_dolt_gitignored(tmp.path()).expect("ensure ok");
        assert!(added);
        let contents = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(contents.contains("target/"));
        assert!(contents.contains("node_modules/"));
        assert!(contents.contains(".smooth/dolt/"));
    }

    #[test]
    fn ensure_dolt_gitignored_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(".gitignore"), "foo/\n.smooth/dolt/\nbar/\n").unwrap();
        let added = ensure_dolt_gitignored(tmp.path()).expect("ensure ok");
        assert!(!added, "should report no change when entry already present");
        let contents = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        // Count exactly one occurrence of `.smooth/dolt` — no duplicates.
        let occurrences = contents.matches(".smooth/dolt").count();
        assert_eq!(occurrences, 1, "got {occurrences} occurrences: {contents}");
    }

    #[test]
    fn ensure_dolt_gitignored_recognizes_wildcard_variant() {
        // smooai uses `.smooth/dolt/**/.dolt/noms/manifest` style entries;
        // a more permissive variant like `.smooth/dolt/**` should also
        // count as "already ignored" so init doesn't add a duplicate.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(".gitignore"), ".smooth/dolt/**\n").unwrap();
        let added = ensure_dolt_gitignored(tmp.path()).expect("ensure ok");
        assert!(!added);
    }

    #[test]
    fn ensure_dolt_gitignored_recognizes_leading_slash_variant() {
        // `/.smooth/dolt/` (anchored) — same semantic as ours.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(".gitignore"), "/.smooth/dolt/\n").unwrap();
        let added = ensure_dolt_gitignored(tmp.path()).expect("ensure ok");
        assert!(!added);
    }

    #[test]
    fn read_git_origin_url_returns_none_when_no_origin() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        assert!(read_git_origin_url(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn read_git_origin_url_returns_origin_when_configured() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        git(tmp.path(), &["remote", "add", "origin", "https://example.com/team/repo.git"]);
        assert_eq!(read_git_origin_url(tmp.path()).unwrap().as_deref(), Some("https://example.com/team/repo.git"));
    }

    #[test]
    fn read_git_origin_url_non_git_dir_returns_none() {
        // Outside a git repo `git remote get-url` exits non-zero; the
        // helper must swallow that as "no origin" rather than bubbling
        // up — caller treats None as "no remote to bootstrap from."
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(read_git_origin_url(tmp.path()).unwrap().is_none());
    }
}

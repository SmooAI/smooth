//! `th` — Smoo AI CLI entry point.
//!
//! Single binary for agent orchestration, config management, and platform tools.

mod active_org;
#[cfg(feature = "admin")]
mod admin;
/// `th attest` — run a repo's CI checks here (or on a build box) and credit them
/// on GitHub so the workflow can skip what already ran. Pearl th-b27ed0.
mod attest;
mod auth;
mod boot_ui;
/// macOS calendar setup driven by `th doctor --setup-calendar` (pearl th-94cc4a).
#[cfg(target_os = "macos")]
mod calendar_setup;
mod claude;
mod config;
mod daemon_health;
mod daemon_launcher;
mod ext;
mod fda;
mod gradient;
mod hooks;
/// macOS Messages setup driven by `th doctor --setup-imessage` (pearl th-1665ed).
#[cfg(target_os = "macos")]
mod imessage_setup;
use smooth_tools::mcp_config;
/// th-374f85: `th agent` / `th msg` / `th inbox` on the machine-level
/// SQLite mail store (ADR-010), off the per-repo Dolt pearl store.
mod mail;
mod mail_backend;
mod mcp_install;
mod mcp_serve;
mod operator_serve;
/// Reclaimable-disk findings reported by `th doctor` (pearl th-91de11).
mod reclaim;
/// macOS Reminders setup driven by `th doctor --setup-reminders` (pearl th-94cc4a).
#[cfg(target_os = "macos")]
mod reminders_setup;
mod service;
mod smooai;
mod statusline_setup;

use smooai::cmd_orgs;

use anstream::{eprintln, print, println};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
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

    /// Resume a saved session.
    ///
    /// With no value, picks the most recently updated one. With a value,
    /// matches by id prefix or title substring. Pair with `--list` to
    /// inspect saved sessions first. Only takes effect when no subcommand
    /// is given (top-level `th` launches the TUI). Same as
    /// `th code --resume`. Pearl th-resume-top-level (2026-05-12).
    #[arg(long, value_name = "QUERY", num_args = 0..=1, default_missing_value = "")]
    resume: Option<String>,

    /// List saved sessions and exit.
    ///
    /// Only takes effect when no subcommand is given. Same as
    /// `th code --list`.
    #[arg(long)]
    list: bool,

    /// Pin the lead role for this session.
    ///
    /// One of fixer / oracle / mapper / scout / heckler. Same as
    /// `th code --agent <name>`.
    #[arg(long, value_name = "NAME")]
    agent: Option<String>,

    /// Auth profile to use for this command.
    ///
    /// Overrides the active profile and `SMOOAI_PROFILE`. Profiles bundle a
    /// user + M2M session under `~/.config/smooth/auth/profiles/<name>/`.
    /// See `th auth profile`.
    #[arg(long, global = true, value_name = "NAME")]
    profile: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run this repo's CI checks and credit the ones that pass, so the
    /// workflow can skip them.
    ///
    /// Run it INSTEAD of `git push`. Checks are whatever the repo defines as `scripts/ci/<name>.sh`; each
    /// passing one posts a `ci-attest/<name>` commit status on HEAD. A check
    /// that could not START posts nothing at all — that is not the same as a
    /// check that failed (pearl th-b27ed0).
    Attest(attest::AttestArgs),
    /// Run / control the chat-first Big Smooth daemon (epic th-c89c2a) on the
    /// smooth-operator LocalServer engine.
    ///
    /// Thin passthrough to the standalone `smooth-daemon` binary — `th daemon --help`
    /// shows its full CLI (`run` foreground / `operator` / `status` / `audit` /
    /// `schedule`).
    Daemon {
        /// Args forwarded verbatim to the `smooth-daemon` binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// smooth-operator engine dev tools — dogfood each polyglot LocalServer.
    Operator {
        #[command(subcommand)]
        cmd: OperatorCommands,
    },
    /// Start Smooth platform — boots Big Smooth on the host and runs dispatched tasks
    /// in-process.
    ///
    /// (The microVM sandbox mode was removed 2026-07, pearl th-f4a801; see git
    /// history.)
    Up {
        /// Skip starting Big Smooth (API + web UI)
        #[arg(long)]
        no_leader: bool,
        /// Big Smooth API port
        #[arg(long, default_value = "4400")]
        port: u16,
        /// Interface to bind Big Smooth on. Defaults to `127.0.0.1`
        /// (loopback only) — any other value opens the API + dashboard
        /// to that interface. The API has no authentication today, so
        /// `0.0.0.0` exposes every route (dispatch agents, mint creds,
        /// read pearls/sessions) to anyone on the network. Pearl
        /// `th-6db839`.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Run in foreground (default: daemonize).
        #[arg(long)]
        foreground: bool,
        /// Max concurrent Smooth operatives. Defaults to 3. Can also be
        /// set via SMOOTH_SANDBOX_MAX_CONCURRENCY.
        #[arg(long)]
        max_operators: Option<usize>,
        /// Skip the workflow's post-implementation TEST phase
        /// (adversarial test augmentation). Benchmark runs want
        /// this so added tests don't change the score. Equivalent
        /// to `SMOOTH_WORKFLOW_SKIP_TEST=1`.
        #[arg(long)]
        skip_test: bool,
    },
    /// Stop Smooth platform
    Down,
    /// Show system health
    Status,
    /// LLM provider credential management (Anthropic, Smoo AI Gateway, OpenRouter,
    /// OpenAI, …).
    ///
    /// Edits `~/.smooth/providers.json`.
    ///
    /// Was `th auth` before 2026-05 — that name now belongs to Smoo
    /// AI identity (`th auth login` for user/email-password, `th auth
    /// login --m2m` for service accounts). LLM-provider config moved
    /// here so the two concerns don't share a verb.
    Model {
        #[command(subcommand)]
        cmd: ModelCommands,
    },
    /// Smoo AI identity — log in to the Smoo AI platform.
    ///
    /// As a user (email + password) or a service account (M2M
    /// client_credentials). Used by `th admin *`, `th api *`, and (soon) llm.smoo.ai's user-attributed LLM
    /// session exchange.
    Auth {
        #[command(subcommand)]
        cmd: auth::AuthCommands,
    },
    /// Smoo AI superadmin operations against the /admin/* endpoints on api.smoo.ai.
    ///
    /// Requires a `th auth login` user session whose account has the
    /// requireSuperAdmin role (403 otherwise).
    #[cfg(feature = "admin")]
    Admin {
        #[command(subcommand)]
        cmd: admin::AdminCommands,
    },
    /// Smoo AI platform API — REST-style verbs backed by `api.smoo.ai`.
    ///
    /// Login + orgs + agents + keys + members + knowledge + jobs + products +
    /// profile + testing live under here. Config has its own top-level
    /// subcommand (`th config`) for the daily surface, `th admin config` for
    /// the platform-admin surface.
    Api {
        #[command(subcommand)]
        cmd: ApiCommands,
    },
    /// Smoo AI organizations — `list`, `switch` the active org, or `show` one.
    ///
    /// `switch` persists across all credential stores.
    ///
    /// Top-level alias for `th api orgs`, promoted for discoverability alongside
    /// `th config` / `th testing`.
    ///
    /// Note: `switch` flips the *active org* that user-JWT commands
    /// default to. The user JWT can act cross-org (a master admin is
    /// authorized over child orgs) — pass `--org`/`--org-id` per call,
    /// or `switch` to change the default. M2M tokens are org-locked
    /// server-side, so `switch` is cosmetic for the `--m2m` surface.
    #[command(visible_alias = "orgs")]
    Org {
        #[command(subcommand)]
        cmd: OrgsCommands,
    },
    /// Smoo AI `@smooai/config` — the daily-developer config surface.
    ///
    /// `get` / `set` / `list` for single values; `feature-flag` to evaluate a flag;
    /// `push` / `pull` / `diff` to sync the `.smooai-config/schema.json` document
    /// with the org's remote schema; `init` to scaffold a fresh local schema package;
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
    /// Scaffold + verify SmooAI dashboard widgets.
    ///
    /// `th widgets new` scaffolds all 5 touchpoints across the smooai monorepo,
    /// `th widgets list` enumerates the registry, `th widgets check` is the
    /// TS↔Rust↔renderer parity gate, `th widgets preview` (best-effort) scaffolds a
    /// temp render route + screenshot command.
    #[command(visible_alias = "widget")]
    Widgets {
        #[command(subcommand)]
        cmd: smooai::widgets::Cmd,
    },
    /// Smoo AI main-dashboard widget layout — get / add / remove widgets.
    ///
    /// Acts on your `/apps` dashboard (per user + org; widget ids from
    /// `th widgets list`). First-class front door for `th api dashboard`.
    Dashboard {
        #[command(subcommand)]
        cmd: smooai::dashboard::Cmd,
    },
    /// Smoo AI in-house web crawler (ADR-035) — a page as clean markdown.
    ///
    /// `th crawl scrape <url>` turns a page into clean markdown through the
    /// authed crawler service (real browser UA + JS render), so it gets pages
    /// a plain fetch 403s on. Any authenticated org member can use it.
    Crawl {
        #[command(subcommand)]
        cmd: smooai::crawl::Cmd,
    },
    /// Edit an org's RBAC roles (SMOODEV-2368 / ADR-105).
    ///
    /// `th roles list`, `show`, `create` (optionally `--template`),
    /// `grant`/`revoke`/`set-permissions` on a role's permission keys, and
    /// `assign`/`unassign` roles to members. System roles are immutable.
    /// User-authed (`th auth login`).
    #[command(visible_alias = "role")]
    Roles {
        #[command(subcommand)]
        cmd: smooai::roles::Cmd,
    },
    /// Smoo AI agentic web search (ADR-088) — `th search <query>` for ranked results.
    ///
    /// Optionally `--answer`. Served by our own search stack (self-hosted
    /// SearXNG + in-house crawler + LLM answer synthesis). Full options when logged in; an anonymous free tier (basic depth, capped
    /// results) otherwise. A companion to `th crawl` for agentic coding.
    Search {
        #[command(flatten)]
        args: smooai::websearch::SearchArgs,
    },
    /// Deprecated alias for `th search` — kept for the form shipped in v0.18.0
    /// (`th web-search search <query>`).
    ///
    /// Use `th search` instead.
    #[command(name = "web-search", hide = true)]
    WebSearch {
        #[command(subcommand)]
        cmd: smooai::websearch::Cmd,
    },
    /// Smoo AI LLM gateway keys — mint / rotate / list the org's `llm.smoo.ai` keys
    /// and inspect spend.
    ///
    /// `th llm create-key` provisions the org's persistent key (a LiteLLM virtual key
    /// scoped to the org budget) and prints it once.
    ///
    /// Authenticates as the user (Supabase JWT) and is org-admin-gated
    /// — a master admin can mint for a child org with `--org-id`.
    /// Wraps `api.smoo.ai/organizations/{org_id}/llm-gateway/*`.
    Llm {
        #[command(subcommand)]
        cmd: smooai::llm_gateway::Cmd,
    },
    /// Ping the human on their own phone.
    ///
    /// Designed to be called BY an agent (Big Smooth / claude-driver) as a
    /// notify-the-human primitive — "blocked, need input", "done", "approve this" —
    /// it sends a PUSH + in-app notification to the logged-in user's own devices via
    /// `api.smoo.ai`. The message is the positional words joined with spaces, so
    /// `th notify done, review the PR` works unquoted.
    Notify {
        /// The message body — the positional words, joined with spaces.
        #[arg(value_name = "MESSAGE", required = true)]
        message: Vec<String>,
        /// Notification title (what shows as the heading).
        #[arg(long, default_value = "Smoo AI")]
        title: String,
        /// Urgency: low, medium (default), high, or critical.
        #[arg(long, value_enum, default_value_t = smooai::notify::Priority::Medium)]
        priority: smooai::notify::Priority,
        /// Optional deep link to open when the notification is tapped.
        #[arg(long, value_name = "DEEPLINK")]
        url: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then
        /// the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Smoo AI testing platform — report test results and manage runs.
    ///
    /// The daily-developer surface for runs / cases / environments /
    /// deployments. `runs report <file>` is the high-level entry point: it creates a run and
    /// submits a CTRF (or, with `--junit`, a converted JUnit) report in one call.
    /// Same commands as `th api testing`, promoted to the top level alongside
    /// `th config`.
    Testing {
        #[command(subcommand)]
        cmd: smooai::testing::Cmd,
    },
    /// Smoo AI referrals — the org's partner / advocate program.
    ///
    /// `show` / `create` / `update` the program economics, `partners` to manage who
    /// gets paid, `link` for a partner's shareable `api.smoo.ai/r/<code>` URL, plus
    /// `attributions` / `visits` / `commissions`. Same commands as
    /// `th api referrals`, promoted to the top level alongside `th config`.
    #[command(visible_alias = "referral")]
    Referrals {
        #[command(subcommand)]
        cmd: smooai::referrals::Cmd,
    },
    /// Smoo AI booking — the org's Google-Calendar booking page.
    ///
    /// `config get/set` availability + link handle, `types` for named event types,
    /// `slots` to see open times, `bookings` to list them, `block add/list/rm` for
    /// manual busy time, `link` for the public URL. Same commands as
    /// `th api booking`, promoted to the top level alongside `th config`.
    #[command(visible_alias = "bookings")]
    Booking {
        #[command(subcommand)]
        cmd: smooai::booking::Cmd,
    },
    /// HeyPage — dogfood the AI website builder through the real product API.
    ///
    /// `build` (generate → create → publish → live URL), plus `generate` /
    /// `publish` / `get`.
    Heypage {
        #[command(subcommand)]
        cmd: smooai::heypage::Cmd,
    },
    /// Smoo AI org file system.
    ///
    /// `ls`, `mkdir`, `upload`, `download`, `mv`, `rm`, `lock`, `share`. Same
    /// commands as `th api files`, promoted to the top level alongside
    /// `th config`.
    #[command(visible_alias = "file")]
    Files {
        #[command(subcommand)]
        cmd: smooai::files::Cmd,
    },
    /// Smoo AI knowledge base — semantic retrieval over the org's own documents.
    ///
    /// `th knowledge search <query>` runs the SAME retrieval an agent does
    /// (scope to one doc with `--doc`), plus `list` / `show` / `content` /
    /// `upload` / `website` / `process` / `update` / `delete`. Same commands as `th api knowledge`, promoted to the top level as an
    /// agentic-coding primitive alongside `th search` (the web) and `th crawl` (a
    /// page).
    #[command(visible_alias = "kb")]
    Knowledge {
        #[command(subcommand)]
        cmd: smooai::knowledge::Cmd,
    },
    /// Smoo AI CRM — contacts, companies, deals, tasks, notes, and pipeline.
    ///
    /// Deals carry `--value` / `--mrr` / `--upfront` economics; also stages,
    /// pipeline forecast, and import. Same commands as `th api crm`, promoted to the top level alongside `th config`
    /// / `th testing`.
    Crm {
        #[command(subcommand)]
        cmd: smooai::crm::Cmd,
    },
    /// White-label a Smoo AI org — app name, chrome colors, and logos.
    ///
    /// Logos come from a local path or a remote URL, always re-hosted on our
    /// CDN. `from-url` derives a theme from the partner's website; `enable` is the live
    /// switch and refuses a theme that fails WCAG AA contrast. The Aurora meaning
    /// tokens (heat / ai / gradients) are never white-labeled.
    #[command(visible_alias = "brand")]
    Branding {
        #[command(subcommand)]
        cmd: smooai::branding::Cmd,
    },
    /// Run a pearl through a Smooth operative.
    ///
    /// Dispatches to Big Smooth (`th up` must be running) and streams agent
    /// events to stdout.
    Run {
        /// Pearl id, or a task description prefixed with a space
        /// (e.g. `th run "refactor x to y"`). If empty, picks the
        /// first ready pearl.
        pearl_id: Option<String>,
        /// Override the default model for this run
        #[arg(long)]
        model: Option<String>,
        /// Lead role to run under: `fixer` (default, full tools),
        /// `mapper` (read-only, decomposes), `oracle` (read-only, reasons),
        /// or `heckler` (read-only, critiques). Unknown names error
        /// out with the list above.
        #[arg(long)]
        agent: Option<String>,
    },
    /// Pause a running operative — its agent loop halts until `resume`.
    Pause {
        /// The operative id from `th operatives list`.
        #[arg(value_name = "OPERATIVE_ID")]
        bead_id: String,
    },
    /// Resume a paused operative.
    Resume {
        /// The operative id from `th operatives list`.
        #[arg(value_name = "OPERATIVE_ID")]
        bead_id: String,
    },
    /// Send mid-run guidance to a running operative.
    Steer {
        /// The operative id from `th operatives list`.
        #[arg(value_name = "OPERATIVE_ID")]
        bead_id: String,
        /// The guidance message to inject into the run.
        message: String,
    },
    /// Cancel a running operative (stops the run; see also `operatives kill`).
    Cancel {
        /// The operative id from `th operatives list`.
        #[arg(value_name = "OPERATIVE_ID")]
        bead_id: String,
    },
    /// Approve a pending operative review gate.
    Approve {
        /// The operative id from `th operatives list`.
        #[arg(value_name = "OPERATIVE_ID")]
        bead_id: String,
    },
    /// Show operative reviews + notifications needing your attention.
    ///
    /// Distinct from `th msg inbox`, which is agent-to-agent mail.
    Inbox,
    /// Smooth operative management
    #[command(visible_alias = "operative")]
    Operatives {
        #[command(subcommand)]
        cmd: Option<OperativesCommands>,
    },
    /// Pearl projects in the global registry (`~/.smooth/registry.json`) — `list`
    /// shows every tracked project.
    ///
    /// Register one by running `th pearls init` inside it. For per-pearl work use
    /// `th pearls`.
    #[command(visible_alias = "projects")]
    Project {
        #[command(subcommand)]
        cmd: ProjectCommands,
    },
    /// Local pearl Dolt database — `status` (health), `backup`, and
    /// on-disk `path` of this project's `.smooth/dolt/` store.
    Db {
        #[command(subcommand)]
        cmd: DbCommands,
    },
    /// Jira sync — `sync` pushes/pulls SMOODEV issues against pearls,
    /// `status` shows the current sync state.
    Jira {
        #[command(subcommand)]
        cmd: JiraCommands,
    },
    /// View audit logs
    Audit {
        #[command(subcommand)]
        cmd: AuditCommands,
    },
    /// Open the Smooth web dashboard in your browser.
    Web,
    /// Supervise Claude Code sessions running inside tmux.
    ///
    /// Launch, send a prompt, and keep the session alive until it exits or the
    /// account hits its usage limit. `run` / `ls` / `attach`.
    Claude {
        #[command(subcommand)]
        cmd: claude::ClaudeCommands,
    },
    /// Git worktree management
    Worktree {
        #[command(subcommand)]
        cmd: WorktreeCommands,
    },
    /// Tailscale status — show the tailnet devices Smooth can see.
    Tailscale {
        #[command(subcommand)]
        cmd: TailscaleCommands,
    },
    /// Operative access control
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
        /// How to resolve Safehouse Narc `Ask` verdicts that fire
        /// during an unattended (headless / bench) run. One of
        /// `deny` / `once` / `session` / `project` / `user`.
        /// Default `deny` — unattended runs are safe by default
        /// (asks turn into denials). Pearl th-400773.
        #[arg(long, value_name = "MODE", default_value = "deny")]
        auto_approve: String,
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
    /// Agent registry — make this session addressable by other agents.
    ///
    /// Registers it under a name so any harness (claude-code, opencode, pi, a
    /// shell loop) can message it. Machine-level: one roster per host
    /// (`~/.smooth/mail.db`).
    #[command(visible_alias = "agents")]
    Agent {
        #[command(subcommand)]
        cmd: mail::AgentCommands,
    },
    /// Agent messaging — send and receive messages between agents.
    ///
    /// `th msg send <name|all> <body...>` to send, `th msg inbox` to read,
    /// `th msg watch` to continuously poll. Harness-agnostic: any process that
    /// can run `th` can participate.
    #[command(visible_alias = "msgs")]
    Msg {
        #[command(subcommand)]
        cmd: mail::MsgCommands,
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
    #[command(visible_alias = "plugins")]
    Plugin {
        #[command(subcommand)]
        cmd: PluginCommands,
    },
    /// SEP extensions (subprocess tools/hooks/UI via the Smooth Extension Protocol).
    ///
    /// Install a local extension, list/trust/remove installed ones.
    Ext {
        #[command(subcommand)]
        cmd: ext::ExtCommands,
    },
    /// Run Smooth as a background service (launchd / systemd / Task Scheduler)
    Service {
        #[command(subcommand)]
        cmd: ServiceCommands,
    },
    /// Print the workflow-rules + current-state context block.
    ///
    /// For Claude Code SessionStart / PreCompact hooks; the `th` equivalent
    /// of `bd prime`.
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
        /// macOS only: guide the one-time Full Disk Access grant when the
        /// workspace is on an external volume. Opens the FDA settings pane and
        /// reveals `th` + the daemon binary in Finder to drag in. FDA can't be
        /// granted programmatically (SIP-protected TCC.db), so this is as
        /// automated as it gets. Run on the host's console, not over SSH.
        #[arg(long)]
        fix_fda: bool,
        /// macOS only: set up Big Smooth's calendar tool. Installs the `ical`
        /// CLI (side-loads the release binary to ~/.smooth/bin) and drives
        /// Big Smooth.app into asking macOS for Calendar access — the grant
        /// belongs to the app bundle, so a bare CLI can't request it. Run on the
        /// host's console, not over SSH: the prompt needs a GUI login session.
        #[arg(long)]
        setup_calendar: bool,
        /// macOS only: set up Big Smooth's Messages tool. Reports whether
        /// `~/Library/Messages/chat.db` is readable (Full Disk Access) and fires
        /// a harmless Apple Event at Messages.app so the one-time Automation
        /// prompt appears now instead of mid-turn. Neither grant can be given
        /// programmatically (SIP-protected TCC.db), so this detects and guides.
        /// Run on the host's console, not over SSH: the prompts need a GUI login
        /// session.
        #[arg(long)]
        setup_imessage: bool,
        /// macOS only: set up Big Smooth's reminders tool. Drives Big Smooth.app
        /// into asking macOS for Reminders access — a SEPARATE grant from
        /// Calendar, and one a bare CLI can't request (the grant belongs to the
        /// app bundle). Nothing to install: reminders go through EventKit
        /// in-process. Run on the host's console, not over SSH: the prompt needs
        /// a GUI login session.
        #[arg(long)]
        setup_reminders: bool,
        /// Set up the Claude Code statusline that shows this session's th-mail
        /// handle and unread count (`⚙ th:fix-auth ✉3`). Writes the `statusLine`
        /// entry into `~/.claude/settings.json` — and NEVER over an existing
        /// one: if you already run a statusline (ponytail's, say) it prints the
        /// wrapper that renders both, for you to opt into.
        #[arg(long)]
        setup_statusline: bool,
        /// Run the health check, then walk every setup step that isn't ready
        /// (LLM providers, Smoo AI sign-in, Full Disk Access, Calendar,
        /// Reminders, Messages) in order, driving each one's `--setup-*` path.
        /// The guided first-run flow — same steps Big Smooth.app's Set Up menu
        /// invokes. Run on the host's console, not over SSH: the macOS grants
        /// need a GUI login session.
        #[arg(long)]
        onboard: bool,
    },
    /// List skills available in the current workspace.
    ///
    /// Reads `.smooth/skills/`, `~/.smooth/skills/`, `~/.claude/skills/`, and
    /// `~/.opencode/skills/` — first hit wins on name. Pearl th-e0f812.
    #[command(visible_alias = "skill")]
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
    /// Bring-your-own LLM providers.
    ///
    /// Add an OpenAI-compatible server (Ollama, LM Studio, llama.cpp, …) by
    /// URL, list what's configured, remove one, or auto-detect a local server.
    ///
    /// Edits `~/.smooth/providers.json` field-preservingly: the typed
    /// loader drops any key it doesn't know (including per-provider
    /// `max_tokens`), so every write here goes through raw JSON to keep
    /// those intact. `th model login` remains the preset-keyed path for
    /// the cloud providers (Anthropic, OpenRouter, Smoo AI gateway, …).
    #[command(visible_alias = "provider")]
    Providers {
        #[command(subcommand)]
        cmd: ProvidersCommands,
    },
}

#[derive(Subcommand)]
enum ProvidersCommands {
    /// Add or update a provider by base URL. Re-running with the same
    /// id merges (only the flags you pass change; everything else on
    /// the entry is preserved). Adding the first provider to an empty
    /// file wires every routing slot to it.
    Add {
        /// Provider id (e.g. `ollama`, `lmstudio`, `mylocal`).
        id: String,
        /// OpenAI-compatible base URL, usually ending in `/v1`
        /// (e.g. `http://localhost:11434/v1`).
        #[arg(long)]
        url: String,
        /// API key. Local servers (Ollama, LM Studio) need none.
        #[arg(long)]
        api_key: Option<String>,
        /// Wire format: `openai` (default) or `anthropic`.
        #[arg(long)]
        format: Option<String>,
        /// Default model id for this provider (e.g. `llama3.3`).
        #[arg(long)]
        model: Option<String>,
        /// Per-provider output-token cap. Small local-model context
        /// windows are blown by the default 32768 — set this to fit.
        #[arg(long)]
        max_tokens: Option<u32>,
    },
    /// List configured providers with a local tag and any per-provider
    /// max_tokens.
    List {
        /// Emit JSON instead of the colorized list.
        #[arg(long)]
        json: bool,
    },
    /// Remove a provider by id.
    Remove { id: String },
    /// Probe common local inference ports (Ollama 11434, LM Studio
    /// 1234) via `GET /v1/models` and report what responds. Pass
    /// `--yes` to add every detected server automatically.
    Detect {
        /// Add detected servers to providers.json (default model = the
        /// first model each server reports).
        #[arg(long)]
        yes: bool,
        /// Emit JSON instead of the colorized report.
        #[arg(long)]
        json: bool,
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
enum OperativesCommands {
    /// List running operative VMs
    List,
    /// Tear down a running operative VM
    Kill { operator_id: String },
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
    /// Switch the active org, persisted across the credential stores.
    /// Subsequent commands default to it unless `--org-id` is passed.
    ///
    /// Omit the argument on a TTY to pick interactively from the orgs you
    /// belong to. A value is matched as a UUID first, then case-insensitively
    /// against org name / slug (substring) — so `th org switch ats`
    /// works without copying a UUID.
    Switch {
        /// Org id (UUID) or a name/slug substring. Omit to pick from a list.
        org_id: Option<String>,
    },
}

#[derive(Subcommand)]
enum ApiCommands {
    // `th api login` / `logout` / `whoami` were removed (pearl th-16b0ca).
    // Smoo AI identity lives under `th auth` — `th auth login [--m2m]`,
    // `th auth logout [--m2m|--all]`, `th auth whoami`. Two spellings for one
    // identity was actively confusing: `th auth` handles both the user browser
    // flow and M2M, and it is the surface that understands auth profiles.
    /// Smoo AI organization management.
    #[command(visible_alias = "org")]
    Orgs {
        #[command(subcommand)]
        cmd: OrgsCommands,
    },
    /// Smoo AI agents — list / show / create / update / delete + the
    /// regenerate-* and per-agent knowledge endpoints.
    #[command(visible_alias = "agent")]
    Agents {
        #[command(subcommand)]
        cmd: smooai::agents::Cmd,
    },
    /// Smoo AI auth clients ("API keys") — mint and manage both
    /// machine-to-machine (M2M, server/CI secret) and browser-to-machine
    /// (B2M, origin-restricted publishable key) clients: list, create
    /// (`--type m2m|b2m`, `--allowed-origin` for B2M), update a B2M
    /// client's origins, rotate, and revoke.
    ///
    /// These routes require a dashboard **user** session (`th auth
    /// login`) — they 403 under M2M. A master admin can target a child
    /// org with `--org-id`.
    #[command(visible_alias = "key")]
    Keys {
        #[command(subcommand)]
        cmd: smooai::keys::Cmd,
    },
    /// Smoo AI org members + invitations.
    #[command(visible_alias = "member")]
    Members {
        #[command(subcommand)]
        cmd: smooai::members::Cmd,
    },
    /// Smoo AI org teams — RBAC groupings of members that hold roles
    /// (list / create / rename / delete / set-members / set-roles).
    #[command(visible_alias = "team")]
    Teams {
        #[command(subcommand)]
        cmd: smooai::teams::Cmd,
    },
    /// Smoo AI CRM — the revenue engine: contacts, companies, deals,
    /// pipeline forecast, stages, tasks, conversations, timeline & invoices.
    /// Authenticates as the logged-in user (`th auth login`), so writes
    /// are attributed to a real person rather than an M2M client.
    Crm {
        #[command(subcommand)]
        cmd: smooai::crm::Cmd,
    },
    /// Smoo AI org Smooth Operator — chat / confirm / history. Drives the org's
    /// always-on dashboard agent (draft email, CRM, analytics, templates).
    /// Authenticates as the logged-in user (`th auth login`).
    #[command(name = "smooth-operator")]
    SmoothOperator {
        #[command(subcommand)]
        cmd: smooai::smooth_operator::Cmd,
    },
    /// Smoo AI knowledge documents (text, websites, files).
    Knowledge {
        #[command(subcommand)]
        cmd: smooai::knowledge::Cmd,
    },
    /// Smoo AI org file system — folders, files, presigned upload/download,
    /// deletion locks, and anonymous/tracked shares (ADR-060).
    #[command(visible_alias = "file")]
    Files {
        #[command(subcommand)]
        cmd: smooai::files::Cmd,
    },
    /// Smoo AI async job queue.
    #[command(visible_alias = "job")]
    Jobs {
        #[command(subcommand)]
        cmd: smooai::jobs::Cmd,
    },
    /// Smoo AI main-dashboard widget layout — get / add / remove widgets
    /// on your `/apps` dashboard (per user + org; widget ids from `th
    /// widgets list`).
    Dashboard {
        #[command(subcommand)]
        cmd: smooai::dashboard::Cmd,
    },
    /// Smoo AI org integrations (SendGrid email).
    #[command(visible_alias = "integration")]
    Integrations {
        #[command(subcommand)]
        cmd: smooai::integrations::Cmd,
    },
    /// Smoo AI billing products / plans.
    #[command(visible_alias = "product")]
    Products {
        #[command(subcommand)]
        cmd: smooai::products::Cmd,
    },
    /// Smoo AI referrals — partner program, partners, attributions,
    /// commissions.
    #[command(visible_alias = "referral")]
    Referrals {
        #[command(subcommand)]
        cmd: smooai::referrals::Cmd,
    },
    /// Smoo AI booking — Google-Calendar availability config + link
    /// handle, named booking types, open slots, bookings, manual busy
    /// blocks, and the public booking link.
    #[command(visible_alias = "bookings")]
    Booking {
        #[command(subcommand)]
        cmd: smooai::booking::Cmd,
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
        /// Plugin name (becomes the tool name as `plugin_<name>`)
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
    ///
    /// With `--harness`, runs the OTHER direction instead: registers
    /// `th mcp serve` (this binary) with a coding harness, so Claude Code,
    /// Codex and OpenCode all reach the same agent mailbox and pearl store.
    Install {
        /// Default name (`budget-aware-mcp`, …). Omit to install every default.
        name: Option<String>,
        /// Register `th mcp serve` with a coding harness instead:
        /// `claude-code` | `codex` | `opencode` | `all`.
        #[arg(long)]
        harness: Option<String>,
        /// Print what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run `th` ITSELF as an MCP server over stdio, exposing th's
    /// high-value surfaces (pearls, memory, …) as MCP tools so Claude
    /// Desktop / Cursor / Windsurf / VS Code can drive them. This is the
    /// inverse of the client commands above: they register OTHER servers
    /// for the operator; this turns `th` into a server other hosts consume.
    ///
    /// Speaks JSON-RPC on stdout — do not mix with other output. Point a
    /// host's `mcpServers` config at `th mcp serve`.
    Serve,
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
enum OperatorCommands {
    /// Boot one of the 5 polyglot smooth-operator LocalServer implementations
    /// behind a uniform env contract (pearl th-3f46fd). Dogfooding tool: the
    /// servers live in the sibling `smooth-operator` repo (override with
    /// `SMOOTH_OPERATOR_REPO`; default `~/dev/smooai/smooth-operator`). Every
    /// engine inherits `SMOOAI_GATEWAY_URL` / `SMOOAI_GATEWAY_KEY` /
    /// `SMOOTH_PERSONA` / `SMOOAI_MODEL` (default `deepseek-v4-flash`).
    ///
    /// Per-engine notes: `rust` runs `th daemon` (the only runnable Rust
    /// server; carries daemon narc/storage/persona extras). `python` bind is
    /// hardcoded 127.0.0.1:8787 upstream — `--port` is ignored. `ts` is
    /// auto-built (`pnpm install && pnpm build`) if `dist/main.js` is missing.
    Serve {
        /// Which LocalServer implementation to boot.
        #[arg(long)]
        lang: operator_serve::Lang,
        /// Port to bind (default 8799; ignored for `python`).
        #[arg(long)]
        port: Option<u16>,
    },
}

#[derive(Subcommand)]
enum ProjectCommands {
    /// Not implemented — run `th pearls init` inside the project instead.
    Create { name: String, description: Option<String> },
    /// List every project in `~/.smooth/registry.json`.
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
    /// Reconcile pearls ↔ Jira: close pearls whose Jira tickets are Done, and
    /// transition Jira tickets to Done once every referencing pearl is closed.
    /// Creating anything is opt-in via --pull / --push.
    Sync {
        /// Print the plan without changing anything
        #[arg(long)]
        dry_run: bool,
        /// Also create a local pearl for every open Jira ticket no pearl references
        #[arg(long)]
        pull: bool,
        /// Also create a Jira ticket for every active pearl without an issue key in its title
        #[arg(long)]
        push: bool,
    },
    /// Show Jira status
    Status,
}

#[derive(Subcommand)]
enum AuditCommands {
    /// Show recent audit log entries.
    ///
    /// Omit `actor` for the most recently written stream (usually
    /// `egress-proxy`, the goalie egress boundary).
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
    /// Approve a pending access request.
    ///
    /// `id` is the request id printed by `th access pending` (or surfaced
    /// in the SSE stream). `--scope` controls how long the approval
    /// sticks: `once` (this request only, default), `session` (VM
    /// lifetime), `project` (<repo>/.smooth/wonk-allow.toml), `user`
    /// (~/.smooth/wonk-allow.toml).
    Approve {
        /// Pending request id (UUID)
        id: String,
        /// Persistence scope (default: once)
        #[arg(long, default_value = "once")]
        scope: String,
        /// Optional glob to bind the approval to instead of the exact
        /// resource — e.g. `*.openai.com` for any openai.com subdomain.
        #[arg(long)]
        glob: Option<String>,
    },
    /// Deny a pending access request.
    Deny {
        /// Pending request id (UUID)
        id: String,
        /// Persistence scope (default: once)
        #[arg(long, default_value = "once")]
        scope: String,
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
    /// Schedule a pearl to "speak up" in the prime hook once its time arrives.
    /// WHEN is relative (`+2h`, `30m`, `2d`, `1w`, `tomorrow`, `now`) or
    /// absolute (`2026-07-10`, `2026-07-10 09:00`, RFC3339). Omit WHEN to clear.
    Schedule {
        id: String,
        // allow_hyphen_values so relative-past offsets like `-1h` aren't
        // mistaken for a flag.
        #[arg(allow_hyphen_values = true)]
        when: Option<String>,
    },
    /// Show scheduled pearls whose time has arrived (scheduled_at <= now)
    Due,
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
    Pull {
        /// Pull even if local `main` has commits not yet on the remote
        /// (which the pull could orphan). Without this, the pull refuses
        /// and tells you to `th pearls push` first.
        #[arg(short = 'f', long)]
        force: bool,
    },
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
    /// from the configured `origin` remote. A store that reads cleanly
    /// is NEVER re-cloned.
    ///
    /// Also reports which `smooth-dolt` processes hold this store, and
    /// whether the store actually accepts WRITES — a store pinned by a
    /// leaked one-shot reads perfectly while every write dies with
    /// `Error 1105: cannot update manifest: database is read only`.
    /// `--reap` (implied by `--auto-repair`) kills the leaked processes
    /// and re-probes.
    ///
    /// Then checks REMOTE SYNC health: clones the remote `refs/dolt/data`
    /// to a temp dir (bounded by SMOOTH_DOLT_SYNC_TIMEOUT_SECS, 30s
    /// default) and compares histories — in-sync / local-ahead (push) /
    /// remote-ahead (pull) / diverged with no common ancestor (incl. the
    /// stray "Initialize data repository" re-init that deadlocks push AND
    /// pull), plus whether the branch upstream is configured. Read-only:
    /// it recommends the fix, it never force-pushes.
    Doctor {
        /// Apply the remedy each finding calls for: reap leaked
        /// smooth-dolt processes when the store is write-locked, resolve
        /// a conflicted manifest, and — ONLY when the manifest doesn't
        /// read cleanly — snapshot + re-clone from `origin`. Without
        /// this flag, `doctor` just reports.
        #[arg(long)]
        auto_repair: bool,
        /// Kill the leaked `smooth-dolt` processes pinning this store
        /// into read-only, without touching the manifest. The targeted
        /// fix for `cannot update manifest: database is read only`.
        #[arg(long)]
        reap: bool,
        /// Reap even a live `smooth-dolt serve`, and one-shots younger
        /// than --reap-age-secs. Also allows repair when a server is
        /// attached (it's stopped first) — without it, doctor refuses to
        /// repair while a server is running, since in-memory state could
        /// differ from disk.
        #[arg(long)]
        force: bool,
        /// How long a one-shot `smooth-dolt` process must have been
        /// alive before it counts as leaked. Healthy one-shots live
        /// milliseconds; the bound only exists so a concurrently-running
        /// query from another `th` isn't killed mid-write.
        #[arg(long, default_value_t = smooth_pearls::dolt::DEFAULT_REAP_AGE_SECS)]
        reap_age_secs: u64,
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
    let log_to_stderr = std::env::var("SMOOTH_LOG").as_deref() == Ok("stderr")
        || matches!(&cli.command, Some(Commands::Code { headless: true, .. }) | Some(Commands::Doctor { .. }));
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
                cmd_code(
                    false,
                    None,
                    None,
                    None,
                    None,
                    false,
                    cli.resume.clone(),
                    cli.list,
                    cli.agent.clone(),
                    "deny".to_string(),
                )
                .await
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
            auto_approve,
        }) => cmd_code(headless, message, file, model, budget, json, resume, list, agent, auto_approve).await,
        Some(Commands::Doctor {
            init_home_repo,
            remote,
            fix_fda,
            setup_calendar,
            setup_imessage,
            setup_reminders,
            setup_statusline,
            onboard,
        }) => {
            if setup_statusline {
                statusline_setup::run()
            } else if init_home_repo {
                cmd_doctor_init_home_repo(remote.as_deref())
            } else if onboard {
                cmd_doctor_onboard().await
            } else if fix_fda {
                cmd_doctor_fix_fda()
            } else if setup_calendar {
                cmd_doctor_setup_calendar()
            } else if setup_imessage {
                cmd_doctor_setup_imessage()
            } else if setup_reminders {
                cmd_doctor_setup_reminders()
            } else {
                cmd_doctor().await
            }
        }
        Some(Commands::Daemon { args }) => daemon_launcher::run(args).await,
        Some(Commands::Operator { cmd }) => match cmd {
            OperatorCommands::Serve { lang, port } => operator_serve::serve(lang, port),
        },
        Some(Commands::Up {
            no_leader,
            port,
            bind,
            foreground,
            max_operators,
            skip_test,
        }) => cmd_up(no_leader, port, bind, foreground, max_operators, skip_test).await,
        Some(Commands::Down) => cmd_down().await,
        Some(Commands::Status) => cmd_status().await,
        Some(Commands::Db { cmd }) => cmd_db(cmd),
        Some(Commands::Model { cmd }) => cmd_model(cmd).await,
        Some(Commands::Auth { cmd }) => auth::dispatch(cmd).await,
        #[cfg(feature = "admin")]
        Some(Commands::Admin { cmd }) => admin::dispatch(cmd).await,
        Some(Commands::Api { cmd }) => match cmd {
            ApiCommands::Orgs { cmd } => cmd_orgs(cmd).await,
            ApiCommands::Agents { cmd } => smooai::agents::cmd(cmd).await,
            ApiCommands::Keys { cmd } => smooai::keys::cmd(cmd).await,
            ApiCommands::Members { cmd } => smooai::members::cmd(cmd).await,
            ApiCommands::Teams { cmd } => smooai::teams::cmd(cmd).await,
            ApiCommands::Crm { cmd } => smooai::crm::cmd(cmd).await,
            ApiCommands::SmoothOperator { cmd } => smooai::smooth_operator::cmd(cmd).await,
            ApiCommands::Knowledge { cmd } => smooai::knowledge::cmd(cmd).await,
            ApiCommands::Files { cmd } => smooai::files::cmd(cmd).await,
            ApiCommands::Jobs { cmd } => smooai::jobs::cmd(cmd).await,
            ApiCommands::Dashboard { cmd } => smooai::dashboard::cmd(cmd).await,
            ApiCommands::Integrations { cmd } => smooai::integrations::cmd(cmd).await,
            ApiCommands::Products { cmd } => smooai::products::cmd(cmd).await,
            ApiCommands::Referrals { cmd } => smooai::referrals::cmd(cmd).await,
            ApiCommands::Booking { cmd } => smooai::booking::cmd(cmd).await,
            ApiCommands::Profile { cmd } => smooai::profile::cmd(cmd).await,
            ApiCommands::Testing { cmd } => smooai::testing::cmd(cmd).await,
            ApiCommands::Observability { cmd } => smooai::observability::cmd(cmd).await,
        },
        Some(Commands::Org { cmd }) => cmd_orgs(cmd).await,
        Some(Commands::Config { cmd }) => config::cmd(cmd).await,
        Some(Commands::Widgets { cmd }) => smooai::widgets::cmd(cmd).await,
        Some(Commands::Dashboard { cmd }) => smooai::dashboard::cmd(cmd).await,
        Some(Commands::Crawl { cmd }) => smooai::crawl::cmd(cmd).await,
        Some(Commands::Roles { cmd }) => smooai::roles::cmd(cmd).await,
        Some(Commands::Search { args }) => smooai::websearch::run(args).await,
        Some(Commands::Knowledge { cmd }) => smooai::knowledge::cmd(cmd).await,
        Some(Commands::Crm { cmd }) => smooai::crm::cmd(cmd).await,
        Some(Commands::Branding { cmd }) => smooai::branding::cmd(cmd).await,
        Some(Commands::WebSearch { cmd }) => smooai::websearch::cmd(cmd).await,
        Some(Commands::Llm { cmd }) => smooai::llm_gateway::cmd(cmd).await,
        Some(Commands::Notify {
            message,
            title,
            priority,
            url,
            org,
        }) => smooai::notify::cmd(message, title, priority, url, org).await,
        Some(Commands::Testing { cmd }) => smooai::testing::cmd(cmd).await,
        Some(Commands::Referrals { cmd }) => smooai::referrals::cmd(cmd).await,
        Some(Commands::Booking { cmd }) => smooai::booking::cmd(cmd).await,
        Some(Commands::Heypage { cmd }) => smooai::heypage::cmd(cmd).await,
        Some(Commands::Files { cmd }) => smooai::files::cmd(cmd).await,
        Some(Commands::Operatives { cmd }) => cmd_operatives(cmd).await,
        Some(Commands::Inbox) => mail::cmd_inbox().await,
        Some(Commands::Run { pearl_id, model, agent }) => cmd_run(pearl_id.as_deref(), model.as_deref(), agent.as_deref()).await,
        Some(Commands::Approve { bead_id }) => cmd_approve(&bead_id).await,
        Some(Commands::Pause { bead_id }) => cmd_steer(&bead_id, "pause", None).await,
        Some(Commands::Resume { bead_id }) => cmd_steer(&bead_id, "resume", None).await,
        Some(Commands::Steer { bead_id, message }) => cmd_steer(&bead_id, "steer", Some(&message)).await,
        Some(Commands::Cancel { bead_id }) => cmd_steer(&bead_id, "cancel", None).await,
        Some(Commands::Attest(args)) => attest::cmd(&args),
        Some(Commands::Hooks { cmd }) => cmd_hooks(cmd),
        Some(Commands::Pearls { cmd }) => cmd_pearls(cmd).await,
        Some(Commands::Agent { cmd }) => mail::cmd_agent(cmd).await,
        Some(Commands::Msg { cmd }) => mail::cmd_msg(cmd).await,
        Some(Commands::Audit { cmd }) => cmd_audit(cmd),
        Some(Commands::Web) => {
            println!("Web UI: http://localhost:4400");
            println!("Start with: th up");
            Ok(())
        }
        Some(Commands::Claude { cmd }) => claude::cmd_claude(cmd).await,
        Some(Commands::Worktree { cmd }) => cmd_worktree(cmd),
        Some(Commands::Tailscale { cmd }) => cmd_tailscale(cmd),
        Some(Commands::Access { cmd }) => cmd_access(cmd).await,
        Some(Commands::Jira { cmd }) => cmd_jira(cmd).await,
        Some(Commands::Routing { cmd }) => cmd_routing(cmd).await,
        // `serve` is async (runs the MCP server); the rest are sync config ops.
        Some(Commands::Mcp { cmd: McpCommands::Serve }) => mcp_serve::serve_stdio().await,
        Some(Commands::Mcp { cmd }) => cmd_mcp(cmd),
        Some(Commands::Plugin { cmd }) => cmd_plugin(cmd),
        Some(Commands::Ext { cmd }) => ext::dispatch(cmd),
        Some(Commands::Service { cmd }) => cmd_service(cmd),
        Some(Commands::Skills { cmd }) => cmd_skills(cmd),
        Some(Commands::Cast { cmd }) => cmd_cast(cmd).await,
        Some(Commands::Providers { cmd }) => cmd_providers(cmd).await,
        Some(Commands::Prime) => cmd_prime(),
        Some(Commands::Project { cmd }) => cmd_project(cmd),
    }
}

/// `th project *` — the global registry view. `list` is the same data
/// `th pearls projects` prints; `create` never had an implementation and
/// can't have a sensible one (registration needs a real `.smooth/dolt/`
/// store, which is what `th pearls init` makes).
fn cmd_project(cmd: ProjectCommands) -> Result<()> {
    match cmd {
        ProjectCommands::List => print_registered_projects(),
        ProjectCommands::Create { .. } => {
            bail!("`th project create` is not implemented — run `th pearls init` inside the project to register it")
        }
    }
}

/// Print every project in `~/.smooth/registry.json`. Shared by
/// `th pearls projects` and `th project list`.
fn print_registered_projects() -> Result<()> {
    let registry = smooth_pearls::Registry::load()?;
    let projects = registry.list();
    if projects.is_empty() {
        println!("No pearl projects registered yet.");
        println!("Run {} in a project to register it.", "th pearls init".bold());
        return Ok(());
    }
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
    Ok(())
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

/// The `host:port` (plus parsed port) a running daemon advertised in
/// `~/.smooth/daemon.addr`, regardless of which launcher started it.
/// None when the file is missing/blank or the port doesn't parse.
fn advertised_daemon_addr() -> Option<(String, u16)> {
    let addr = std::fs::read_to_string(dirs_next::home_dir()?.join(".smooth").join("daemon.addr")).ok()?;
    let addr = addr.trim().to_owned();
    let port = addr.rsplit(':').next()?.parse::<u16>().ok()?;
    Some((addr, port))
}

async fn cmd_up(no_leader: bool, port: u16, bind: String, foreground: bool, max_operators: Option<usize>, skip_test: bool) -> Result<()> {
    // CLI flag beats env; set env so AppState::new() (which only sees
    // env) picks the right value in both foreground + daemon paths.
    if let Some(n) = max_operators {
        std::env::set_var("SMOOTH_SANDBOX_MAX_CONCURRENCY", n.to_string());
    }

    // Shipped-default MCP servers — populate `~/.smooth/mcp.toml` with our
    // baseline tool set (budget-aware-mcp, …). Idempotent: never touches an
    // existing entry of the same name (the user's config always wins).
    // Failures here are non-fatal — `th up` must still boot if disk is
    // read-only, the home dir is unwriteable, etc. Set
    // `SMOOTH_SKIP_DEFAULT_MCP=1` to opt out entirely.
    if std::env::var("SMOOTH_SKIP_DEFAULT_MCP").is_err() {
        if let Some(p) = mcp_config::McpConfig::default_path() {
            match mcp_config::ensure_default_mcp_servers(&p) {
                Ok(report) => {
                    for (name, outcome) in &report {
                        if matches!(outcome, mcp_config::DefaultOutcome::Added) {
                            tracing::info!(server = %name, path = %p.display(), "MCP defaults: registered shipped server");
                        }
                    }
                    // Surface a one-line install hint per default whose host probe is missing.
                    for d in mcp_config::default_mcp_servers() {
                        if !mcp_config::host_probe_on_path(d.host_probe) {
                            tracing::warn!(
                                server = d.name,
                                probe = d.host_probe,
                                hint = d.install_hint,
                                "MCP default's runtime is not on PATH — server will fail to spawn until installed"
                            );
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, path = %p.display(), "MCP defaults: ensure failed"),
            }
        }
    }
    // Smooth boots Big Smooth on the host and runs dispatched tasks
    // in-process. (The microVM sandbox mode was removed 2026-07, pearl
    // th-f4a801.) SMOOTH_WORKFLOW_DIRECT is kept set for any harness
    // that still keys off it.
    std::env::set_var("SMOOTH_WORKFLOW_DIRECT", "1");
    // Benchmark knob — skip the TEST phase so the agent doesn't
    // add tests that change the score.
    if skip_test {
        std::env::set_var("SMOOTH_WORKFLOW_SKIP_TEST", "1");
    }
    // ponytail: th up now launches the chat-first smooth-daemon (was in-process
    // smooth-bigsmooth). Point the daemon at the same host:port `th up` probes
    // via `SMOOTH_ADDR` so the re-exec health check below still finds /health.
    // The daemonized child inherits this env; the foreground path reads it too.
    std::env::set_var("SMOOTH_ADDR", format!("{bind}:{port}"));

    // Daemon mode: re-exec ourselves with --foreground and redirect
    // output to log file. Without daemonizing, `th up` would block its
    // caller until ctrl-c, breaking shell chains like
    // `th down && th up && th`.
    if !foreground {
        // A daemon from ANY launcher (the Big Smooth app bundle, a bare
        // `smooth-daemon run`) advertises itself in ~/.smooth/daemon.addr.
        // Probe it FIRST so `th up` says "already running" up front instead
        // of spawning a child that loses the ~/.smooth/daemon.lock race and
        // buries the refusal in the log (pearl th-c71e6f).
        if let Some((addr, advertised_port)) = advertised_daemon_addr() {
            if daemon_health::probe(advertised_port).await.is_up() {
                println!();
                println!();
                println!("  {} {} {}", "●".yellow(), gradient::smooth(), format!("is already running at {addr}").yellow());
                println!();
                println!("    {}  {}", "Web UI".dimmed(), format!("http://{addr}").cyan().bold());
                println!("    {}  {}", "Stop  ".dimmed(), "th down (or quit the Big Smooth app)".dimmed());
                println!();
                return Ok(());
            }
        }
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
                        println!(
                            "  {} {} {}",
                            "●".yellow(),
                            gradient::smooth(),
                            format!("is already running (pid {pid})").yellow()
                        );
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
        // Re-exec the daemon with `th up --foreground [flags...]`.
        let mut args = vec![
            "up".to_string(),
            "--foreground".to_string(),
            "--port".to_string(),
            port.to_string(),
            "--bind".to_string(),
            bind.clone(),
        ];
        if no_leader {
            args.push("--no-leader".to_string());
        }
        if let Some(n) = max_operators {
            args.push("--max-operators".to_string());
            args.push(n.to_string());
        }
        if skip_test {
            args.push("--skip-test".to_string());
        }

        let child = std::process::Command::new(exe)
            .args(&args)
            .stdout(log_file)
            .stderr(log_err)
            .stdin(std::process::Stdio::null())
            .spawn()?;

        let pid = child.id();
        std::fs::write(&pid_path, pid.to_string())?;

        // Pearl th-7840d8 — animated boot indicator while the daemon
        // child boots Big Smooth. Same timing budget as the cold-start
        // path in main() so `th up` and `th code` (with no leader
        // running) look identical to the user.
        let indicator = boot_ui::BootIndicator::new();
        let step_vm = indicator.step("starting Big Smooth");
        let step_cast = indicator.step("dolt store online");
        let step_runner = indicator.step("dispatch ready");
        let step_health = indicator.step("health check");

        const TIMEOUT_PER_STEP: std::time::Duration = std::time::Duration::from_secs(30);

        let probe = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build()?;
        let probe_url = daemon_health::health_url(port);

        // Step 1: TCP listener on :{port}.
        let vm_deadline = std::time::Instant::now() + TIMEOUT_PER_STEP;
        let mut vm_up = false;
        while std::time::Instant::now() < vm_deadline {
            if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                vm_up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        if !vm_up {
            step_vm.fail("timeout");
            step_cast.fail("not reached");
            step_runner.fail("not reached");
            step_health.fail("not reached");
            indicator.finish();
            anyhow::bail!("Big Smooth never opened :{port} — check {}", log_path.display());
        }
        step_vm.ok();

        // Step 2 + 3: HTTP listener answers /health.
        let listener_deadline = std::time::Instant::now() + TIMEOUT_PER_STEP;
        let mut listener_up = false;
        while std::time::Instant::now() < listener_deadline {
            if probe.get(&probe_url).send().await.is_ok() {
                listener_up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        if !listener_up {
            step_cast.fail("timeout");
            step_runner.fail("not reached");
            step_health.fail("not reached");
            indicator.finish();
            anyhow::bail!("Big Smooth :{port} accepted TCP but never answered HTTP — check {}", log_path.display());
        }
        step_cast.ok();
        step_runner.ok();

        // Step 4: /health returns 200.
        let health_deadline = std::time::Instant::now() + TIMEOUT_PER_STEP;
        let mut ready = false;
        while std::time::Instant::now() < health_deadline {
            if probe.get(&probe_url).send().await.is_ok_and(|r| r.status().is_success()) {
                ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        if !ready {
            step_health.fail("timeout");
            indicator.finish();
            anyhow::bail!("Big Smooth booted but :{port} never became healthy — check {}", log_path.display());
        }
        step_health.ok();
        indicator.finish();

        // Box width is sized off the *visible* text — the gradient
        // wordmark below carries ANSI escapes that inflate the byte
        // length, so we measure the plain form and render the colored
        // one. Wordmark matches the foreground `th up` startup line
        // and the TUI's role label (pearl th-2ce91f).
        let visible = format!(" Smooth started (pid {pid}) ");
        let w = visible.len() + 2;
        let colored = format!(" {} started (pid {pid}) ", gradient::smooth());
        println!();
        println!("  \x1b[2m\u{256d}{}\u{256e}\x1b[0m", "\u{2500}".repeat(w));
        println!("  \x1b[2m\u{2502}\x1b[0m {colored} \x1b[2m\u{2502}\x1b[0m");
        println!("  \x1b[2m\u{2570}{}\u{256f}\x1b[0m", "\u{2500}".repeat(w));
        println!("    {}  {}", "Web UI".dimmed(), format!("http://localhost:{port}").cyan().bold());
        println!("    {}  {}", "Logs  ".dimmed(), log_path.display().to_string().dimmed());
        println!("    {}  {}", "Stop  ".dimmed(), "th down".dimmed());
        println!();
        return Ok(());
    }

    // Foreground mode — run the chat-first daemon on the host. When this
    // is the daemon-child re-exec, the parent has already detached stdio to
    // the log file, written the pid, and returned. The daemon binds
    // `SMOOTH_ADDR` (set above to {bind}:{port}) and serves /health.
    println!();
    println!("  {} / {}", gradient::smoo_ai(), gradient::smooth());
    println!();

    if no_leader {
        println!();
        println!("  {} {}", gradient::smooth(), "infrastructure ready (Big Smooth skipped).".green());
        return Ok(());
    }

    // ponytail: th up now launches the chat-first smooth-daemon (was in-process
    // smooth-bigsmooth). All the AppState/pearl-store/addr plumbing that only fed
    // the removed `smooth_bigsmooth::server::start` is gone with it.
    crate::daemon_launcher::run(vec!["run".to_string()]).await
}

async fn cmd_down() -> Result<()> {
    // Kill the daemonized Big Smooth child recorded in the pid file.
    let pid_path = pid_file_path();
    let mut pid_killed: Option<u32> = None;
    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
                pid_killed = Some(pid);
            }
        }
        let _ = std::fs::remove_file(&pid_path);
    }

    match pid_killed {
        Some(pid) => {
            let tag = format!("(pid {pid})");
            println!("  \u{1f534} {} {} {}", gradient::smooth(), "stopped".green().bold(), tag.dimmed());
        }
        None => {
            println!("  {} {}", gradient::smooth(), "is not running.".yellow());
        }
    }
    Ok(())
}

async fn cmd_status() -> Result<()> {
    // `/health` answers with the plain string `ok` (smooth-operator's
    // LocalServer), so the daemon reports liveness and nothing else. Only
    // subsystems we actually checked get a line — the old panel printed
    // "healthy" for the Dolt store, operatives and Tailscale off JSON fields
    // that no daemon has ever sent, which is worse than saying nothing.
    let port = daemon_health::DEFAULT_PORT;
    let health = daemon_health::probe(port).await;
    println!();

    if let Some([what, fix]) = health.failure_lines(port) {
        println!(
            "  {} {} {}",
            gradient::paint("\u{2717}", |s| s.red().bold().to_string()),
            gradient::smooth_auto(),
            what
        );
        println!("  {fix}");
        println!();
        return Ok(());
    }

    println!(
        "  {} {} {} {}",
        gradient::paint("\u{2713}", |s| s.green().bold().to_string()),
        gradient::smooth_auto(),
        gradient::paint("running", |s| s.green().to_string()),
        gradient::paint(&format!("http://localhost:{port}"), |s| s.cyan().to_string()),
    );

    // A daemon that grows a richer JSON `/health` lights these up; the current
    // one doesn't send them, so they stay quiet rather than guess.
    if let daemon_health::Health::Up { details: Some(details) } = &health {
        if let Some(version) = details["version"].as_str() {
            println!("  {} {version}", gradient::paint("version", |s| s.dimmed().to_string()));
        }
        if let Some(secs) = details["uptime_seconds"].as_u64().or_else(|| details["uptime"].as_u64()) {
            println!("  {} {}", gradient::paint("uptime ", |s| s.dimmed().to_string()), format_uptime(secs));
        }
    }

    match open_pearl_store().and_then(|store| store.stats()) {
        Ok(stats) => println!(
            "  {} {} open, {} active, {} closed",
            gradient::paint("pearls ", |s| s.dimmed().to_string()),
            stats.open,
            stats.in_progress,
            stats.closed
        ),
        Err(e) => println!("  {} unreadable ({e})", gradient::paint("pearls ", |s| s.dimmed().to_string())),
    }
    println!();
    Ok(())
}

/// Human-readable uptime: `45s`, `12m 3s`, `4h 12m`.
fn format_uptime(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

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

/// The provider catalog `th model login` offers: `(id, display name, models,
/// needs_key)`. The first entry is the recommended default — it's surfaced at
/// the top of the picker. Smoo AI Gateway is the hosted LiteLLM-backed gateway
/// run by Smoo AI with billing, moderation, governance, and provider routing on
/// the server side. Display names are `String` so the recommended entry can
/// carry the gradient wordmark for "Smoo AI" alongside the rest of the label.
///
/// Paired with [`provider_config_for`]: `catalog_ids_all_build_a_config` asserts
/// every id here builds a config, so the picker can never again offer a provider
/// the save path panics on (pearl th-6062ea — the *recommended default* was the
/// one with no match arm).
fn provider_catalog() -> Vec<(&'static str, String, Vec<&'static str>, bool)> {
    vec![
        (
            "smooai-gateway",
            format!("{} Gateway (recommended)", gradient::smoo_ai()),
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
    ]
}

/// Build the engine [`ProviderConfig`](smooth_operator::providers::ProviderConfig)
/// for a [`provider_catalog`] id. `None` for an id the engine has no constructor
/// for — the caller turns that into an error instead of a panic.
fn provider_config_for(id: &str, api_key: &str) -> Option<smooth_operator::providers::ProviderConfig> {
    use smooth_operator::providers::ProviderConfig as P;
    Some(match id {
        "smooai-gateway" => P::smooai_gateway(api_key),
        "llmgateway" => P::llmgateway(api_key),
        "kimi-code" => P::kimi_code(api_key),
        "kimi" => P::kimi(api_key),
        "openrouter" => P::openrouter(api_key),
        "openai" => P::openai(api_key),
        "anthropic" => P::anthropic(api_key),
        "google" => P::google(api_key),
        "ollama" => P::ollama(),
        _ => return None,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is the idiom for test assertions")]
mod provider_catalog_tests {
    use super::{provider_catalog, provider_config_for};

    /// The picker's list and the config builder used to be two hand-maintained
    /// match arms; `smooai-gateway` — the RECOMMENDED DEFAULT — was in the first
    /// and missing from the second, so picking it hit `unreachable!()` and
    /// panicked on a new user's very first command (pearl th-6062ea). This test
    /// is the structural link: adding a catalog entry without a constructor is a
    /// red build, not a first-run panic.
    #[test]
    fn catalog_ids_all_build_a_config() {
        for (id, ..) in provider_catalog() {
            let cfg = provider_config_for(id, "test-key").unwrap_or_else(|| panic!("catalog offers `{id}` with no provider_config_for arm"));
            assert_eq!(cfg.id, id, "`{id}` must build a config carrying its own id");
            assert!(!cfg.api_url.is_empty(), "`{id}` needs an api_url");
        }
    }

    #[test]
    fn catalog_is_unique_and_leads_with_the_recommended_gateway() {
        let catalog = provider_catalog();
        assert_eq!(
            catalog.first().map(|(id, ..)| *id),
            Some("smooai-gateway"),
            "recommended default leads the picker"
        );
        let mut ids: Vec<&str> = catalog.iter().map(|(id, ..)| *id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate catalog ids");
        // Every entry offers at least one model to pick.
        assert!(catalog.iter().all(|(_, _, models, _)| !models.is_empty()));
    }

    #[test]
    fn unknown_provider_is_none_not_a_panic() {
        assert!(provider_config_for("no-such-provider", "k").is_none());
        assert!(provider_config_for("", "k").is_none());
    }

    /// Ollama is the one keyless entry — it must build without a key.
    #[test]
    fn ollama_needs_no_key() {
        let (_, _, _, needs_key) = provider_catalog().into_iter().find(|(id, ..)| *id == "ollama").expect("ollama in catalog");
        assert!(!needs_key);
        assert_eq!(provider_config_for("ollama", "").expect("builds").api_key, "");
    }
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
                                    "none configured \u{2014} run: th model login <provider>".red()
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
                        "not configured \u{2014} run: th model login <provider>".red()
                    );
                }
            }

            // Shared probe: a 404 from something else squatting :4400 used to
            // read as "running" here, because `reqwest::get(..).is_ok()` only
            // means "a response came back".
            let leader_up = daemon_health::probe(daemon_health::DEFAULT_PORT).await.is_up();
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

            let catalog = provider_catalog();

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
                        let _ = std::io::Write::flush(&mut anstream::stdout());
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
            let config = provider_config_for(&provider_id, &api_key).with_context(|| format!("no client for provider '{provider_id}'"))?;

            // Quick test: send a tiny request
            let test_llm = smooth_operator::llm::LlmClient::new(smooth_operator::llm::LlmConfig {
                api_url: config.api_url.clone(),
                api_key: config.api_key.clone(),
                model: model.clone(),
                max_tokens: 32,
                temperature: smooth_policy::llm_params::AGENT_TEMPERATURE,
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
                                println!("No providers configured. Run: th model login <provider>");
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
                    println!("No providers configured. Run: th model login <provider>");
                }
            }
        }
        ModelCommands::Default { provider } => {
            let path = providers_path.as_ref().context("cannot determine home directory")?;
            if let Some(p) = provider {
                if !path.exists() {
                    println!("No providers configured. Run: th model login {p} --api-key YOUR_KEY");
                    return Ok(());
                }
                let mut registry = smooth_cast::provider_migration::load_providers_with_migration(path)?;
                if registry.get_provider(&p).is_none() {
                    println!("Provider {p} not configured. Run: th model login {p} --api-key YOUR_KEY");
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
                println!("No providers configured. Run: th model login <provider> --api-key YOUR_KEY");
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

async fn cmd_operatives(cmd: Option<OperativesCommands>) -> Result<()> {
    let client = reqwest::Client::new();
    match cmd.unwrap_or(OperativesCommands::List) {
        OperativesCommands::List => {
            let resp = client.get("http://localhost:4400/api/workers").send().await;
            let json: serde_json::Value = match resp {
                Ok(r) => r.json().await.unwrap_or(serde_json::json!({"data": []})),
                Err(_) => {
                    println!("Cannot reach Big {}. Run: th up", gradient::smooth());
                    return Ok(());
                }
            };
            let empty = vec![];
            let workers = json["data"].as_array().unwrap_or(&empty);
            if workers.is_empty() {
                println!("\n  {} No active {} operatives.\n", "ℹ".cyan(), gradient::smooth());
                return Ok(());
            }
            println!("\n  {} {} {}\n", "Active".cyan().bold(), gradient::smooth(), "operatives".cyan().bold());
            for w in workers {
                let id = w.get("operator_id").and_then(|v| v.as_str()).unwrap_or("?");
                let bead = w.get("bead_id").and_then(|v| v.as_str()).unwrap_or("");
                let host_port = w.get("host_port").and_then(serde_json::Value::as_u64).unwrap_or(0);
                let ports = w.get("port_mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                println!("  {} {} {}", "●".green().bold(), id.bold(), format!("(pearl {bead})").dimmed());
                if host_port > 0 {
                    println!("    {} {}", "runner ws".dimmed(), format!("ws://localhost:{host_port}").cyan());
                }
                for p in ports {
                    if let (Some(guest), Some(host)) = (p.get(0).and_then(serde_json::Value::as_u64), p.get(1).and_then(serde_json::Value::as_u64)) {
                        if guest != 4096 {
                            // Skip the runner's own control port; show user-useful forwards.
                            println!("    {} guest:{guest} → {}", "port".dimmed(), format!("http://localhost:{host}").cyan());
                        }
                    }
                }
            }
            println!();
            Ok(())
        }
        OperativesCommands::Kill { operator_id } => {
            let url = format!("http://localhost:4400/api/workers/{operator_id}");
            let resp = client.delete(&url).send().await;
            match resp {
                Ok(r) => {
                    let body: serde_json::Value = r.json().await.unwrap_or(serde_json::json!({"ok": false}));
                    if body.get("ok").and_then(serde_json::Value::as_bool).unwrap_or(false) {
                        println!("\n  {} Operator {} stopped.\n", "✓".green().bold(), operator_id.bold());
                    } else {
                        println!("\n  {} No active operator with id {}\n", "✗".red().bold(), operator_id.bold());
                    }
                }
                Err(_) => println!("Cannot reach Big {}. Run: th up", gradient::smooth()),
            }
            Ok(())
        }
    }
}

async fn cmd_run(pearl_id_arg: Option<&str>, model: Option<&str>, agent: Option<&str>) -> Result<()> {
    // Validate the agent name up front so a typo fails at the CLI
    // instead of falling through to the runner's "unknown agent,
    // falling back to code" warning.
    let agent_name = resolve_primary_agent(agent)?;
    // Resolve the task message.
    // - If pearl_id_arg looks like a pearl id (starts with "th-"), fetch
    //   the pearl's title+description and use that as the task message.
    // - Otherwise treat the whole arg as an ad-hoc task message.
    // - If missing, grab the first ready pearl.
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build()?;

    let (pearl_id, message) = match pearl_id_arg {
        Some(arg) if arg.starts_with("th-") => {
            let url = format!("http://localhost:4400/api/pearls/{arg}");
            let resp: serde_json::Value = client.get(&url).send().await?.json().await?;
            let data = resp.get("data").cloned().unwrap_or(serde_json::Value::Null);
            if data.is_null() {
                anyhow::bail!("pearl {arg} not found");
            }
            let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let desc = data.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let body = if desc.is_empty() { title.clone() } else { format!("{title}\n\n{desc}") };
            (Some(arg.to_string()), body)
        }
        Some(adhoc) => (None, adhoc.to_string()),
        None => {
            // Take the first ready pearl.
            let resp: serde_json::Value = client.get("http://localhost:4400/api/pearls/ready").send().await?.json().await?;
            let first = resp.get("data").and_then(|v| v.as_array()).and_then(|a| a.first()).cloned();
            let first = first.ok_or_else(|| anyhow::anyhow!("no ready pearls — pass a pearl id or a task description"))?;
            let id = first.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let title = first.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let desc = first.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let body = if desc.is_empty() { title.clone() } else { format!("{title}\n\n{desc}") };
            (Some(id), body)
        }
    };

    let cwd = std::env::current_dir()?;

    if let Some(ref id) = pearl_id {
        println!("\n  {} {} {}", "▶".cyan().bold(), "Running pearl".bold(), id.bold());
    } else {
        println!("\n  {} {}", "▶".cyan().bold(), "Running ad-hoc task".bold());
    }
    println!("  {} {}", "cwd".dimmed(), cwd.display().to_string().dimmed());
    println!("  {} {}", "agent".dimmed(), agent_name.dimmed());
    println!();

    let body = serde_json::json!({
        "message": message,
        "model": model,
        "working_dir": cwd.to_string_lossy(),
        "agent": agent_name,
    });

    // Stream SSE from /api/tasks.
    use futures_util::StreamExt;

    let stream_client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30 * 60)).build()?;
    let resp = stream_client.post("http://localhost:4400/api/tasks").json(&body).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("dispatch failed: HTTP {}", resp.status());
    }

    let mut byte_stream = resp.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk_res) = byte_stream.next().await {
        let chunk = chunk_res?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // SSE frames separated by "\n\n". Each starts with "data: ".
        while let Some(idx) = buffer.find("\n\n") {
            let frame = buffer[..idx].to_string();
            buffer.drain(..=idx + 1);

            for line in frame.lines() {
                let Some(payload) = line.strip_prefix("data: ") else {
                    continue;
                };
                let Ok(evt) = serde_json::from_str::<serde_json::Value>(payload) else {
                    continue;
                };
                let kind = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match kind {
                    "TokenDelta" => {
                        if let Some(content) = evt.get("content").and_then(|v| v.as_str()) {
                            print!("{content}");
                            let _ = std::io::Write::flush(&mut anstream::stdout());
                        }
                    }
                    "ToolCallStart" => {
                        let tool = evt.get("tool_name").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("\n  {} {}", "⚙".cyan(), tool.dimmed());
                    }
                    "ToolCallComplete" => {
                        let tool = evt.get("tool_name").and_then(|v| v.as_str()).unwrap_or("?");
                        let is_error = evt.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false);
                        if is_error {
                            let result = evt.get("result").and_then(|v| v.as_str()).unwrap_or("");
                            println!("  {} {} {}", "✗".red().bold(), tool.dimmed(), result.red());
                        }
                    }
                    "Complete" | "TaskComplete" => {
                        println!("\n  {} agent completed", "✓".green().bold());
                    }
                    "Error" | "TaskError" => {
                        let msg = evt.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error");
                        println!("\n  {} {msg}", "✗".red().bold());
                    }
                    _ => {}
                }
            }
        }
    }

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

/// Every audit stream in `dir` as `(actor, path, size_bytes)`, most recently
/// written first.
///
/// Both extensions count: the pre-microVM per-actor writers used `<actor>.log`,
/// and the one live writer today — goalie's egress proxy, wired in
/// `smooth-daemon/src/config.rs` — writes JSON lines to `egress-proxy.jsonl`
/// (pearl th-f50195). Matching only `.log` made the sole real audit stream
/// invisible to `th audit`.
fn audit_streams(dir: &std::path::Path) -> Vec<(String, std::path::PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, std::path::PathBuf, u64, std::time::SystemTime)> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let path = e.path();
            if !path.extension().is_some_and(|x| x == "log" || x == "jsonl") {
                return None;
            }
            let actor = path.file_stem()?.to_string_lossy().into_owned();
            let meta = e.metadata().ok()?;
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            Some((actor, path, meta.len(), modified))
        })
        .collect();
    // Newest first, name as a stable tiebreak (mtime granularity ties in tests).
    out.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)));
    out.into_iter().map(|(a, p, n, _)| (a, p, n)).collect()
}

/// The stream `th audit tail` should read: the named actor under either
/// extension, or — with no actor — the most recently written stream.
///
/// The old default was the literal actor `leader`, which no longer exists
/// anywhere post-microVM, so a bare `th audit tail` could only ever report
/// "No audit log for leader".
fn resolve_audit_stream(dir: &std::path::Path, actor: Option<&str>) -> Option<(String, std::path::PathBuf)> {
    let streams = audit_streams(dir);
    match actor {
        Some(name) => streams.into_iter().find(|(a, _, _)| a == name).map(|(a, p, _)| (a, p)),
        None => streams.into_iter().next().map(|(a, p, _)| (a, p)),
    }
}

fn cmd_audit(cmd: AuditCommands) -> Result<()> {
    // ponytail: standard local audit dir (~/.smooth/audit) — was smooth_bigsmooth::audit::get_audit_dir().
    let dir = dirs_next::home_dir()
        .map(|h| h.join(".smooth").join("audit"))
        .context("no home dir for ~/.smooth/audit")?;
    match cmd {
        AuditCommands::Path => println!("{}", dir.display()),
        AuditCommands::List => {
            let streams = audit_streams(&dir);
            if streams.is_empty() {
                println!("No audit logs yet.");
                return Ok(());
            }
            for (actor, _, bytes) in streams {
                println!("  {actor:<24} {:.1} KB", bytes as f64 / 1024.0);
            }
        }
        AuditCommands::Tail { actor, lines } => {
            let Some((actor, path)) = resolve_audit_stream(&dir, actor.as_deref()) else {
                match actor {
                    Some(a) => println!("No audit log for {a}"),
                    None => println!("No audit logs yet in {}", dir.display()),
                }
                return Ok(());
            };
            println!("{}", path.display());
            let content = std::fs::read_to_string(&path).with_context(|| format!("read audit log for {actor}"))?;
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
                    println!("{:<38} {:<10} {:<14} {:<30} Reason", "ID", "Kind", "Bead", "Resource");
                    println!("{}", "-".repeat(120));
                    for req in requests {
                        println!(
                            "{:<38} {:<10} {:<14} {:<30} {}",
                            req["id"].as_str().unwrap_or("-"),
                            req["kind"].as_str().unwrap_or("-"),
                            req["bead_id"].as_str().unwrap_or("-"),
                            req["resource"].as_str().unwrap_or("-"),
                            req["reason"].as_str().unwrap_or("-"),
                        );
                    }
                    println!();
                    println!("Resolve with: th access approve <id> [--scope=session|project|user] [--glob=*.example.com]");
                    println!("              th access deny <id> [--scope=user]");
                }
            }
        }
        AccessCommands::Approve { id, scope, glob } => {
            let mut body = serde_json::Map::new();
            body.insert("id".into(), serde_json::Value::String(id.clone()));
            body.insert("scope".into(), serde_json::Value::String(scope.clone()));
            if let Some(g) = glob {
                body.insert("glob_override".into(), serde_json::Value::String(g));
            }
            let resp = client.post(format!("{base}/approve")).json(&serde_json::Value::Object(body)).send().await?;
            if resp.status().is_success() {
                println!("Approved {id} at scope {scope}");
            } else {
                let status = resp.status();
                println!("Failed ({status}): {}", resp.text().await.unwrap_or_default());
            }
        }
        AccessCommands::Deny { id, scope } => {
            let resp = client
                .post(format!("{base}/deny"))
                .json(&serde_json::json!({"id": id, "scope": scope}))
                .send()
                .await?;
            if resp.status().is_success() {
                println!("Denied {id} at scope {scope}");
            } else {
                let status = resp.status();
                println!("Failed ({status}): {}", resp.text().await.unwrap_or_default());
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
    use std::io::{IsTerminal, Read};
    // Only read if stdin is not a terminal (i.e. data is piped in)
    if std::io::stdin().is_terminal() {
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
        "  • Always-on agent daemon: Big {}            {}",
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
    auto_approve: String,
) -> Result<()> {
    // Validate the agent name at CLI time so a typo doesn't waste a
    // runner spin-up. The value flows into the TUI's status bar and
    // into dispatch's `agent` field when the user sends a message.
    let agent_name = resolve_primary_agent(agent.as_deref())?;
    // Validate the auto-approve mode at CLI time too. We pin the
    // string to one of the known forms early so a typo doesn't
    // silently fall through to "deny" later. Pearl th-400773.
    //
    // ponytail: the old smooth_bench::scenarios::AutoApprove parser + headless
    // resolver polled the removed in-process Big Smooth `/api/access` queue. The
    // chat-first daemon owns permissions (auto-mode) now, so there's nothing to
    // spawn here — just validate the flag locally.
    const AUTO_APPROVE_MODES: [&str; 5] = ["deny", "once", "session", "project", "user"];
    if !AUTO_APPROVE_MODES.contains(&auto_approve.as_str()) {
        anyhow::bail!("unknown --auto-approve mode '{auto_approve}': expected one of deny/once/session/project/user");
    }
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
                    let source_label = skill.source.label();
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
                println!("  {} {}", "\u{26a0}".yellow().bold(), "No providers configured. Run: th model login".yellow());
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

    // Check if Big Smooth is running. If not, boot it via the same
    // daemonized path `th up` takes — one auto-start route, so the
    // daemon always comes up with the same env, log file, and pid file
    // regardless of which command triggered it.
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build()?;

    if !daemon_health::probe(daemon_health::DEFAULT_PORT).await.is_up() {
        // Pearl th-7840d8 — animated boot indicator (was a bare
        // `Starting Smooth...`). Daemonization happens in the
        // background via `th up`; the parent polls `/health` and
        // advances steps based on observable signals. Labels match the
        // `th up` path so both cold-start routes look identical.
        let indicator = boot_ui::BootIndicator::new();
        let step_vm = indicator.step("starting Big Smooth");
        let step_cast = indicator.step("dolt store online");
        let step_runner = indicator.step("dispatch ready");
        let step_health = indicator.step("health check");

        // Re-exec ourselves as `th up` so Big Smooth daemonizes exactly
        // the way it would if the user had typed `th up`. The child
        // detaches its stdio to ~/.smooth/smooth.log, writes
        // ~/.smooth/smooth.pid, returns immediately, and the daemon
        // keeps running in the background until `th down`.
        let exe = std::env::current_exe()?;
        let status = std::process::Command::new(exe)
            .arg("up")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .status()
            .context("spawn `th up` to boot Big Smooth")?;
        if !status.success() {
            // The daemon spawn itself failed before the VM ever
            // got off the ground. Mark every step failed so the
            // user has a clear transcript.
            step_vm.fail(&format!("`th up` exited {}", status.code().unwrap_or(-1)));
            step_cast.fail("not started");
            step_runner.fail("not started");
            step_health.fail("not started");
            indicator.finish();
            anyhow::bail!("`th up` failed (exit {})", status.code().unwrap_or(-1));
        }

        // VM daemon spawned. From here on we poll observable
        // signals to advance the steps. Total budget is ~30s — the
        // same as the old hard-coded health-poll loop — split across
        // the four steps.
        //
        // The signals we can actually probe from the host:
        //   * daemon up: TCP connect to localhost:4400 succeeds.
        //   * store + dispatch ready: implied once /health responds;
        //     the daemon only flips the listener on after its internal
        //     init is done.
        //
        // So we drive step_vm off the TCP probe, and once /health
        // returns 200 we cascade the remaining three. This is
        // intentionally coarse — v1 doesn't thread real progress
        // events out of the daemon (would need a separate IPC
        // channel; pearl can land later if we want finer-grained
        // visibility).
        const TIMEOUT_PER_STEP: std::time::Duration = std::time::Duration::from_secs(30);

        // Step 1: wait for TCP listener on :4400.
        let vm_deadline = std::time::Instant::now() + TIMEOUT_PER_STEP;
        let mut vm_up = false;
        while std::time::Instant::now() < vm_deadline {
            if tokio::net::TcpStream::connect(("127.0.0.1", 4400)).await.is_ok() {
                vm_up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        if !vm_up {
            step_vm.fail("timeout");
            step_cast.fail("not reached");
            step_runner.fail("not reached");
            step_health.fail("not reached");
            indicator.finish();
            anyhow::bail!("Big Smooth never opened :4400 — check ~/.smooth/smooth.log");
        }
        step_vm.ok();

        // Step 2 + 3: wait for /health to respond at all (any response
        // means the daemon's listener is up; store + dispatch init is
        // what gates that listener flipping on). We split them
        // visually for the receipt.
        let cast_deadline = std::time::Instant::now() + TIMEOUT_PER_STEP;
        let mut listener_up = false;
        while std::time::Instant::now() < cast_deadline {
            if client.get(daemon_health::health_url(daemon_health::DEFAULT_PORT)).send().await.is_ok() {
                listener_up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        if !listener_up {
            step_cast.fail("timeout");
            step_runner.fail("not reached");
            step_health.fail("not reached");
            indicator.finish();
            anyhow::bail!("Big Smooth :4400 accepted TCP but never answered HTTP — check ~/.smooth/smooth.log");
        }
        step_cast.ok();
        step_runner.ok();

        // Step 4: /health returns 200 (state.touch + everything
        // wired up).
        let health_deadline = std::time::Instant::now() + TIMEOUT_PER_STEP;
        let mut ready = false;
        while std::time::Instant::now() < health_deadline {
            if client
                .get(daemon_health::health_url(daemon_health::DEFAULT_PORT))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        if !ready {
            step_health.fail("timeout");
            indicator.finish();
            anyhow::bail!("Big Smooth booted but :4400 never became healthy — check ~/.smooth/smooth.log");
        }
        step_health.ok();
        indicator.finish();
    }

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
            let outcome = hooks::install(None)?;
            hooks::print_install_outcome(&outcome);
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
/// A one-time setup step `th doctor` reports and `--onboard` drives (pearl
/// th-ba764e). Each maps to an already-existing `th doctor --…` flag: doctor
/// raises what isn't ready, `--onboard` walks them in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "the TCC grants are macOS-only"))]
enum SetupStep {
    Providers,
    SmooLogin,
    Fda,
    Calendar,
    Reminders,
    Messages,
}

impl SetupStep {
    fn label(self) -> &'static str {
        match self {
            Self::Providers => "LLM provider credentials",
            Self::SmooLogin => "Smoo AI sign-in",
            Self::Fda => "Full Disk Access",
            Self::Calendar => "Calendar access",
            Self::Reminders => "Reminders access",
            Self::Messages => "Messages access",
        }
    }

    /// The command that fixes it — printed by `th doctor`, run by `--onboard`.
    fn fix(self) -> &'static str {
        match self {
            Self::Providers => "th model login <provider>",
            Self::SmooLogin => "th auth login",
            Self::Fda => "th doctor --fix-fda",
            Self::Calendar => "th doctor --setup-calendar",
            Self::Reminders => "th doctor --setup-reminders",
            Self::Messages => "th doctor --setup-imessage",
        }
    }

    /// Run this step. The two credential steps are interactive flows of their
    /// own, so onboarding points at them rather than hijacking the terminal.
    fn drive(self) -> Result<()> {
        match self {
            Self::Providers | Self::SmooLogin => {
                println!("  {} run: {}", "→".cyan(), self.fix().bold());
                Ok(())
            }
            Self::Fda => cmd_doctor_fix_fda(),
            Self::Calendar => cmd_doctor_setup_calendar(),
            Self::Reminders => cmd_doctor_setup_reminders(),
            Self::Messages => cmd_doctor_setup_imessage(),
        }
    }
}

async fn cmd_doctor() -> Result<()> {
    run_doctor().await.map(|_| ())
}

/// The health check. Returns the setup steps that aren't ready, in the order
/// they should be done — `--onboard` walks exactly that list.
async fn run_doctor() -> Result<Vec<SetupStep>> {
    println!("{} {}", gradient::smooth(), "Doctor".bold().cyan());
    println!("{}", "checking system health...\n".dimmed());

    let mut issues = 0;
    let mut pending: Vec<SetupStep> = Vec::new();

    // 1. Check Big Smooth API — same probe and same wording as `th status`.
    let port = daemon_health::DEFAULT_PORT;
    match daemon_health::probe(port).await.failure_lines(port) {
        None => println!("  {} Big {} API: {}", "✓".green().bold(), gradient::smooth(), "healthy".green()),
        Some([what, fix]) => {
            println!("  {} Big {} API: {}", "✗".red().bold(), gradient::smooth(), what.red());
            println!("    {fix}");
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
            println!("  {} Providers: {}", "✗".red().bold(), "not configured (run: th model login <provider>)".red());
            issues += 1;
            pending.push(SetupStep::Providers);
        }
    }

    // 3b. Smoo AI session — `th api …`, the config server and the hosted
    // gateway all need one. Not a health *failure* (a local-only Big Smooth
    // works without it), so it's raised as a setup step, not an issue.
    let profile = smooth_policy::auth_paths::active_profile();
    let signed_in = smooth_policy::auth_paths::user_file(profile.as_deref()).exists() || smooth_policy::auth_paths::m2m_file(profile.as_deref()).exists();
    if signed_in {
        let which = profile.as_deref().unwrap_or("default");
        println!("  {} Smoo AI: {}", "✓".green().bold(), format!("signed in (profile: {which})").green());
    } else {
        println!("  {} Smoo AI: {}", "○".dimmed(), "not signed in (run: th auth login)".dimmed());
        pending.push(SetupStep::SmooLogin);
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

    // 7. Flag the legacy SQLite store. Nothing reads it and the
    // `migrate-from-sqlite` command it used to point at no longer exists
    // (pearl th-91de11), so the only honest advice is "delete it".
    let sqlite_path = dirs_next::home_dir().map(|h| h.join(".smooth/smooth.db"));
    if let Some(ref path) = sqlite_path {
        if path.exists() && find_dolt_dir().is_ok() {
            println!(
                "  {} SQLite: {}",
                "○".dimmed(),
                format!("legacy smooth.db is unread — safe to delete: rm {}", path.display()).dimmed()
            );
        }
    }

    // 8. Sandboxes (built-in via microsandbox crate)
    println!("  {} Sandboxes: {}", "✓".green().bold(), "built-in (microsandbox)".green());

    // 10. Workspace on a TCC-gated external volume (macOS). If the daemon's
    // workspace lives under /Volumes it needs Full Disk Access, or every fs op
    // there returns EPERM and Big Smooth looks jailed (pearl th-b85641).
    #[cfg(target_os = "macos")]
    if let Some(ext) = fda::candidate_workspace().as_deref().and_then(fda::workspace_on_external_volume) {
        let denied = fda::read_access_denied(&ext);
        let msg = if denied {
            "external volume — access DENIED"
        } else {
            "external volume — needs Full Disk Access"
        };
        println!("  {} Workspace: {}", "✗".red().bold(), format!("{} ({})", msg, ext.display()).red());
        println!(
            "    {} grant Full Disk Access to th + the daemon: run `th doctor --fix-fda` on the host console",
            "→".cyan()
        );
        issues += 1;
        pending.push(SetupStep::Fda);
    } else {
        println!("  {} Workspace: {}", "✓".green().bold(), "on boot volume (no Full Disk Access needed)".green());
    }

    // 11. macOS access grants (pearl th-ba764e). Before this they were only
    // visible behind the `--setup-*` flags, so a bare `th doctor` said "all
    // checks passed" on a Mac where every personal-data tool was dead. RAISE
    // them here.
    //
    // The grants belong to **Big Smooth.app** — TCC is per-binary — so what
    // `th` can read about itself is a proxy, never proof. Say so, and never
    // report a grant as present on `th`'s word alone.
    #[cfg(target_os = "macos")]
    {
        use smooth_menubar::eventkit::{calendar_access, reminders_access, Access};

        println!("\n  {}", "macOS access".bold());
        println!("    {}", "grants belong to Big Smooth.app; `th`'s own probe is a proxy, not proof".dimmed());

        if let Some(app) = calendar_setup::app_bundle() {
            println!(
                "    {} Big {}.app: {}",
                "✓".green().bold(),
                gradient::smooth(),
                app.display().to_string().dimmed()
            );
        } else {
            println!(
                "    {} Big {}.app: not installed — every grant below attaches to it",
                "✗".red().bold(),
                gradient::smooth()
            );
            println!("      {} {}", "→".cyan(), "scripts/macos/install-local.sh".bold());
        }

        // The calendar tool shells `ical`; no binary, no calendar, grant or not.
        if let Some(bin) = smooth_tools::calendar::resolve_ical() {
            println!("    {} ical CLI: {}", "✓".green().bold(), bin.display().to_string().dimmed());
        } else {
            println!("    {} ical CLI: {}", "✗".red().bold(), "not installed (the calendar tool shells it)".red());
            println!("      {} {}", "→".cyan(), SetupStep::Calendar.fix().bold());
            pending.push(SetupStep::Calendar);
        }

        for (step, name) in [(SetupStep::Calendar, "Calendar"), (SetupStep::Reminders, "Reminders")] {
            let access = if step == SetupStep::Calendar { calendar_access() } else { reminders_access() };
            match access {
                Access::Granted => println!("    {} {name}: {} for `th`", "✓".green().bold(), access.label().green()),
                Access::NotDetermined => {
                    println!("    {} {name}: {} — nobody has asked yet", "○".dimmed(), access.label().dimmed());
                    println!("      {} {} (or Big Smooth's Set Up menu)", "→".cyan(), step.fix().bold());
                    if !pending.contains(&step) {
                        pending.push(step);
                    }
                }
                Access::Denied => {
                    println!("    {} {name}: {} — macOS was told no", "✗".red().bold(), access.label().red());
                    println!("      {} {} (or Big Smooth's Set Up menu)", "→".cyan(), step.fix().bold());
                    if !pending.contains(&step) {
                        pending.push(step);
                    }
                }
            }
        }

        // Messages is the one grant `th` can check for real: chat.db is
        // FDA-gated, so a successful read means Full Disk Access is in place.
        match smooth_tools::imessage::chat_db_path() {
            Some(path) => match smooth_tools::imessage::probe(&path) {
                Ok(()) => println!("    {} Messages: {}", "✓".green().bold(), "chat.db readable (Full Disk Access granted)".green()),
                Err(smooth_tools::imessage::Unavailable::Missing) => {
                    println!("    {} Messages: {}", "○".dimmed(), format!("no chat.db at {}", path.display()).dimmed());
                    println!("      {} open Messages.app and sign in once, then re-run", "→".cyan());
                }
                Err(smooth_tools::imessage::Unavailable::Denied) => {
                    println!(
                        "    {} Messages: {}",
                        "✗".red().bold(),
                        "chat.db unreadable — Full Disk Access not granted".red()
                    );
                    println!("      {} {} (or Big Smooth's Set Up menu)", "→".cyan(), SetupStep::Messages.fix().bold());
                    pending.push(SetupStep::Messages);
                }
            },
            None => println!("    {} Messages: {}", "✗".red().bold(), "cannot determine the home directory".red()),
        }
        println!();
    }

    // 9. Git hooks
    let hooks_status = hooks::check(None);
    if !hooks::print_doctor_status(&hooks_status) {
        issues += 1;
        // Auto-fix: install hooks
        println!("    {} installing hooks...", "→".cyan());
        match hooks::install(None) {
            Ok(hooks::InstallOutcome::Installed(hooks_dir)) => {
                println!("    {} fixed: hooks installed at {}", "✓".green().bold(), hooks_dir.display());
                issues -= 1;
            }
            Ok(outcome @ hooks::InstallOutcome::SkippedForeign(_)) => {
                // Repo has its own hooks (husky etc.); not an issue to fix.
                hooks::print_install_outcome(&outcome);
                issues -= 1;
            }
            Err(e) => {
                println!("    {} could not auto-install hooks: {e}", "✗".red().bold());
            }
        }
    }

    // 12. Reclaimable disk in ~/.smooth (pearl th-91de11). Reported, never
    // deleted — these are the user's build caches, logs and credential
    // backups, and doctor only ever auto-fixes config it owns.
    if let Some(ref dir) = smooth_home {
        let found = reclaim::findings(dir, &smooth_policy::auth_paths::auth_dir());
        if !found.is_empty() {
            let total: u64 = found.iter().map(|f| f.bytes).sum();
            println!("\n  {} ({} reclaimable)", "Disk".bold(), reclaim::human_bytes(total).yellow().bold());
            for f in &found {
                println!("    {} {} {}", "○".dimmed(), f.what, format!("({})", reclaim::human_bytes(f.bytes)).dimmed());
                println!("      {} {}", "→".cyan(), f.hint.bold());
            }
        }
    }

    println!();
    if issues == 0 && pending.is_empty() {
        println!("{} {} {}", "All checks passed.".green().bold(), gradient::smooth(), "is ready.".green().bold());
    } else if issues > 0 {
        println!("{}", format!("{issues} issue(s) found. Fix them and run: th doctor").yellow().bold());
    }
    if !pending.is_empty() {
        let labels = pending.iter().map(|s| s.label()).collect::<Vec<_>>().join(", ");
        println!("{}", format!("{} setup step(s) not ready: {labels}", pending.len()).yellow().bold());
        println!("{}", "Walk them all: th doctor --onboard".yellow());
    }

    Ok(pending)
}

/// `th doctor --onboard` — the guided first-run flow (pearl th-ba764e). Runs the
/// full health check, then walks every not-ready setup step in order, driving
/// each one's existing `--setup-*` path. This is the CLI backbone Big Smooth's
/// "Set Up" menu and the daemon's first-run onboarding shell out to, so the
/// sequence lives in exactly one place.
async fn cmd_doctor_onboard() -> Result<()> {
    let pending = run_doctor().await?;
    if pending.is_empty() {
        println!("\n{} nothing left to set up.", "✓".green().bold());
        return Ok(());
    }

    println!("\n{} {}", gradient::smooth(), "Onboarding".bold().cyan());
    println!("{}", format!("walking {} setup step(s)…", pending.len()).dimmed());
    println!("{}", "run this on the Mac's console — the macOS prompts never appear over SSH\n".dimmed());

    let total = pending.len();
    for (i, step) in pending.iter().enumerate() {
        println!("{}", format!("── {}/{total}  {}", i + 1, step.label()).bold().cyan());
        // One failing step must not strand the rest — report and keep walking.
        if let Err(e) = step.drive() {
            println!("  {} {} failed: {e:#}", "✗".red().bold(), step.label());
            println!("  {} fix by hand with: {}", "→".cyan(), step.fix().bold());
        }
        println!();
    }

    println!("{}", "Onboarding walked every step.".bold());
    println!(
        "  {} click through any macOS prompts that appeared, then verify: {}",
        "→".cyan(),
        "th doctor".bold()
    );
    Ok(())
}

/// Guide the one-time Full Disk Access grant (pearl th-b85641). FDA can't be set
/// programmatically (SIP-protected TCC.db), so this opens the settings pane and
/// reveals the binaries to drag in — the fastest a grant can be made.
#[cfg(target_os = "macos")]
fn cmd_doctor_fix_fda() -> Result<()> {
    println!("{} {}", gradient::smooth(), "Full Disk Access".bold().cyan());

    let targets = fda::grant_targets();
    println!("\n  These binaries need Full Disk Access (grant is per-binary):");
    for t in &targets {
        println!("    {} {}", "•".cyan(), t.display().to_string().bold());
    }

    println!(
        "\n  {} Opening the Full Disk Access settings pane + revealing each binary in Finder…",
        "→".cyan()
    );
    if let Err(e) = fda::open_fda_settings() {
        println!("    {} couldn't open System Settings ({e}). Open it by hand:", "✗".red().bold());
        println!("      System Settings → Privacy & Security → Full Disk Access");
    }
    for t in &targets {
        let _ = fda::reveal_in_finder(t);
    }

    println!("\n  Then, in the Full Disk Access list:");
    println!(
        "    1. Click {} and drag each revealed binary in (or add it), toggle it {}.",
        "+".bold(),
        "on".green()
    );
    println!("    2. Restart the daemon so it re-reads the grant: {}.", "th down && th up".bold());
    println!(
        "\n  {} the `th` grant is keyed to its code signature; every `pnpm install:th` rebuild",
        "⚠".yellow().bold()
    );
    println!("     changes it and drops the grant. Sign `th` with a stable identity to make it stick,");
    println!("     or keep the workspace on the boot volume to avoid the gate entirely.");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn cmd_doctor_fix_fda() -> Result<()> {
    println!("{} Full Disk Access is a macOS-only concept; nothing to do here.", "○".dimmed());
    Ok(())
}

/// Install the `ical` CLI and drive the one-time Calendar (EventKit) grant so
/// Big Smooth's `calendar` tool works (pearl th-94cc4a). Logic lives in
/// [`calendar_setup`] so the app bundle's future "Set up calendar…" menu item
/// can shell out to this exact command instead of duplicating it.
#[cfg(target_os = "macos")]
fn cmd_doctor_setup_calendar() -> Result<()> {
    println!("{} {}", gradient::smooth(), "Calendar".bold().cyan());
    calendar_setup::run()
}

#[cfg(not(target_os = "macos"))]
fn cmd_doctor_setup_calendar() -> Result<()> {
    println!("{} Calendar access is a macOS/EventKit concept; nothing to set up here.", "○".dimmed());
    Ok(())
}

/// Drive the two one-time grants Big Smooth's `imessage` tool needs — Full Disk
/// Access (to read chat.db) and Automation (to send through Messages.app) —
/// pearl th-1665ed. Logic lives in [`imessage_setup`] so the app bundle's future
/// "Set up messages…" menu item can shell out to this exact command instead of
/// duplicating it.
#[cfg(target_os = "macos")]
fn cmd_doctor_setup_imessage() -> Result<()> {
    println!("{} {}", gradient::smooth(), "Messages".bold().cyan());
    imessage_setup::run()
}

#[cfg(not(target_os = "macos"))]
fn cmd_doctor_setup_imessage() -> Result<()> {
    println!("{} iMessage is a macOS-only concept; nothing to set up here.", "○".dimmed());
    Ok(())
}

/// Drive the one-time Reminders (EventKit) grant so Big Smooth's `reminders`
/// tool works (pearl th-94cc4a). A separate grant from Calendar, so a separate
/// flag. Logic lives in [`reminders_setup`] so the app bundle's future
/// "Set up reminders…" menu item can shell out to this exact command.
#[cfg(target_os = "macos")]
fn cmd_doctor_setup_reminders() -> Result<()> {
    println!("{} {}", gradient::smooth(), "Reminders".bold().cyan());
    reminders_setup::run()
}

#[cfg(not(target_os = "macos"))]
fn cmd_doctor_setup_reminders() -> Result<()> {
    println!("{} Reminders access is a macOS/EventKit concept; nothing to set up here.", "○".dimmed());
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
        JiraCommands::Sync { dry_run, pull, push } => cmd_jira_sync(dry_run, pull, push).await,
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

async fn cmd_jira_sync(dry_run: bool, pull: bool, push: bool) -> Result<()> {
    use smooth_diver::jira::{plan_sync, SyncPearl};

    let Some(config) = smooth_diver::jira::JiraConfig::from_env() else {
        anyhow::bail!("Jira not configured. Set JIRA_URL, JIRA_PROJECT, JIRA_EMAIL, JIRA_API_TOKEN env vars.");
    };

    let client = smooth_diver::jira::JiraClient::new(config.clone());
    if !client.check_connection().await {
        anyhow::bail!("Cannot connect to Jira. Check your credentials.");
    }

    let store = open_pearl_store()?;
    println!("{}", "Reconciling pearls ↔ Jira...".bold().cyan());

    let jira_issues = client.list_project_issues().await?;
    let all_pearls = store.list(&smooth_pearls::PearlQuery::new().with_limit(0))?;
    let sync_pearls: Vec<SyncPearl> = all_pearls
        .iter()
        .map(|p| SyncPearl {
            id: p.id.clone(),
            status: p.status.as_str().to_string(),
            title: p.title.clone(),
        })
        .collect();
    let plan = plan_sync(&sync_pearls, &jira_issues, &config.project);

    println!(
        "  {} pearl(s), {} Jira issue(s) → close {} pearl(s), transition {} ticket(s) to Done",
        sync_pearls.len(),
        jira_issues.len(),
        plan.close_pearls.len().to_string().green(),
        plan.transition_keys.len().to_string().green(),
    );
    if !pull && !plan.untracked_jira.is_empty() {
        println!(
            "  {} open Jira ticket(s) have no pearl — pass --pull to create pearls for them",
            plan.untracked_jira.len().to_string().yellow()
        );
    }
    if !push && !plan.unkeyed_pearls.is_empty() {
        println!(
            "  {} active pearl(s) have no Jira key — pass --push to create tickets for them",
            plan.unkeyed_pearls.len().to_string().yellow()
        );
    }

    if dry_run {
        for pearl in &plan.close_pearls {
            println!("  {} would close {} ({})", "◌".cyan(), pearl.id, pearl.title);
        }
        for key in &plan.transition_keys {
            println!("  {} would transition {} → Done", "◌".cyan(), key);
        }
        if pull {
            for key in &plan.untracked_jira {
                println!("  {} would create pearl for {}", "◌".cyan(), key);
            }
        }
        if push {
            for pearl in &plan.unkeyed_pearls {
                println!("  {} would create Jira ticket for {} ({})", "◌".cyan(), pearl.id, pearl.title);
            }
        }
        println!("{}", "dry run — nothing changed".yellow());
        return Ok(());
    }

    // Close pearls whose Jira work is Done.
    let close_ids: Vec<&str> = plan.close_pearls.iter().map(|p| p.id.as_str()).collect();
    let closed = if close_ids.is_empty() { 0 } else { store.close(&close_ids)? };
    for pearl in &plan.close_pearls {
        println!("  {} closed {} ({})", "✓".green(), pearl.id, pearl.title);
    }

    // Transition Jira tickets whose pearls are all closed.
    let mut transitioned = 0u32;
    for key in &plan.transition_keys {
        match client.transition_ticket(key, "done").await {
            Ok(()) => {
                println!("  {} {} → Done", "✓".green(), key);
                transitioned += 1;
            }
            Err(e) => eprintln!("  {} {} transition failed: {e}", "✗".red(), key),
        }
    }

    // Opt-in: Jira → pearls.
    let pulled = if pull {
        jira_sync_pull(&store, &jira_issues, &plan.untracked_jira)
    } else {
        0
    };

    // Opt-in: pearls → Jira.
    let pushed = if push {
        jira_sync_push(&store, &client, &all_pearls, &plan.unkeyed_pearls).await
    } else {
        0
    };

    println!();
    println!(
        "{} pearl(s) closed, {} ticket(s) transitioned, {} pulled, {} pushed",
        closed.to_string().green(),
        transitioned.to_string().green(),
        pulled.to_string().cyan(),
        pushed.to_string().cyan(),
    );

    Ok(())
}

/// Create local pearls for open Jira tickets nothing references (`th jira sync --pull`).
fn jira_sync_pull(store: &smooth_pearls::PearlStore, jira_issues: &[smooth_diver::jira::JiraIssue], untracked: &[String]) -> u32 {
    let mut pulled = 0u32;
    for key in untracked {
        let Some(issue) = jira_issues.iter().find(|i| &i.key == key) else { continue };
        let new = smooth_pearls::NewPearl {
            title: format!("{}: {}", issue.key, issue.summary),
            description: issue.description.clone().unwrap_or_default(),
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
            Err(e) => eprintln!("  {} {} failed: {e}", "✗".red(), key),
        }
    }
    pulled
}

/// Create Jira tickets for active pearls with no issue key (`th jira sync --push`).
async fn jira_sync_push(
    store: &smooth_pearls::PearlStore,
    client: &smooth_diver::jira::JiraClient,
    all_pearls: &[smooth_pearls::Pearl],
    unkeyed: &[smooth_diver::jira::SyncPearl],
) -> u32 {
    let mut pushed = 0u32;
    for sync_pearl in unkeyed {
        let description = all_pearls
            .iter()
            .find(|p| p.id == sync_pearl.id)
            .map(|p| p.description.clone())
            .unwrap_or_default();
        match client.create_ticket(&sync_pearl.title, &description).await {
            Ok(ticket) => {
                let update = smooth_pearls::PearlUpdate {
                    title: Some(format!("{}: {}", ticket.key, sync_pearl.title)),
                    ..Default::default()
                };
                let _ = store.update(&sync_pearl.id, &update);
                println!("  {} {} → {}", "↑".green(), sync_pearl.id, ticket.key);
                pushed += 1;
            }
            Err(e) => eprintln!("  {} {} failed: {e}", "✗".red(), sync_pearl.id),
        }
    }
    pushed
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

/// Best-effort push of the pearl/messaging state to the repo's
/// `refs/dolt/data` remote so other clones/machines on the same repo see
/// it. Pearls and messages both live in the pearl store, which syncs over
/// the repo's git origin via Dolt's own ref — so a mutation that only
/// commits locally won't reach a teammate's clone until a push, and an
/// un-pushed local commit is exactly what a later `th pearls pull` can
/// orphan (pearl th-4a4559). Quiet by design: a missing remote (the global
/// `~/.smooth/dolt`, or a project with no origin) or being offline is a
/// silent no-op — never an error, and never a stray `fatal:` on stderr (we
/// drive only `dolt push`, which captures its own output; the git-side
/// `git_push_pearl_state` inherits git's stderr and is only for the legacy
/// tracked-store model, so it's not used here). Pearls th-bdaaa7 / th-4a4559.
fn sync_push_pearl_state(dolt_dir: &std::path::Path) {
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
        Err(e) => tracing::debug!(error = %e, "pearl push skipped (no remote / offline)"),
    }
}

/// Commit a pearl mutation to the on-disk store/git AND push it to the
/// repo's `refs/dolt/data` remote, so the work is durable the moment it's
/// made — closing the un-pushed window that a later pull/re-clone can drop
/// (pearl th-4a4559). The git commit is propagated (callers `?` it); the
/// push is best-effort + quiet (offline / no-remote is fine). Opt out of
/// the push with `SMOOTH_PEARLS_NO_PUSH=1` (e.g. bulk/scripted creates that
/// push once at the end).
fn commit_and_push_pearl_state(dolt_dir: &std::path::Path, action: &str) -> Result<()> {
    auto_commit_pearl_state(dolt_dir, action)?;
    if std::env::var_os("SMOOTH_PEARLS_NO_PUSH").is_none() {
        sync_push_pearl_state(dolt_dir);
    }
    Ok(())
}

/// How many commits local `main` is ahead of `remotes/origin/main` — i.e.
/// committed locally but not yet on the remote's `refs/dolt/data`. These
/// are exactly the commits a `th pearls pull` could orphan. Returns `None`
/// when it can't be determined (no `origin`, fetch fails, or the remote
/// branch was never fetched) so callers skip the guard rather than wrongly
/// block. Pearl th-4a4559.
fn pearl_local_ahead_count(dolt_dir: &std::path::Path) -> Option<usize> {
    let dolt = smooth_pearls::SmoothDolt::new(dolt_dir).ok()?;
    // Refresh the remote-tracking ref so the comparison is current.
    // Best-effort — a missing/unreachable remote just leaves
    // `remotes/origin/main` stale or absent, handled below.
    let _ = dolt.sql("CALL DOLT_FETCH('origin', 'main')");
    // Commits reachable from local `main` but not from the remote tip.
    let rows = dolt.sql("SELECT COUNT(*) AS n FROM dolt_log('remotes/origin/main..main')").ok()?;
    let n = rows.first().and_then(|r| r["n"].as_u64())?;
    usize::try_from(n).ok()
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

/// Parse a `th pearls schedule` WHEN argument into an absolute UTC instant,
/// relative to `now`. Accepts:
///   - `now`
///   - relative offsets: `+2h`, `30m`, `2d`, `1w`, `90s` (unit s/m/h/d/w; `+` optional)
///   - `tomorrow` (now + 24h)
///   - absolute: `YYYY-MM-DD`, `YYYY-MM-DD HH:MM`, or RFC3339
///
/// ponytail: absolute dates without a zone are read as UTC — good enough for
/// reminders; wire chrono-tz (already a dep) here if wall-clock local time matters.
fn parse_when(input: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let s = input.trim();
    let lower = s.to_lowercase();
    if lower == "now" {
        return Ok(now);
    }
    if lower == "tomorrow" {
        return Ok(now + chrono::Duration::days(1));
    }
    // Relative offset: optional '+', digits, single unit suffix.
    let rel = lower.strip_prefix('+').unwrap_or(&lower);
    if let Some(unit) = rel.chars().last() {
        if "smhdw".contains(unit) && rel.len() > 1 {
            if let Ok(n) = rel[..rel.len() - 1].parse::<i64>() {
                let dur = match unit {
                    's' => chrono::Duration::seconds(n),
                    'm' => chrono::Duration::minutes(n),
                    'h' => chrono::Duration::hours(n),
                    'd' => chrono::Duration::days(n),
                    'w' => chrono::Duration::weeks(n),
                    _ => unreachable!(),
                };
                return Ok(now + dur);
            }
        }
    }
    // Absolute forms.
    if let Ok(dt) = s.parse::<DateTime<Utc>>() {
        return Ok(dt);
    }
    if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        return Ok(ndt.and_utc());
    }
    if let Ok(nd) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(nd.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc());
    }
    anyhow::bail!("could not parse schedule time '{input}' (try +2h, 30m, 2d, 1w, tomorrow, or 2026-07-10 09:00)")
}

fn format_pearl_line(issue: &smooth_pearls::Pearl) -> String {
    let labels_str = if issue.labels.is_empty() {
        String::new()
    } else {
        format!(" [{}]", issue.labels.join(", "))
    };
    let sched_str = match issue.scheduled_at {
        Some(dt) if dt <= Utc::now() => format!(" ⏰ due {}", dt.format("%Y-%m-%d %H:%M")),
        Some(dt) => format!(" ⏰ {}", dt.format("%Y-%m-%d %H:%M")),
        None => String::new(),
    };
    format!(
        "{} {} {} P{} {}{}{}",
        issue.status,
        issue.id.dimmed(),
        "\u{25CF}".dimmed(),
        issue.priority.as_u8(),
        issue.title,
        labels_str.dimmed(),
        sched_str.yellow()
    )
}

#[cfg(test)]
mod schedule_tests {
    use super::*;

    fn base() -> DateTime<Utc> {
        "2026-07-05T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn relative_offsets() {
        let n = base();
        assert_eq!(parse_when("+2h", n).unwrap(), n + chrono::Duration::hours(2));
        assert_eq!(parse_when("30m", n).unwrap(), n + chrono::Duration::minutes(30));
        assert_eq!(parse_when("2d", n).unwrap(), n + chrono::Duration::days(2));
        assert_eq!(parse_when("1w", n).unwrap(), n + chrono::Duration::weeks(1));
        assert_eq!(parse_when("90s", n).unwrap(), n + chrono::Duration::seconds(90));
    }

    #[test]
    fn keywords() {
        let n = base();
        assert_eq!(parse_when("now", n).unwrap(), n);
        assert_eq!(parse_when("TOMORROW", n).unwrap(), n + chrono::Duration::days(1));
    }

    #[test]
    fn absolute_forms() {
        let n = base();
        assert_eq!(
            parse_when("2026-07-10 09:00", n).unwrap(),
            "2026-07-10T09:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(parse_when("2026-07-10", n).unwrap(), "2026-07-10T00:00:00Z".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(
            parse_when("2026-07-10T09:30:00Z", n).unwrap(),
            "2026-07-10T09:30:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_when("whenever", base()).is_err());
        assert!(parse_when("2x", base()).is_err());
    }
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
            commit_and_push_pearl_state(&dolt_dir, &format!("create {} {}", issue.id, truncate_for_msg(&issue.title)))?;
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
            if let Some(dt) = issue.scheduled_at {
                let tag = if dt <= Utc::now() { " (due)" } else { "" };
                println!("  {} {}{tag}", "Scheduled:".dimmed(), dt.format("%Y-%m-%d %H:%M UTC"));
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
            commit_and_push_pearl_state(&dolt_dir, &format!("update {}", updated.id))?;
        }

        PearlCommands::Close { ids } => {
            let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            let count = store.close(&id_refs)?;
            println!("{} Closed {count} issue(s)", "✓".green().bold());
            commit_and_push_pearl_state(&dolt_dir, &format!("close {}", ids.join(", ")))?;
        }

        PearlCommands::Reopen { id } => {
            let issue = store.reopen(&id)?;
            println!("{} Reopened {}", "✓".green().bold(), issue.id);
            println!("  {}", format_pearl_line(&issue));
            commit_and_push_pearl_state(&dolt_dir, &format!("reopen {}", issue.id))?;
        }

        PearlCommands::Schedule { id, when } => {
            let scheduled = match when {
                Some(ref w) => Some(parse_when(w, Utc::now())?),
                None => None,
            };
            let updates = smooth_pearls::PearlUpdate {
                scheduled_at: Some(scheduled),
                ..Default::default()
            };
            let updated = store.update(&id, &updates)?;
            match scheduled {
                Some(dt) => println!("{} Scheduled {} for {}", "✓".green().bold(), updated.id, dt.format("%Y-%m-%d %H:%M UTC")),
                None => println!("{} Cleared schedule on {}", "✓".green().bold(), updated.id),
            }
            commit_and_push_pearl_state(&dolt_dir, &format!("schedule {}", updated.id))?;
        }

        PearlCommands::Due => {
            let issues = store.due_scheduled()?;
            if issues.is_empty() {
                println!("No pearls due.");
            } else {
                println!("{}", "Due Pearls (scheduled time arrived):".bold().yellow());
                for issue in &issues {
                    println!("  {}", format_pearl_line(issue));
                }
                println!("\n{} issue(s)", issues.len());
            }
        }

        PearlCommands::Dep { cmd } => match cmd {
            DepCommands::Add { issue, depends_on } => {
                store.add_dep(&issue, &depends_on)?;
                println!("{} {issue} now depends on {depends_on}", "✓".green().bold());
                commit_and_push_pearl_state(&dolt_dir, &format!("dep add {issue} → {depends_on}"))?;
            }
            DepCommands::Remove { issue, depends_on } => {
                store.remove_dep(&issue, &depends_on)?;
                println!("{} Removed dependency {issue} → {depends_on}", "✓".green().bold());
                commit_and_push_pearl_state(&dolt_dir, &format!("dep remove {issue} → {depends_on}"))?;
            }
        },

        PearlCommands::Comment { id, content } => {
            let comment = store.add_comment(&id, &content)?;
            println!("{} Comment added ({})", "✓".green().bold(), comment.id.dimmed());
            commit_and_push_pearl_state(&dolt_dir, &format!("comment on {id}"))?;
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
                commit_and_push_pearl_state(&dolt_dir, &format!("label add {id} +{label}"))?;
            }
            LabelCommands::Remove { label } => {
                store.remove_label(&id, &label)?;
                println!("{} Removed label \"{label}\" from {id}", "✓".green().bold());
                commit_and_push_pearl_state(&dolt_dir, &format!("label remove {id} -{label}"))?;
            }
        },

        PearlCommands::MigrateFromBeads => {
            cmd_migrate_from_beads(&store)?;
            commit_and_push_pearl_state(&dolt_dir, "migrate from beads")?;
        }

        PearlCommands::Projects => print_registered_projects()?,

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

        PearlCommands::Pull { force } => {
            // Guard against the data-loss footgun: if local `main` carries
            // commits the remote doesn't have, a pull can orphan them
            // (the refs/dolt/data divergence). Refuse by default and tell
            // the user to push first; `--force` opts into the old
            // behaviour. Skipped silently when we can't determine ahead-ness
            // (no remote / fetch fails) so a remote-less store still pulls.
            // Pearl th-4a4559.
            if !force {
                if let Some(ahead) = pearl_local_ahead_count(&dolt_dir) {
                    if ahead > 0 {
                        anyhow::bail!(
                            "Refusing to pull: {ahead} local pearl commit(s) aren't on the remote yet, and \
                             pulling could orphan them.\n  • Recommended: `th pearls push` first, then pull.\n  \
                             • Or `th pearls pull --force` to pull anyway (your local-only commits stay in the \
                             Dolt history and can be recovered, but `main` will move to the remote)."
                        );
                    }
                }
            }
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

        PearlCommands::Doctor {
            auto_repair,
            reap,
            force,
            reap_age_secs,
        } => {
            use smooth_pearls::dolt::{find_store_holders, probe_writable, reap_store_holders, select_remedy, DoctorDiagnosis, HolderKind, Remedy, WriteProbe};

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

            let mut any_bad_remote = false;

            // ── REMOTE URL — the root cause of the wedge ─────────────
            // A `/./`-mangled origin makes git reject the path, so
            // `smooth-dolt push` hangs forever holding the write lock and
            // the whole store goes read-only for every agent. Check and
            // repair this FIRST: reaping a hung push against a still-
            // broken remote just buys time until the next auto-push.
            for db_dir in &db_dirs {
                let cli = smooth_pearls::SmoothDolt::new_cli_only(db_dir)?;
                let Some(fixed) = smooth_pearls::dolt::repair_malformed_remote_url(&cli.origin_url().unwrap_or_default()) else {
                    continue;
                };
                println!("✗ malformed `origin` remote on {}", db_dir.display());
                println!("    the `/./` makes git reject the path — every push hangs holding the write lock,");
                println!("    which is what turns the whole store read-only for every writer.");
                println!("    repaired URL: {fixed}");
                if !auto_repair {
                    println!("    fix: th pearls doctor --auto-repair   (repoints the remote; never touches history)");
                    any_bad_remote = true;
                    continue;
                }
                match cli.repair_origin_remote() {
                    Ok(Some(url)) => println!("  ✓ origin repointed to {url}"),
                    Ok(None) => {}
                    Err(e) => {
                        println!("  ✗ could not repair origin: {e:#}");
                        any_bad_remote = true;
                    }
                }
            }

            // ── PROCESSES holding this store ─────────────────────────
            // The write-lock class (pearl th-118847): a hung `smooth-dolt
            // push` (or a leaked one-shot queued behind it) keeps the
            // store open, so every write fails read-only while every read
            // still works. Doctor used to call that "✓ healthy" and offer
            // only the destructive re-clone. Name the holders first.
            let holders = find_store_holders(&dolt_root);
            if holders.is_empty() {
                println!("smooth-dolt processes holding this store: none");
            } else {
                println!("smooth-dolt processes holding this store: {}", holders.len());
                for h in &holders {
                    let kind = match h.kind {
                        HolderKind::Serve => "serve",
                        HolderKind::Sync => "sync (push/pull)",
                        HolderKind::OneShot => "one-shot",
                        HolderKind::Child => "child of a holder",
                    };
                    println!(
                        "  pid {} [{}] alive {}s: {}",
                        h.pid,
                        kind,
                        h.age_secs,
                        h.cmd.chars().take(100).collect::<String>()
                    );
                }
            }
            println!();

            let mut any_corrupt = false;
            let mut any_failed_repair = false;
            let mut any_write_locked = false;
            let mut healthy_dbs: Vec<std::path::PathBuf> = Vec::new();
            for db_dir in &db_dirs {
                let name = db_dir.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                println!("probing db: {} at {}", name, db_dir.display());
                let diagnosis = smooth_pearls::SmoothDolt::diagnose(db_dir);

                // A cold `log` probe only proves the store READS. Probe
                // writes too — otherwise a write-locked store is reported
                // healthy while `th pearls create` dies.
                let write = matches!(diagnosis, DoctorDiagnosis::Healthy).then(|| probe_writable(db_dir));
                let write_locked = matches!(write, Some(WriteProbe::ReadOnly { .. }));
                let remedy = select_remedy(&diagnosis, write_locked);

                match diagnosis {
                    DoctorDiagnosis::Healthy if write_locked => {
                        println!("  ✓ manifest reads cleanly — the db is NOT corrupt");
                        if holders.is_empty() {
                            println!("  ✗ store is write-locked — writes will fail with \"database is read only\"");
                            println!("    but no smooth-dolt process was found holding it, so there is nothing to reap.");
                            println!("    Something outside this store's own machinery has it open — investigate with:");
                            println!("      lsof {}", db_dir.join(".dolt/noms/LOCK").display());
                        } else if holders.iter().any(|h| h.kind == HolderKind::Sync) {
                            // The real causal chain, named explicitly.
                            println!(
                                "  ✗ store is write-locked by a hung `smooth-dolt push` against a malformed/unreachable remote \
                                 — writes will fail with \"database is read only\""
                            );
                            println!("    → fix the remote, then reap the push. Other leaked processes are queued BEHIND it, not the cause.");
                        } else {
                            println!(
                                "  ✗ store is write-locked by {} leaked smooth-dolt process(es) — writes will fail with \"database is read only\"",
                                holders.len()
                            );
                        }
                        if let Some(WriteProbe::ReadOnly { detail }) = &write {
                            println!("    {detail}");
                        }

                        if remedy != Remedy::Reap || !(reap || auto_repair) {
                            any_write_locked = true;
                            println!("    fix: th pearls doctor --reap");
                            println!("         (add --force to also stop an attached `smooth-dolt serve`)");
                            continue;
                        }

                        // REAP — never a re-clone. The store is healthy;
                        // only the processes pinning it need to go.
                        let (reaped, refused) = reap_store_holders(&dolt_root, reap_age_secs, force);
                        for h in &reaped {
                            println!(
                                "  ✓ reaped pid {} (alive {}s): {}",
                                h.pid,
                                h.age_secs,
                                h.cmd.chars().take(100).collect::<String>()
                            );
                        }
                        for h in &refused {
                            let why = match h.kind {
                                HolderKind::Serve => "a live `smooth-dolt serve` — re-run with --force to stop it".to_string(),
                                HolderKind::Sync | HolderKind::OneShot | HolderKind::Child => {
                                    format!(
                                        "only {}s old (< --reap-age-secs {reap_age_secs}) — may still be working; --force to reap anyway",
                                        h.age_secs
                                    )
                                }
                            };
                            println!("  ○ left alone pid {}: {why}", h.pid);
                        }
                        if reaped.is_empty() {
                            println!("  ✗ nothing eligible to reap — see above");
                        }

                        match probe_writable(db_dir) {
                            WriteProbe::Writable => {
                                println!("  ✓ store accepts writes again");
                                healthy_dbs.push(db_dir.clone());
                            }
                            other => {
                                println!("  ✗ store still refuses writes: {other:?}");
                                any_write_locked = true;
                            }
                        }
                    }
                    DoctorDiagnosis::Healthy => {
                        match &write {
                            Some(WriteProbe::Failed { detail }) => {
                                println!("  ✓ healthy (reads OK)");
                                println!("  ! write probe could not run: {detail}");
                            }
                            _ => println!("  ✓ healthy (reads + writes OK)"),
                        }
                        healthy_dbs.push(db_dir.clone());
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
                            DoctorDiagnosis::Healthy => {
                                println!("  ✓ post-repair probe healthy");
                                healthy_dbs.push(db_dir.clone());
                            }
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
                        // The re-clone is the ONE destructive remedy, and
                        // it is reachable only from here — a store whose
                        // manifest reads cleanly can never land on this
                        // arm, so a healthy-but-write-locked db can never
                        // be re-cloned away (pearl th-118847).
                        if !auto_repair || remedy != Remedy::RecloneFromRemote {
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
                            DoctorDiagnosis::Healthy => {
                                println!("  ✓ post-repair probe healthy");
                                healthy_dbs.push(db_dir.clone());
                            }
                            other => {
                                println!("  ✗ post-repair probe still unhealthy: {other:?}");
                                any_failed_repair = true;
                            }
                        }
                    }
                }
            }

            // REMOTE SYNC — the 2026-07-02 incident class: local store
            // perfectly healthy, but push/pull dead (unset upstream,
            // stray remote re-init, remote-ahead, …). Diagnose-only.
            let any_diverged = doctor_remote_sync(&healthy_dbs);

            if any_corrupt && !auto_repair {
                anyhow::bail!(
                    "one or more dbs are corrupt. Re-run with `--auto-repair` to snapshot + re-clone\n\
                     from the configured `origin` remote for each affected db."
                );
            }
            if any_failed_repair {
                anyhow::bail!("some repairs failed — see output above");
            }
            if any_bad_remote {
                anyhow::bail!(
                    "the dolt `origin` remote is malformed (the `/./` mangling) — git rejects the path, so every\n\
                     `smooth-dolt push` hangs holding the write lock and the store goes read-only for every agent.\n  \
                     • Fix: th pearls doctor --auto-repair   (repoints the remote — it never touches history)"
                );
            }
            if any_write_locked {
                anyhow::bail!(
                    "the pearl store reads fine but refuses WRITES — leaked smooth-dolt process(es) are holding it.\n\
                     `th pearls create` / `th msg send` will keep failing with \"cannot update manifest: database is read only\".\n  \
                     • Fix: th pearls doctor --reap\n  \
                     • If a `smooth-dolt serve` is holding it: th pearls doctor --reap --force\n\
                     This does NOT re-clone — your local pearl history is intact."
                );
            }
            if any_diverged {
                anyhow::bail!(
                    "local and remote pearl histories have diverged — push AND pull are deadlocked.\n\
                     See the remote sync section above for the recommended fix."
                );
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

/// `th pearls doctor` — REMOTE SYNC section. Read-only diagnosis of the
/// local↔remote `refs/dolt/data` relationship. A cheap tip-level check
/// runs first (see [`smooth_pearls::dolt::classify_tip_check`]) — when
/// it proves in-sync, no clone happens. Otherwise it temp-clones the
/// remote (bounded, see [`smooth_pearls::dolt::clone_from_bounded`]),
/// compares bounded logs via
/// [`smooth_pearls::dolt::classify_remote_sync`], and reports whether
/// the branch upstream is configured (an unset upstream makes a bare
/// push fail with `remote '' not found`).
///
/// Returns whether any db is diverged (no common ancestor with the
/// remote) — the push/pull-deadlock class the doctor previously missed
/// (2026-07-02 incident: remote stray-re-initialized with a single bare
/// "Initialize data repository" commit while the local store held 2547
/// commits; push refused as diverged, pull refused by the data-loss
/// guard, and doctor said nothing).
fn doctor_remote_sync(healthy_dbs: &[std::path::PathBuf]) -> bool {
    use smooth_pearls::dolt::{
        branch_hash, classify_remote_sync, classify_tip_check, clone_from_bounded, last_synced_dolt_data_tip, remote_dolt_data_tip, RemoteSyncStatus, TipCheck,
    };

    println!();
    println!("remote sync:");
    if healthy_dbs.is_empty() {
        println!("  - skipped (no healthy local db to compare against)");
        return false;
    }
    // Probe the primary `pearls` db for remote config — every db under
    // one root shares the same git remote (refs/dolt/data).
    let probe = healthy_dbs
        .iter()
        .find(|d| d.file_name().and_then(|n| n.to_str()) == Some("pearls"))
        .unwrap_or(&healthy_dbs[0]);

    let remotes = match smooth_pearls::SmoothDolt::new_cli_only(probe).and_then(|d| d.remote_list()) {
        Ok(out) => out,
        Err(e) => {
            println!("  ✗ couldn't list remotes: {e:#}");
            return false;
        }
    };
    // smooth-dolt prints one `name<TAB>url` per line; prefer `origin`.
    let remote = remotes
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            Some((parts.next()?, parts.next()?))
        })
        .max_by_key(|(name, _)| *name == "origin");
    let Some((remote_name, remote_url)) = remote else {
        println!("  - no remote configured — nothing to sync with (add one: th pearls remote add origin <url>)");
        return false;
    };
    println!("  remote: {remote_name} {remote_url}");

    // Upstream check — cheap repo_state.json read. Plain `th pearls
    // push` auto-repairs a missing upstream via its `-u` retry (PR #123),
    // so this is informational.
    match pearl_upstream_remote(probe) {
        Some(up) => println!("  ✓ branch upstream configured ({up})"),
        None => {
            println!("  ! branch upstream not set — a bare dolt push fails with `remote '' not found`.");
            println!("    Plain `th pearls push` auto-repairs this (retries with -u).");
        }
    }

    // Tip-level check FIRST (pearl th-c42cc4). The deep probe below
    // clones the full remote refs/dolt/data — measured ~5 minutes at 96%
    // CPU on a 2547-commit store, which always exceeds the default 30s
    // sync bound, so on large stores the doctor used to skip the
    // comparison entirely. Four cheap signals answer the common case
    // without any clone: local dolt head vs remote-tracking head (no
    // unpushed commits?) and last-synced git tip vs `git ls-remote`
    // (remote ref unmoved?). Anything short of a clean "in sync" falls
    // through to the deep probe unchanged.
    let remote_tip = match remote_dolt_data_tip(remote_url) {
        Ok(tip) => tip,
        Err(e) => {
            println!("  ! tip check skipped — git ls-remote failed: {e:#}");
            None
        }
    };
    let tip_verdicts: Vec<(String, TipCheck)> = healthy_dbs
        .iter()
        .map(|db_dir| {
            let name = db_dir.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
            let (local, tracking) = smooth_pearls::SmoothDolt::new_cli_only(db_dir).map_or((None, None), |dolt| {
                let head = |query: &str, branch: &str| dolt.sql(query).ok().and_then(|rows| branch_hash(&rows, branch));
                (
                    head("select name, hash from dolt_branches", "main"),
                    head("select name, hash from dolt_remote_branches", "remotes/origin/main"),
                )
            });
            let last_synced = last_synced_dolt_data_tip(db_dir);
            let verdict = classify_tip_check(local.as_deref(), tracking.as_deref(), last_synced.as_deref(), remote_tip.as_deref());
            (name, verdict)
        })
        .collect();
    if tip_verdicts.iter().all(|(_, v)| *v == TipCheck::InSync) {
        for (name, _) in &tip_verdicts {
            println!("  ✓ {name}: in sync with remote (tip-level check — no local commits since last sync, remote ref unmoved)");
        }
        return false;
    }
    for (name, verdict) in &tip_verdicts {
        match verdict {
            TipCheck::InSync => {}
            TipCheck::LocalMoved => println!("  … {name}: tip check found local commits since the last sync — running the deep probe to classify"),
            TipCheck::RemoteMoved => println!("  … {name}: tip check found the remote ref moved since the last sync — running the deep probe to classify"),
            TipCheck::Unknown => println!("  … {name}: tip check inconclusive (missing sync marker or remote ref) — running the deep probe to classify"),
        }
    }

    // Bounded temp clone of the remote's refs/dolt/data.
    let tmp = match tempfile::TempDir::new() {
        Ok(t) => t,
        Err(e) => {
            println!("  ✗ couldn't create temp dir for remote clone: {e}");
            return false;
        }
    };
    let clone_root = tmp.path().join("remote");
    if let Err(e) = clone_from_bounded(remote_url, &clone_root) {
        // A deadline hit is NOT "unreachable" — a full clone of a large
        // store is legitimately minutes of CPU (measured ~5min for a
        // 2547-commit history; pearl th-6c6843). Say what actually
        // happened and how to get the diagnosis anyway.
        if smooth_pearls::dolt::is_sync_timeout_err(&e) {
            println!("  ! remote comparison skipped — the probe clone exceeded its time bound: {e:#}");
            println!("    A large store can take minutes to clone. Re-run with a bigger bound, e.g.:");
            println!("    SMOOTH_DOLT_SYNC_TIMEOUT_SECS=600 th pearls doctor");
        } else {
            println!("  ✗ remote unreachable — clone of {remote_url} failed: {e:#}");
        }
        return false;
    }

    // Compare histories per db. `log` is bounded, so the classification
    // is a heuristic over the last 500 commits on each side.
    let bounded_log = |dir: &std::path::Path| -> Result<Vec<String>> {
        let entries = smooth_pearls::SmoothDolt::new_cli_only(dir)?.log(500)?;
        Ok(entries.into_iter().map(|(line, ..)| line).collect())
    };
    let mut any_diverged = false;
    for db_dir in healthy_dbs {
        let name = db_dir.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let remote_db = clone_root.join(name);
        if !remote_db.join(".dolt").is_dir() {
            println!("  ! {name}: remote has no `{name}` db — never pushed? run: th pearls push");
            continue;
        }
        let (local, remote) = match (bounded_log(db_dir), bounded_log(&remote_db)) {
            (Ok(l), Ok(r)) => (l, r),
            (Err(e), _) => {
                println!("  ✗ {name}: couldn't read local log: {e:#}");
                continue;
            }
            (_, Err(e)) => {
                println!("  ✗ {name}: couldn't read remote log: {e:#}");
                continue;
            }
        };
        match classify_remote_sync(&local, &remote) {
            RemoteSyncStatus::InSync => println!("  ✓ {name}: in sync with remote"),
            RemoteSyncStatus::LocalAhead => {
                println!("  → {name}: local is ahead of the remote (remote tip found in local history within the last 500 commits) — run: th pearls push");
            }
            RemoteSyncStatus::RemoteAhead => {
                println!("  ← {name}: remote is ahead of local (local tip found in remote history within the last 500 commits) — run: th pearls pull");
            }
            RemoteSyncStatus::DivergedBareInit => {
                any_diverged = true;
                println!("  ✗ {name}: DIVERGED — the remote refs/dolt/data has exactly ONE commit (\"Initialize data repository\")");
                println!(
                    "    sharing no ancestor with the {} local commits. This is a stray re-init of the remote ref:",
                    local.len()
                );
                println!("    push is refused (diverged) and pull is refused (data-loss guard).");
                println!("    `th pearls push --force` would overwrite ONLY that bare init commit — recommended.");
            }
            RemoteSyncStatus::Diverged => {
                any_diverged = true;
                println!("  ✗ {name}: DIVERGED — no common ancestor with the remote within the last 500 commits,");
                println!("    and the remote has real commits. Inspect before any force:");
                println!("    smooth-dolt clone {remote_url} /tmp/check && smooth-dolt log /tmp/check/{name}");
            }
            RemoteSyncStatus::EmptyRemote => println!("  ! {name}: remote history is empty — run: th pearls push"),
            RemoteSyncStatus::EmptyLocal => println!("  ! {name}: local history is empty — run: th pearls pull"),
        }
    }
    any_diverged
}

/// Cheap upstream detection for the doctor: dolt records the branch
/// upstream in `.dolt/repo_state.json` under `branches.<name>.remote`.
/// `None` when the file/field is missing or the remote is empty —
/// exactly the state that makes a bare `CALL DOLT_PUSH()` resolve the
/// remote name to `''`.
fn pearl_upstream_remote(db_dir: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(db_dir.join(".dolt").join("repo_state.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("branches")?.as_object()?.iter().find_map(|(branch, b)| {
        let remote = b.get("remote")?.as_str()?;
        if remote.is_empty() {
            None
        } else {
            Some(format!("{branch} → {remote}"))
        }
    })
}

#[cfg(test)]
mod pearl_upstream_remote_tests {
    use super::pearl_upstream_remote;

    fn write_repo_state(json: &str) -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        let dolt = tmp.path().join(".dolt");
        std::fs::create_dir_all(&dolt).unwrap();
        std::fs::write(dolt.join("repo_state.json"), json).unwrap();
        tmp
    }

    #[test]
    fn detects_configured_upstream() {
        let tmp = write_repo_state(r#"{"head":"refs/heads/main","branches":{"main":{"head":"refs/heads/main","remote":"origin"}}}"#);
        assert_eq!(pearl_upstream_remote(tmp.path()), Some("main → origin".to_string()));
    }

    #[test]
    fn missing_branches_key_is_none() {
        let tmp = write_repo_state(r#"{"head":"refs/heads/main","remotes":{}}"#);
        assert_eq!(pearl_upstream_remote(tmp.path()), None);
    }

    #[test]
    fn empty_remote_is_none() {
        let tmp = write_repo_state(r#"{"branches":{"main":{"head":"refs/heads/main","remote":""}}}"#);
        assert_eq!(pearl_upstream_remote(tmp.path()), None);
    }

    #[test]
    fn missing_file_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(pearl_upstream_remote(tmp.path()), None);
    }
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

    // Install git hooks if not already present. `install` itself refuses to
    // clobber a foreign `core.hooksPath` (husky etc.) — pearl th-9550e6.
    let hooks_status = hooks::check(None);
    if !hooks_status.is_ok() {
        println!();
        match hooks::install(None) {
            Ok(outcome) => hooks::print_install_outcome(&outcome),
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
You can talk to every other agent on this machine — other sessions, other\n\
harnesses, other repos — through a shared mailbox at `~/.smooth/mail.db`. It's\n\
all plain `th` calls, so it works the same whether you're Claude Code, opencode,\n\
pi, or a shell loop.\n\
\n\
**On session start:**\n\
```bash\n\
th agent register --name <your-handle>     # idempotent; pick a stable name\n\
th agent whoami                            # what handle am I, what's waiting\n\
```\n\
\n\
**Continuously check for messages** (do this every few turns, or run it in the\n\
background of your session):\n\
```bash\n\
th msg inbox --unread           # what's waiting for me\n\
th msg watch                    # blocking poll loop — prints messages as they land\n\
th msg watch --once --json      # block until mail arrives, print it, exit\n\
th msg ack --all                # done with them (per-recipient: only your copy)\n\
```\n\
\n\
**Send / reply:**\n\
```bash\n\
th agent list                   # who can I reach (presence, branch, current task)\n\
th msg send <name|all> \"…\" [--type request|result|handoff|cancel] [--priority N]\n\
th msg reply <message-id> --body \"…\"   # threads automatically\n\
th msg thread <message-id>      # read a whole conversation\n\
th agent status --status working --task \"…\"   # tell others what you're up to\n\
```\n\
\n\
Identity resolves `$SMOOTH_AGENT_HANDLE` → `$SMOOTH_AGENT` → this session's\n\
recorded handle → `user@host`; set `$SMOOTH_HARNESS` so others can see what tool\n\
you are, and `th agent claim <handle>` to take a durable name (your mail comes\n\
with it). The mailbox is machine-local, so there is nothing to push or pull —\n\
`--no-push`/`--pull` still parse but do nothing.\n\
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
///
/// `remote '' not found` (pearl th-2681fd) is the same condition with a
/// newer Dolt error string: the branch's upstream remote is empty, so a
/// bare `CALL DOLT_PUSH()` resolves the remote name to `''`. Note the
/// quoted-empty form does NOT overlap with [`is_no_remote_error`]'s
/// `remote not found` (that one matches a *named* missing remote).
fn is_no_upstream_error(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    s.contains("no upstream branch") || s.contains("has no upstream") || s.contains("remote '' not found")
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

#[cfg(test)]
mod push_error_predicate_tests {
    use super::*;

    fn err(msg: &str) -> anyhow::Error {
        anyhow::anyhow!("smooth-dolt push failed (exit 1): smooth-dolt: push: {msg}")
    }

    // Regression for pearl th-2681fd: newer Dolt reports a missing branch
    // upstream as `remote '' not found`, which must trigger the `-u` retry
    // — and must NOT be classified as "no remote at all" (the global-store
    // skip), which only matches a *named* missing remote.
    #[test]
    fn empty_remote_is_no_upstream_not_no_remote() {
        let e = err("Error 1105: fatal: remote '' not found.");
        assert!(is_no_upstream_error(&e));
        assert!(!is_no_remote_error(&e));
    }

    #[test]
    fn classic_no_upstream_strings_still_match() {
        assert!(is_no_upstream_error(&err("no upstream branch")));
        assert!(is_no_upstream_error(&err("branch has no upstream")));
    }

    #[test]
    fn named_missing_remote_is_no_remote_not_no_upstream() {
        let e = err("fatal: remote not found: origin");
        assert!(is_no_remote_error(&e));
        assert!(!is_no_upstream_error(&e));
    }

    #[test]
    fn divergence_matches_neither() {
        let e = err("hint: Integrate the remote changes (e.g. 'dolt pull ...') before pushing again.");
        assert!(!is_no_upstream_error(&e));
        assert!(!is_no_remote_error(&e));
        assert!(!is_no_common_ancestor_error(&e));
    }
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
            // ponytail: tailscale status surfaced via the daemon now — this
            // stubs the no-tailscale (disconnected) branch the printing code
            // already handled. smooth-cli does not depend on smooth-daemon.
            println!("Tailscale: disconnected");
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
                println!("  {} No providers configured. Run: th model login", "✗".red().bold());
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
                println!("  {} No providers configured. Run: th model login", "✗".red().bold());
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
                println!("  {} No providers configured. Run: th model login", "✗".red().bold());
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

// ── `th providers` — bring-your-own LLM providers ──────────────────

/// Local inference servers probed by `th providers detect`. OpenAI-
/// compatible `/v1` in every case: Ollama on 11434, LM Studio on 1234.
const LOCAL_PROBE_TARGETS: &[(&str, &str)] = &[("ollama", "http://localhost:11434/v1"), ("lmstudio", "http://localhost:1234/v1")];

fn providers_json_path() -> Result<std::path::PathBuf> {
    dirs_next::home_dir()
        .map(|h| h.join(".smooth/providers.json"))
        .context("cannot determine home directory")
}

async fn cmd_providers(cmd: ProvidersCommands) -> Result<()> {
    match cmd {
        ProvidersCommands::Add {
            id,
            url,
            api_key,
            format,
            model,
            max_tokens,
        } => cmd_providers_add(&id, &url, api_key, format, model, max_tokens),
        ProvidersCommands::List { json } => cmd_providers_list(json),
        ProvidersCommands::Remove { id } => cmd_providers_remove(&id),
        ProvidersCommands::Detect { yes, json } => {
            // `reqwest::blocking` panics if dropped in a runtime context.
            tokio::task::spawn_blocking(move || cmd_providers_detect(yes, json))
                .await
                .context("providers detect task panicked")?
        }
    }
}

fn cmd_providers_add(id: &str, url: &str, api_key: Option<String>, format: Option<String>, model: Option<String>, max_tokens: Option<u32>) -> Result<()> {
    let path = providers_json_path()?;
    let mut root = smooth_cast::providers::load_value(&path)?;
    let updated = smooth_cast::providers::upsert_provider(
        &mut root,
        &smooth_cast::providers::NewProvider {
            id: id.to_string(),
            api_url: url.to_string(),
            api_key,
            api_format: format,
            default_model: model,
            max_tokens,
        },
    );
    smooth_cast::providers::save_value(&path, &root)?;
    let verb = if updated { "updated" } else { "added" };
    println!("  {} provider {} {}", verb.green().bold(), id.bold(), url.dimmed());
    Ok(())
}

fn cmd_providers_list(json: bool) -> Result<()> {
    let path = providers_json_path()?;
    let root = smooth_cast::providers::load_value(&path)?;
    let providers = smooth_cast::providers::list_providers(&root);

    if json {
        let arr: Vec<_> = providers
            .iter()
            .map(|p| serde_json::json!({ "id": p.id, "api_url": p.api_url, "default_model": p.default_model, "max_tokens": p.max_tokens, "local": p.local }))
            .collect();
        println!("{}", serde_json::to_string(&serde_json::json!({ "providers": arr }))?);
        return Ok(());
    }

    println!();
    println!("  {} {}", gradient::smooth(), "providers".bold());
    println!();
    if providers.is_empty() {
        println!("  {} \u{2014} add one with {}", "no providers configured".yellow(), "th providers add".cyan());
    } else {
        for p in &providers {
            let tag = if p.local { format!(" {}", "[local]".cyan()) } else { String::new() };
            let mt = p.max_tokens.map(|m| format!("  max_tokens={m}")).unwrap_or_default();
            println!("  {}{}  {}{}", p.id.bold(), tag, p.api_url.dimmed(), mt.dimmed());
            if !p.default_model.is_empty() {
                println!("      {} {}", "default_model".dimmed(), p.default_model);
            }
        }
    }
    println!();
    Ok(())
}

fn cmd_providers_remove(id: &str) -> Result<()> {
    let path = providers_json_path()?;
    let mut root = smooth_cast::providers::load_value(&path)?;
    if smooth_cast::providers::remove_provider(&mut root, id) {
        smooth_cast::providers::save_value(&path, &root)?;
        println!("  {} provider {}", "removed".green().bold(), id.bold());
    } else {
        println!("  {} no provider with id {}", "!".yellow().bold(), id.bold());
    }
    Ok(())
}

/// A local server that answered the `/v1/models` probe.
struct DetectedServer {
    id: String,
    api_url: String,
    models: Vec<String>,
}

/// Probe [`LOCAL_PROBE_TARGETS`] and return the ones that answer with a
/// parseable model list. A refused connection returns immediately; a
/// live-but-slow server is bounded by the 2s client timeout.
fn probe_local_servers() -> Vec<DetectedServer> {
    let Ok(client) = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(2)).build() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (id, url) in LOCAL_PROBE_TARGETS {
        let models_url = models_url_for(url);
        if let Ok(resp) = client.get(&models_url).send() {
            if resp.status().is_success() {
                let body = resp.text().unwrap_or_default();
                let (strict, lossy) = parse_models_response(&body);
                let models = if strict.is_empty() { lossy } else { strict };
                found.push(DetectedServer {
                    id: (*id).to_string(),
                    api_url: (*url).to_string(),
                    models,
                });
            }
        }
    }
    found
}

fn cmd_providers_detect(yes: bool, json: bool) -> Result<()> {
    let found = probe_local_servers();

    if json {
        let arr: Vec<_> = found
            .iter()
            .map(|s| serde_json::json!({ "id": s.id, "api_url": s.api_url, "models": s.models }))
            .collect();
        println!("{}", serde_json::to_string(&serde_json::json!({ "detected": arr }))?);
    } else {
        println!();
        println!("  {} {}", gradient::smooth(), "providers \u{00b7} detect".bold());
        println!();
        if found.is_empty() {
            println!("  {}", "no local inference server found on :11434 (Ollama) or :1234 (LM Studio)".yellow());
            println!("  {}", "start one, or add a custom server with `th providers add`".dimmed());
            println!();
            return Ok(());
        }
        for s in &found {
            println!("  {} {}  {} models", s.id.bold(), s.api_url.dimmed(), s.models.len().to_string().cyan());
        }
        println!();
    }

    if yes {
        let path = providers_json_path()?;
        let mut root = smooth_cast::providers::load_value(&path)?;
        for s in &found {
            smooth_cast::providers::upsert_provider(
                &mut root,
                &smooth_cast::providers::NewProvider {
                    id: s.id.clone(),
                    api_url: s.api_url.clone(),
                    api_key: None,
                    api_format: None,
                    default_model: s.models.first().cloned(),
                    max_tokens: None,
                },
            );
        }
        if !found.is_empty() {
            smooth_cast::providers::save_value(&path, &root)?;
            if !json {
                for s in &found {
                    println!("  {} {} {}", "added".green().bold(), s.id.bold(), s.api_url.dimmed());
                }
                println!();
            }
        }
    } else if !json && !found.is_empty() {
        println!("  {} `th providers detect --yes` to add {}", "\u{2192}".dimmed(), "all of the above".dimmed());
        println!();
    }

    Ok(())
}

// ── `th cast` — inspect the LLM cast ───────────────────────────────

/// `th cast models` — list live model groups from the configured
/// provider's `GET /v1/models` endpoint. Pearl th-2b5f63.
async fn cmd_cast(cmd: CastCommands) -> Result<()> {
    match cmd {
        CastCommands::Models { provider, json, filter } => {
            // Extension-registered providers (SEP Phase 7) are collected on the
            // async runtime (loading them handshakes subprocesses); the HTTP
            // listing itself then runs on a blocking thread. Skipped with zero
            // cost when no global extensions are installed.
            let ext_providers = collect_extension_providers().await;
            // `cmd_cast_models` uses `reqwest::blocking`, which panics
            // if dropped inside a tokio runtime context. Hop onto a
            // dedicated blocking thread to keep the runtime happy.
            tokio::task::spawn_blocking(move || cmd_cast_models(provider.as_deref(), json, filter.as_deref(), &ext_providers))
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
fn cmd_cast_models(provider_override: Option<&str>, json_out: bool, filter: Option<&str>, ext_providers: &[(String, Vec<String>)]) -> Result<()> {
    let providers_path = dirs_next::home_dir()
        .map(|h| h.join(".smooth/providers.json"))
        .context("cannot determine home directory")?;

    if !providers_path.exists() {
        eprintln!("not authed \u{2014} run th model login");
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
            eprintln!("not authed \u{2014} run th model login");
            std::process::exit(2);
        }
    };

    let Some(config) = registry.get_provider(&provider_id) else {
        eprintln!("provider '{provider_id}' not configured \u{2014} run th model login");
        std::process::exit(2);
    };

    if config.api_key.is_empty() && provider_id != "ollama" {
        eprintln!("not authed \u{2014} run th model login");
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

    // Fold in live models from any *other* configured local provider
    // (Ollama, LM Studio, …), unless the user pinned a specific
    // `--provider`. Unreachable servers are skipped silently. Pearl
    // th-f4a0fb.
    let local_extra = if provider_override.is_none() {
        probe_configured_local_providers(&registry, &provider_id, filter)
    } else {
        Vec::new()
    };

    // Extension-registered providers (SEP Phase 7): declared models, filtered +
    // sorted like the rest. `ext` prefixes each id (`<provider>/<model>`) so an
    // extension model never collides with a gateway one in the flat JSON list.
    let ext_extra = fold_extension_models(ext_providers, filter);

    if json_out {
        // Stable shape: `{"data": [{"id": "..."}]}` — primary provider
        // first, then each local provider's live models, then extension models.
        let mut data: Vec<_> = chosen.iter().map(|id| serde_json::json!({ "id": id })).collect();
        for (_pid, models) in &local_extra {
            data.extend(models.iter().map(|id| serde_json::json!({ "id": id })));
        }
        for (provider, models) in &ext_extra {
            data.extend(models.iter().map(|id| serde_json::json!({ "id": format!("{provider}/{id}") })));
        }
        println!("{}", serde_json::to_string(&serde_json::json!({ "data": data }))?);
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

    // Local providers, each under its own labeled section.
    for (pid, models) in &local_extra {
        println!();
        println!("  {} {}", "local".cyan().bold(), pid.bold());
        if models.is_empty() {
            println!("  {}", "no models returned".yellow());
        } else {
            for id in models {
                println!("  {id}");
            }
        }
        println!("  {} models", models.len().to_string().cyan().bold());
    }

    // Extension-registered providers, each under its own labeled section.
    for (provider, models) in &ext_extra {
        println!();
        println!("  {} {}", "extension".magenta().bold(), provider.bold());
        for id in models {
            println!("  {id}");
        }
        println!("  {} models", models.len().to_string().cyan().bold());
    }
    println!();

    Ok(())
}

/// Fold extension-registered providers into the model listing: apply the same
/// `filter` + sort as gateway/local models, and drop any provider left with no
/// matching models. Pure so it can be unit-tested without spawning extensions.
fn fold_extension_models(ext_providers: &[(String, Vec<String>)], filter: Option<&str>) -> Vec<(String, Vec<String>)> {
    ext_providers
        .iter()
        .filter_map(|(provider, models)| {
            let models = filter_and_sort(models.clone(), filter);
            if models.is_empty() {
                None
            } else {
                Some((provider.clone(), models))
            }
        })
        .collect()
}

/// Load global extensions headlessly and collect the providers they register
/// (SEP Phase 7). Returns `(provider_label, model_ids)` per registered provider,
/// where the label is `<extension>.<provider>`. Only GLOBAL extensions (from
/// `~/.smooth/extensions/`) are loaded — never project extensions — so a plain
/// `th cast models` in a repo can't be made to spawn an untrusted project
/// extension. Any failure yields an empty list: extension providers are additive
/// and must never break the core listing.
async fn collect_extension_providers() -> Vec<(String, Vec<String>)> {
    use smooth_operator::extension::manifest::default_global_dir;
    use smooth_operator::extension::protocol::{HostInfo, WorkspaceInfo};
    use smooth_operator::extension::{discover, DefaultHostDelegate, ExtensionHost};

    let global = default_global_dir();
    let (discovered, _failures) = discover(global.as_deref(), None);
    if discovered.is_empty() {
        return Vec::new();
    }

    let host_info = HostInfo {
        name: "th".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    // `trusted: false` + no project dir ⇒ only global extensions load.
    let workspace = WorkspaceInfo {
        root: String::new(),
        trusted: false,
    };
    let (host, _load_failures) = ExtensionHost::load(
        discovered,
        host_info,
        workspace,
        "headless",
        Vec::new(),
        std::sync::Arc::new(DefaultHostDelegate),
    )
    .await;
    let providers = host
        .providers()
        .into_iter()
        .map(|(ext, reg)| (format!("{ext}.{}", reg.name), reg.models.into_iter().map(|m| m.id).collect()))
        .collect();
    host.shutdown_all().await;
    providers
}

/// GET `/v1/models` for every *configured* local provider except
/// `skip_id`, tolerating unreachable servers (2s timeout, errors and
/// non-2xx skipped). Returns `(provider_id, sorted+filtered ids)` for
/// each that answered. Folds local models into `th cast models`. Pearl
/// th-f4a0fb.
fn probe_configured_local_providers(
    registry: &smooth_operator::providers::ProviderRegistry,
    skip_id: &str,
    filter: Option<&str>,
) -> Vec<(String, Vec<String>)> {
    let Ok(client) = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(2)).build() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pid in registry.list_providers() {
        if pid == skip_id {
            continue;
        }
        let Some(config) = registry.get_provider(pid) else { continue };
        if !smooth_cast::providers::is_local_url(&config.api_url) {
            continue;
        }
        let url = models_url_for(&config.api_url);
        let mut req = client.get(&url);
        if !config.api_key.is_empty() {
            req = req.bearer_auth(&config.api_key);
        }
        if let Ok(resp) = req.send() {
            if resp.status().is_success() {
                let body = resp.text().unwrap_or_default();
                let (strict, lossy) = parse_models_response(&body);
                let ids = if strict.is_empty() { lossy } else { strict };
                out.push((pid.to_string(), filter_and_sort(ids, filter)));
            }
        }
    }
    out
}

#[cfg(test)]
mod cast_models_tests {
    use super::{extract_model_ids_lossy, filter_and_sort, fold_extension_models, models_url_for, parse_models_response, strip_control_chars};

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
    fn fold_extension_models_filters_sorts_and_drops_empty() {
        let ext = vec![
            ("corp.corporate-proxy".to_string(), vec!["corp-gpt-4o".to_string(), "corp-fast".to_string()]),
            ("other.thing".to_string(), vec!["unrelated".to_string()]),
        ];
        // No filter: both providers kept, models sorted.
        let all = fold_extension_models(&ext, None);
        assert_eq!(all.len(), 2);
        assert_eq!(
            all[0],
            ("corp.corporate-proxy".to_string(), vec!["corp-fast".to_string(), "corp-gpt-4o".to_string()])
        );

        // Filter to `corp`: the second provider has no match and is dropped.
        let filtered = fold_extension_models(&ext, Some("corp"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, "corp.corporate-proxy");
        assert_eq!(filtered[0].1, vec!["corp-fast".to_string(), "corp-gpt-4o".to_string()]);
    }

    #[test]
    fn fold_extension_models_empty_input_is_empty() {
        assert!(fold_extension_models(&[], None).is_empty());
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

    #[test]
    fn detect_parses_ollama_models_body() {
        // `th providers detect` feeds the local server's /v1/models
        // response through `parse_models_response`; the first id becomes
        // the added provider's default_model. Guard that path on the
        // exact shape Ollama returns.
        let body = r#"{"object":"list","data":[{"id":"llama3.3","object":"model"},{"id":"qwen2.5-coder:7b","object":"model"}]}"#;
        let (strict, lossy) = parse_models_response(body);
        assert_eq!(strict, vec!["llama3.3".to_string(), "qwen2.5-coder:7b".to_string()]);
        // `--yes` picks the first reported model as the default.
        let chosen = if strict.is_empty() { lossy } else { strict };
        assert_eq!(chosen.first().map(String::as_str), Some("llama3.3"));
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

        McpCommands::Install { name, harness, dry_run } => {
            use mcp_config::{default_mcp_servers, ensure_default_mcp_servers, host_probe_on_path, DefaultOutcome, McpConfig, McpServerConfig};

            // `--harness` is the inverse operation: register THIS binary with a
            // coding harness, rather than registering a third-party server with
            // the operator.
            if let Some(ref h) = harness {
                return cmd_mcp_install_harness(h, dry_run);
            }
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
        // Handled in the async dispatch (it runs the MCP server); never reaches
        // this sync path.
        McpCommands::Serve => unreachable!("`th mcp serve` is dispatched asynchronously"),
    }
}

/// `th mcp install --harness <claude-code|codex|opencode|all>` — register
/// `th mcp serve` with a coding harness so its sessions join the agent bus.
fn cmd_mcp_install_harness(spec: &str, dry_run: bool) -> Result<()> {
    use mcp_install::{harness_home, install_into, Harness, Outcome};

    let all = spec.trim().eq_ignore_ascii_case("all");
    let targets: Vec<Harness> = if all { Harness::ALL.to_vec() } else { vec![Harness::parse(spec)?] };
    let home = harness_home()?;

    println!();
    if dry_run {
        println!("  {} nothing will be written\n", "dry run —".yellow().bold());
    }
    let mut wrote = 0;
    for h in targets {
        let path = h.config_path(&home);
        let outcome = install_into(h, &home, dry_run)?;
        let line = match &outcome {
            Outcome::Added => format!("  {} {} — registered `th mcp serve`", "✓".green().bold(), h.to_string().bold()),
            Outcome::Updated => format!("  {} {} — repointed at `th mcp serve`", "✓".green().bold(), h.to_string().bold()),
            Outcome::AlreadyPresent => format!("  {} {} — already registered", "·".dimmed(), h.to_string().bold()),
            // Only worth calling out when the user asked for everything; if
            // they named one harness explicitly, say it plainly instead.
            Outcome::NotInstalled if all => format!("  {} {} — not installed here, skipped", "○".dimmed(), h.to_string().dimmed()),
            Outcome::NotInstalled => format!(
                "  {} {} is not installed here (no {})",
                "!".yellow().bold(),
                h.to_string().bold(),
                h.marker_dir(&home).display()
            ),
        };
        println!("{line}");
        if !matches!(outcome, Outcome::NotInstalled) {
            println!("    {}", path.display().to_string().dimmed());
        }
        if outcome.wrote() {
            wrote += 1;
        }
    }
    if wrote > 0 && !dry_run {
        println!("\n  {} restart the harness to pick up the new server.", "→".dimmed());
    }
    println!();
    Ok(())
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
                    println!("  {} plugin_{:<14} {}  {}", marker, n.bold(), desc.dimmed(), tag);
                }
            }

            if !project_plugins.is_empty() {
                if let Some(ref pd) = project_dir {
                    println!("\n  {} {}\n", "Project".cyan().bold(), format!("({})", pd.display()).dimmed());
                }
                for (n, desc) in &project_plugins {
                    println!("  {} plugin_{:<14} {}  {}", "✓".green().bold(), n.bold(), desc.dimmed(), "[project]".dimmed());
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
        // Scheduled pearls that have come due "speak up" first (pearl th-01aa6a).
        prime_pearls_section(&exe, "due", "\u{23F0} Scheduled & due", 20);
        prime_pearls_section(&exe, "ready", "Ready to work", 40);
    }

    Ok(())
}

/// Run `th pearls <sub>` and, if it produced output, print it as a fenced
/// markdown section under `heading` (capped to `cap` lines). Best-effort:
/// a missing/empty store just skips the section.
fn prime_pearls_section(exe: &std::path::Path, sub: &str, heading: &str, cap: usize) {
    let Ok(out) = std::process::Command::new(exe)
        .args(["pearls", sub])
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let trimmed = s.trim();
    // "No pearls due." / "No ready issues." — nothing to surface.
    if trimmed.is_empty() || trimmed.starts_with("No ") {
        return;
    }
    println!("\n## {heading}\n");
    println!("```");
    for (i, line) in trimmed.lines().enumerate() {
        if i >= cap {
            println!("... (truncated; run `th pearls {sub}` for the full list)");
            break;
        }
        println!("{line}");
    }
    println!("```");
}

/// `th skills` — list / show skills discovered from every source.
/// Pearl th-e0f812. Walks the project's `.smooth/skills/` first,
/// then the user-level Smooth / Claude Code / opencode skill dirs.
fn cmd_skills(cmd: SkillsCommands) -> Result<()> {
    use owo_colors::OwoColorize;
    use smooth_cast::skills::{discover, discover_with_overrides, Skill, SkillSource};

    let workspace = std::env::current_dir().context("current directory")?;

    fn source_label(src: &SkillSource) -> &'static str {
        src.label()
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
        ServiceCommands::Install { system } => service::install(system),
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

#[cfg(test)]
mod doctor_setup_tests {
    use super::*;
    use clap::Parser;

    /// Every step must point at a command that actually exists — the fix string
    /// is what a stuck human types, so a typo here is the whole feature broken.
    #[test]
    fn every_step_fix_is_a_real_command() {
        let steps = [
            SetupStep::Providers,
            SetupStep::SmooLogin,
            SetupStep::Fda,
            SetupStep::Calendar,
            SetupStep::Reminders,
            SetupStep::Messages,
        ];
        for step in steps {
            assert!(!step.label().is_empty(), "{step:?} has no label");
            let fix = step.fix();
            assert!(fix.starts_with("th "), "{step:?} fix must be a th command: {fix}");
            // The doctor-driven ones must name a flag `th doctor` really parses.
            if let Some(flag) = fix.strip_prefix("th doctor ") {
                Cli::try_parse_from(["th", "doctor", flag]).unwrap_or_else(|e| panic!("{step:?} fix `{fix}` does not parse: {e}"));
            }
        }
    }

    #[test]
    fn onboard_parses_and_is_off_by_default() {
        let onboard = |args: &[&str]| match Cli::try_parse_from(args).expect("parses").command {
            Some(Commands::Doctor { onboard, .. }) => onboard,
            _ => panic!("expected Doctor"),
        };
        assert!(onboard(&["th", "doctor", "--onboard"]));
        assert!(!onboard(&["th", "doctor"]));
    }

    /// The walk order is the dependency order: credentials, then the disk-access
    /// grant, then the per-tool grants that depend on both.
    #[test]
    fn setup_steps_are_distinct() {
        let steps = [
            SetupStep::Providers,
            SetupStep::SmooLogin,
            SetupStep::Fda,
            SetupStep::Calendar,
            SetupStep::Reminders,
            SetupStep::Messages,
        ];
        for (i, a) in steps.iter().enumerate() {
            for b in &steps[i + 1..] {
                assert_ne!(a, b);
                assert_ne!(a.fix(), b.fix(), "{a:?} and {b:?} share a fix command");
            }
        }
    }
}

#[cfg(test)]
mod org_cli_tests {
    use super::*;
    use clap::Parser;

    /// clap's own structural lint — catches alias collisions, duplicate
    /// flags, and other config errors at test time.
    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    /// `th org` is the top-level alias for `th api orgs` — list / show /
    /// switch must all parse into the same OrgsCommands as the api path.
    #[test]
    fn th_org_top_level_alias_parses() {
        let cli = Cli::try_parse_from(["th", "org", "list"]).expect("th org list parses");
        assert!(matches!(cli.command, Some(Commands::Org { cmd: OrgsCommands::List })));

        let cli = Cli::try_parse_from(["th", "org", "switch", "ats"]).expect("th org switch parses");
        match cli.command {
            Some(Commands::Org {
                cmd: OrgsCommands::Switch { org_id },
            }) => assert_eq!(org_id.as_deref(), Some("ats")),
            _ => panic!("expected Org/Switch"),
        }

        let cli = Cli::try_parse_from(["th", "org", "show"]).expect("th org show parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Org {
                cmd: OrgsCommands::Show { org_id: None }
            })
        ));
    }

    /// The whole point of this pearl: `--org` and `--org-id` are
    /// interchangeable on the config surface (`th config` declares
    /// `org_id`, `th admin config` declares `org`).
    #[test]
    fn config_accepts_both_org_and_org_id() {
        let canonical = Cli::try_parse_from(["th", "config", "get", "databaseUrl", "--org-id", "X"]).expect("--org-id parses");
        let aliased = Cli::try_parse_from(["th", "config", "get", "databaseUrl", "--org", "X"]).expect("--org alias parses");
        // Both must land on the same Config/Get with org_id = "X".
        for cli in [canonical, aliased] {
            match cli.command {
                Some(Commands::Config {
                    cmd: config::Cmd::Get { org_id, .. },
                }) => assert_eq!(org_id.as_deref(), Some("X")),
                _ => panic!("expected Config/Get"),
            }
        }
    }

    /// Booking plural/singular forms are interchangeable: top-level
    /// `booking` ⇄ `bookings`, and within the group `types` ⇄ `type`,
    /// `block` ⇄ `blocks`, `calendars` ⇄ `calendar`. `types create`
    /// parses its flags on both the plural and singular spellings.
    #[test]
    fn th_booking_aliases_and_types_parse() {
        use smooai::booking::{Cmd as BookingCmd, TypesCmd};

        // `th bookings types create …` (both plural). `--slug` is required (the
        // server rejects a slug-less create), so it's included here.
        let cli = Cli::try_parse_from([
            "th",
            "bookings",
            "types",
            "create",
            "--name",
            "X",
            "--slug",
            "x",
            "--durations",
            "30",
            "--note",
            "hi",
        ])
        .expect("th bookings types create parses");
        match cli.command {
            Some(Commands::Booking {
                cmd: BookingCmd::Types {
                    cmd: TypesCmd::Create { name, durations, note, .. },
                },
            }) => {
                assert_eq!(name, "X");
                assert_eq!(durations, vec![30]);
                assert_eq!(note.as_deref(), Some("hi"));
            }
            _ => panic!("expected Booking/Types/Create"),
        }

        // Singular everything: `th booking type create …` must land identically.
        let cli = Cli::try_parse_from([
            "th",
            "booking",
            "type",
            "create",
            "--name",
            "X",
            "--slug",
            "x",
            "--durations",
            "30,45",
            "--one-time",
            "--org-shared",
        ])
        .expect("th booking type create parses");
        match cli.command {
            Some(Commands::Booking {
                cmd:
                    BookingCmd::Types {
                        cmd:
                            TypesCmd::Create {
                                durations,
                                one_time,
                                org_shared,
                                ..
                            },
                    },
            }) => {
                assert_eq!(durations, vec![30, 45]);
                assert!(one_time);
                assert!(org_shared);
            }
            _ => panic!("expected Booking/Types/Create"),
        }

        // block ⇄ blocks and calendars ⇄ calendar both resolve.
        assert!(matches!(
            Cli::try_parse_from(["th", "booking", "blocks", "list"]).expect("blocks alias").command,
            Some(Commands::Booking { cmd: BookingCmd::Block { .. } })
        ));
        assert!(matches!(
            Cli::try_parse_from(["th", "booking", "calendar", "list"]).expect("calendar alias").command,
            Some(Commands::Booking {
                cmd: BookingCmd::Calendars { .. }
            })
        ));

        // Typed link with a note parses the new Link flags.
        match Cli::try_parse_from(["th", "booking", "link", "--type", "demo", "--note", "hi"])
            .expect("link flags")
            .command
        {
            Some(Commands::Booking {
                cmd: BookingCmd::Link { type_slug, note, .. },
            }) => {
                assert_eq!(type_slug.as_deref(), Some("demo"));
                assert_eq!(note.as_deref(), Some("hi"));
            }
            _ => panic!("expected Booking/Link"),
        }
    }

    /// CLI-wide plural⇄singular normalization (th-6c4ddf): resource-noun
    /// command groups accept either form. Sample a few across the surfaces —
    /// top-level Commands, ApiCommands, and a nested crm/testing group — so a
    /// dropped alias regresses this test. (Full coverage is the clap
    /// debug_assert + the /normalize skill audit.)
    #[test]
    fn singular_plural_aliases_parse() {
        // top-level: `th org` ⇄ `th orgs`
        assert!(matches!(
            Cli::try_parse_from(["th", "orgs", "list"]).expect("th orgs").command,
            Some(Commands::Org { .. })
        ));
        // top-level: `th operatives` ⇄ `th operative`
        assert!(matches!(
            Cli::try_parse_from(["th", "operative"]).expect("th operative").command,
            Some(Commands::Operatives { .. })
        ));
        // api: `th api agents` ⇄ `th api agent`, `th api keys` ⇄ `th api key`
        assert!(matches!(
            Cli::try_parse_from(["th", "api", "agent", "list"]).expect("api agent").command,
            Some(Commands::Api {
                cmd: ApiCommands::Agents { .. }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["th", "api", "key", "list"]).expect("api key").command,
            Some(Commands::Api { cmd: ApiCommands::Keys { .. } })
        ));
        // nested crm: `th api crm contacts` ⇄ `th api crm contact`
        assert!(matches!(
            Cli::try_parse_from(["th", "api", "crm", "contact", "list"]).expect("crm contact").command,
            Some(Commands::Api {
                cmd: ApiCommands::Crm {
                    cmd: smooai::crm::Cmd::Contacts { .. }
                }
            })
        ));
        // top-level promotion: `th crm deals …` ⇄ `th api crm deals …`,
        // including the economics triple on `deals update`.
        assert!(matches!(
            Cli::try_parse_from(["th", "crm", "deals", "list"]).expect("th crm deals").command,
            Some(Commands::Crm {
                cmd: smooai::crm::Cmd::Deals { .. }
            })
        ));
        match Cli::try_parse_from([
            "th",
            "crm",
            "deals",
            "update",
            "some-deal",
            "--value",
            "13600",
            "--mrr",
            "550",
            "--upfront",
            "7000",
        ])
        .expect("th crm deals update")
        .command
        {
            Some(Commands::Crm {
                cmd: smooai::crm::Cmd::Deals {
                    cmd: smooai::crm::DealsCmd::Update { value, mrr, upfront, .. },
                },
            }) => {
                assert_eq!(value, Some(13600.0));
                assert_eq!(mrr, Some(550.0));
                assert_eq!(upfront, Some(7000.0));
            }
            _ => panic!("expected Crm/Deals/Update"),
        }
        // nested testing: `th testing runs` ⇄ `th testing run`
        assert!(matches!(
            Cli::try_parse_from(["th", "testing", "run", "list"]).expect("testing run").command,
            Some(Commands::Testing {
                cmd: smooai::testing::Cmd::Runs { .. }
            })
        ));
        // config: `th config environments` ⇄ `th config environment` (and the old `env`)
        assert!(matches!(
            Cli::try_parse_from(["th", "config", "environment", "list"])
                .expect("config environment")
                .command,
            Some(Commands::Config {
                cmd: config::Cmd::Environments { .. }
            })
        ));
        // admin: `th admin org` ⇄ `th admin orgs` — behind the `admin` feature,
        // so only parse-tested in admin builds (the /normalize audit covers it statically).
        #[cfg(feature = "admin")]
        assert!(matches!(
            Cli::try_parse_from(["th", "admin", "orgs", "list"]).expect("admin orgs").command,
            Some(Commands::Admin {
                cmd: admin::AdminCommands::Org { .. }
            })
        ));
        // The plural spelling still works too (aliases don't displace the canonical name).
        assert!(matches!(
            Cli::try_parse_from(["th", "api", "agents", "list"]).expect("api agents").command,
            Some(Commands::Api {
                cmd: ApiCommands::Agents { .. }
            })
        ));
    }

    /// `th llm` wraps the org llm-gateway API. create-key takes the org
    /// override (with the --org alias) and the keys subgroup parses.
    #[test]
    fn th_llm_parses() {
        use smooai::llm_gateway::{Cmd as LlmCmd, KeysCmd};

        // create-key with both flag spellings lands on the same variant.
        for flag in ["--org-id", "--org"] {
            let cli = Cli::try_parse_from(["th", "llm", "create-key", flag, "org-x"]).expect("th llm create-key parses");
            match cli.command {
                Some(Commands::Llm {
                    cmd: LlmCmd::CreateKey { org_id, .. },
                }) => assert_eq!(org_id.as_deref(), Some("org-x")),
                _ => panic!("expected Llm/CreateKey"),
            }
        }

        // Nested keys create carries the positional name + org override.
        let cli = Cli::try_parse_from(["th", "llm", "keys", "create", "ci", "--org", "org-x"]).expect("th llm keys create parses");
        match cli.command {
            Some(Commands::Llm {
                cmd: LlmCmd::Keys {
                    cmd: KeysCmd::Create { name, org_id, .. },
                },
            }) => {
                assert_eq!(name, "ci");
                assert_eq!(org_id.as_deref(), Some("org-x"));
            }
            _ => panic!("expected Llm/Keys/Create"),
        }
    }

    /// `th api keys create` exposes structured --type + repeatable
    /// --allowed-origin flags (the first-class B2M/M2M surface).
    #[test]
    fn th_api_keys_create_flags_parse() {
        use smooai::keys::{ClientType, Cmd as KeysCmd};

        let cli = Cli::try_parse_from([
            "th",
            "api",
            "keys",
            "create",
            "--type",
            "b2m",
            "--allowed-origin",
            "https://a.example.com",
            "--allowed-origin",
            "https://b.example.com",
            "--org",
            "org-x",
        ])
        .expect("th api keys create --type b2m parses");
        match cli.command {
            Some(Commands::Api {
                cmd:
                    ApiCommands::Keys {
                        cmd:
                            KeysCmd::Create {
                                client_type,
                                allowed_origins,
                                org_id,
                                ..
                            },
                    },
            }) => {
                assert_eq!(client_type, ClientType::B2m);
                assert_eq!(allowed_origins, vec!["https://a.example.com", "https://b.example.com"]);
                assert_eq!(org_id.as_deref(), Some("org-x"));
            }
            _ => panic!("expected Api/Keys/Create"),
        }

        // Default type is m2m when --type is omitted.
        let cli = Cli::try_parse_from(["th", "api", "keys", "create"]).expect("default create parses");
        match cli.command {
            Some(Commands::Api {
                cmd: ApiCommands::Keys {
                    cmd: KeysCmd::Create { client_type, .. },
                },
            }) => assert_eq!(client_type, ClientType::M2m),
            _ => panic!("expected Api/Keys/Create"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is the idiom for test assertions")]
mod cli_dispatch_tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, Commands, ProjectCommands};

    /// clap's own derive validator — catches malformed `#[arg]`/`#[command]`
    /// definitions (duplicate long flags, bad defaults) that otherwise only
    /// panic at runtime on the affected subcommand.
    #[test]
    fn command_definitions_are_well_formed() {
        Cli::command().debug_assert();
    }

    /// Pearl th-91de11: `main`'s dispatch used to end in a `Some(_) =>` arm that
    /// printed "Command not yet implemented. Coming soon!" and returned `Ok`.
    /// That wildcard defeated match exhaustiveness — `th project` was unwired
    /// and exited 0 — so a new `Commands` variant could ship dead and silent.
    /// The arm is gone; this test pins the two `project` verbs that it hid.
    #[test]
    fn project_subcommands_parse_to_their_own_variant() {
        let list = Cli::try_parse_from(["th", "project", "list"]).expect("th project list");
        assert!(matches!(list.command, Some(Commands::Project { cmd: ProjectCommands::List })));

        let create = Cli::try_parse_from(["th", "project", "create", "demo"]).expect("th project create");
        assert!(matches!(
            create.command,
            Some(Commands::Project {
                cmd: ProjectCommands::Create { .. }
            })
        ));
    }

    /// `th projects` is the documented alias and must reach the same variant.
    #[test]
    fn projects_alias_reaches_the_project_variant() {
        let aliased = Cli::try_parse_from(["th", "projects", "list"]).expect("th projects list");
        assert!(matches!(aliased.command, Some(Commands::Project { cmd: ProjectCommands::List })));
    }

    /// `th project create` has no implementation and must say so with a
    /// non-zero exit, not the old "Coming soon!" + success.
    #[test]
    fn project_create_fails_loudly_instead_of_succeeding_silently() {
        let err = super::cmd_project(ProjectCommands::Create {
            name: "demo".to_string(),
            description: None,
        })
        .expect_err("`th project create` must not report success");
        let msg = err.to_string();
        assert!(msg.contains("not implemented"), "{msg}");
        assert!(msg.contains("th pearls init"), "the error must point at the working path: {msg}");
    }

    /// Pearl th-f50195: `th audit list`/`tail` matched only `<actor>.log`, so
    /// `egress-proxy.jsonl` — the ONLY audit stream anything writes today
    /// (goalie's egress proxy) — was invisible, and a bare `th audit tail`
    /// looked for the long-dead `leader` actor. Both halves fail without the
    /// fix: `.jsonl` is skipped entirely, and the no-actor case resolves to
    /// nothing.
    #[test]
    fn audit_streams_include_jsonl_and_default_to_the_newest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        std::fs::write(dir.join("wonk.log"), "old\n").expect("write .log");
        // Ensure a distinct, strictly-later mtime than the .log above.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("egress-proxy.jsonl"), "{\"host\":\"api.smoo.ai\"}\n").expect("write .jsonl");
        std::fs::write(dir.join("notes.txt"), "ignore me\n").expect("write .txt");

        let actors: Vec<String> = super::audit_streams(dir).into_iter().map(|(a, _, _)| a).collect();
        assert!(actors.contains(&"egress-proxy".to_string()), "a .jsonl stream must be listed: {actors:?}");
        assert!(actors.contains(&"wonk".to_string()), "a .log stream must still be listed: {actors:?}");
        assert!(!actors.iter().any(|a| a == "notes"), "unrelated extensions must be ignored: {actors:?}");

        // No actor → newest stream, not the dead `leader` default.
        let (actor, path) = super::resolve_audit_stream(dir, None).expect("a default stream must resolve");
        assert_eq!(actor, "egress-proxy");
        assert!(path.ends_with("egress-proxy.jsonl"), "{path:?}");

        // Named actor resolves regardless of extension.
        assert_eq!(super::resolve_audit_stream(dir, Some("egress-proxy")).expect("named .jsonl").0, "egress-proxy");
        assert_eq!(super::resolve_audit_stream(dir, Some("wonk")).expect("named .log").0, "wonk");
    }

    /// An empty or missing directory must resolve to nothing rather than
    /// panicking on the unreadable read_dir.
    #[test]
    fn audit_streams_empty_when_nothing_to_read() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(super::audit_streams(tmp.path()).is_empty());
        assert!(super::resolve_audit_stream(tmp.path(), None).is_none());
        assert!(super::resolve_audit_stream(&tmp.path().join("nope"), None).is_none());
        assert!(super::resolve_audit_stream(tmp.path(), Some("leader")).is_none());
    }

    /// Every top-level command's SHORT help (`th -h`) must be one sentence.
    /// Pearl th-91de11: without a blank `///` line clap dumps the entire doc
    /// comment into short help, which turned `th -h` into a ~300-line wall.
    /// A multi-sentence `about` is the exact symptom, so assert on it.
    #[test]
    fn short_help_for_every_command_is_a_single_sentence() {
        for sub in Cli::command().get_subcommands() {
            let Some(about) = sub.get_about().map(ToString::to_string) else {
                continue;
            };
            // A sentence-ending period is one followed by a space. Abbreviations
            // ("e.g. ") and decimals are the only legitimate interior hits.
            let sentences = about
                .match_indices(". ")
                .filter(|(i, _)| {
                    let head = &about[..*i];
                    !head.ends_with("e.g") && !head.ends_with("i.e") && !head.ends_with("etc")
                })
                .count();
            assert_eq!(
                sentences,
                0,
                "`th {} -h` summary runs to multiple sentences — add a blank `///` line after the first:\n  {about}",
                sub.get_name()
            );
        }
    }
}

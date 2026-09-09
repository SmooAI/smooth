//! `th harness` — the harness manifests SmoothFlow launches (`list` / `show`
//! / `add` / `hide` / `unhide` / `order`, pearl th-0f6126) and one idempotent
//! setup/update/status command per coding harness on this machine (`enable`
//! / `status` / `disable`, pearl th-19dac1 / EPIC th-1945b9).
//!
//! Manifests are files (`smooth_flow::harness::Registry`), so `list`/`show`
//! read them directly; the sort/hide prefs live in the daemon's flow.db, so
//! `list` merges them when the daemon is up and `hide`/`unhide`/`order` need
//! it. `add` validates a manifest and copies it into `~/.smooth/harnesses/`.
//!
//! `enable` is also the update command: re-run it after upgrading `th` or the
//! smooth-agent plugin, the way `tsx agents enable <provider>` works in the
//! TSX toolbox this is modeled on. Every step is idempotent and preserving:
//! user-owned config survives untouched, and `disable` only ever removes what
//! smooth wrote — the MCP entry, and what `th pkg` recorded in its index.
//!
//! Since th-55b2c7 the skills / lifecycle-plugin half is sugar for
//! `th pkg install <smooth-agent checkout> --harness <x>`: smooth-agent is the
//! first package, and `th pkg` owns the rendering + provenance
//! (`~/.smooth/pkg/index.toml`). This command keeps the per-harness extras
//! (`claude` CLI plugin install, statusline check, Codex plugin detection).
//!
//! What each harness gets:
//!
//! | Harness | MCP (`th mcp serve`) | Package rendering (`th pkg`) | Extras |
//! |---|---|---|---|
//! | claude-code | `~/.claude.json` | via the smooth-agent marketplace plugin | plugin install/update via the `claude` CLI; statusline check |
//! | codex | `~/.codex/config.toml` | skills → `~/.codex/skills/`, SmoothFlow hooks key-merged into `~/.codex/hooks.json` (th-4ad334), rules → `~/.codex/AGENTS.md` | plugin state detection; `~/.smooth` added to the workspace-write sandbox's `writable_roots` |
//! | opencode | `~/.config/opencode/opencode.json` | skills → `~/.opencode/skills/`, lifecycle plugin → `~/.config/opencode/plugins/`, rules → `~/.config/opencode/AGENTS.md` | — |
//! | cursor | `~/.cursor/mcp.json` | rules → `~/.cursor/rules/smooth-agent/*.mdc` | — |

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;

use serde_json::{json, Value};
use smooth_flow::harness::{self as manifests, HarnessInfo, Prefs, Registry};

use crate::gradient::paint;
use crate::mcp_install::{self, Harness, Outcome};
use crate::pkg;

#[derive(Subcommand)]
pub enum Cmd {
    /// List the harness manifests this machine knows — built-ins,
    /// ~/.smooth/harnesses, the project's .smooth/harnesses, th pkg packages —
    /// in the picker order, with the resolved binary. Hidden ones need --all.
    #[command(visible_alias = "ls")]
    List {
        /// Include hidden harnesses.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// One manifest in full: origin, resolved binary, the TOML.
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Drop a harness from every picker (the manifest stays; `unhide` restores it).
    Hide { name: String },
    /// Put a hidden harness back in the pickers.
    Unhide { name: String },
    /// Put NAME… first in every picker, in this order; the rest follow.
    Order {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Validate a manifest and copy it into ~/.smooth/harnesses/<name>.toml.
    /// SOURCE is a .toml file, a directory holding harness.toml (or
    /// harness/<name>/harness.toml), or owner/repo[/subdir][#ref] on GitHub.
    ///
    /// With --agentic, SOURCE is a harness NAME and Big Smooth writes the
    /// manifest itself: it probes the CLI's --help (and --docs), drafts a
    /// manifest, validates it by launching a real session on a private
    /// engine (launch → working → idle, steer, kill+resume), iterates, installs
    /// it, and reports what it could not prove. Needs a running daemon with an
    /// LLM provider — you are offered the Smoo AI Gateway or your own key.
    Add {
        source: String,
        /// Overwrite an existing ~/.smooth/harnesses/<name>.toml.
        #[arg(long)]
        force: bool,
        /// Let Big Smooth draft + validate the manifest (SOURCE = harness name).
        #[arg(long)]
        agentic: bool,
        /// The executable's name or path when it differs from the name
        /// (e.g. `gemini-cli` → `gemini`). --agentic only.
        #[arg(long, requires = "agentic")]
        binary: Option<String>,
        /// A docs page (CLI reference / hooks) to give the drafter. --agentic only.
        #[arg(long, requires = "agentic", value_name = "URL")]
        docs: Option<String>,
        /// Draft → validate rounds before giving up (1–6). --agentic only.
        #[arg(long, requires = "agentic", default_value_t = 3)]
        iterations: u8,
        /// Install the best draft even when no run reached idle. --agentic only.
        #[arg(long, requires = "agentic")]
        install_unverified: bool,
        /// Model to pass through `{model}` while validating. --agentic only.
        #[arg(long, requires = "agentic")]
        model: Option<String>,
    },
    /// Set up (or update) a harness: register the `th mcp serve` MCP server,
    /// install/update the smooth-agent plugin where the harness has a plugin
    /// system, and link the shared skills where it doesn't.
    ///
    /// Idempotent — re-run after upgrading `th` or the plugin. This is the
    /// install AND the update command.
    Enable {
        /// claude-code | codex | opencode | cursor | all
        provider: String,
    },
    /// Show, per harness: installed?, MCP entry state, plugin/skills state,
    /// and (Claude Code) whether a statusline is wired.
    Status,
    /// Remove what smooth wrote for a harness: the MCP entry and any skill
    /// symlinks that resolve into smooth-owned sources. Never touches
    /// user-owned config; plugin uninstall stays with the harness's own CLI.
    Disable {
        /// claude-code | codex | opencode | cursor | all
        provider: String,
    },
}

/// # Errors
/// Returns an error when the provider name is unknown or a config file is
/// malformed (never silently clobbered).
pub async fn cmd(cmd: Cmd) -> Result<()> {
    let home = mcp_install::harness_home()?;
    match cmd {
        Cmd::List { all, json } => list(&home, all, json).await,
        Cmd::Show { name, json } => show(&home, &name, json),
        Cmd::Hide { name } => set_hidden(&name, true).await,
        Cmd::Unhide { name } => set_hidden(&name, false).await,
        Cmd::Order { names } => {
            let v = crate::flow::call(reqwest::Method::PUT, "/api/flow/harnesses/prefs", Some(json!({ "order": names }))).await?;
            print_harnesses(&infos_of(&v), true);
            Ok(())
        }
        Cmd::Add {
            source,
            force,
            agentic,
            binary,
            docs,
            iterations,
            install_unverified,
            model,
        } => {
            if agentic {
                add_agentic(crate::harness_agentic::AgenticArgs {
                    name: source,
                    binary,
                    docs,
                    iterations: iterations.clamp(1, 6),
                    force,
                    install_unverified,
                    model,
                })
                .await
            } else {
                add(&home, &source, force)
            }
        }
        Cmd::Enable { provider } => {
            for h in providers(&provider)? {
                enable(h, &home);
            }
            Ok(())
        }
        Cmd::Status => {
            for h in Harness::ALL {
                status(h, &home);
            }
            Ok(())
        }
        Cmd::Disable { provider } => {
            for h in providers(&provider)? {
                disable(h, &home)?;
            }
            Ok(())
        }
    }
}

// ------------------------------------------------------------- manifests ----

/// The registry as the daemon would see it from this cwd.
fn local_registry(home: &Path) -> Registry {
    let project = std::env::current_dir().ok().map(|d| smooth_flow::engine::project_root(&d));
    Registry::load(home, project.as_deref())
}

fn infos_of(v: &Value) -> Vec<HarnessInfo> {
    v.get("harnesses").cloned().and_then(|h| serde_json::from_value(h).ok()).unwrap_or_default()
}

/// `th harness list`: the daemon's view (prefs applied) when it runs, else
/// the files on disk with a note.
async fn list(home: &Path, all: bool, json: bool) -> Result<()> {
    let (infos, source, note) = match crate::flow::call(reqwest::Method::GET, "/api/flow/harnesses", None).await {
        Ok(v) => (infos_of(&v), "daemon", None),
        Err(e) => {
            let reg = local_registry(home);
            let path = std::env::var_os("PATH").unwrap_or_default();
            let note = format!(
                "daemon not reachable — order/hidden prefs not applied ({})",
                e.to_string().lines().next().unwrap_or("")
            );
            (reg.infos(&Prefs::default(), true, home, &path), "local", Some(note))
        }
    };
    let shown: Vec<HarnessInfo> = infos.into_iter().filter(|h| all || !h.hidden).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&json!({ "harnesses": shown, "source": source }))?);
        return Ok(());
    }
    print_harnesses(&shown, all);
    if let Some(n) = note {
        println!("{}", paint(&format!("  {n}"), |t| t.dimmed().to_string()));
    }
    for (file, err) in local_registry(home).errors {
        println!("{} {}: {err}", paint("!", |t| t.yellow().to_string()), file.display());
    }
    Ok(())
}

/// Presence CLI rules: `●` installed, `○` not; teal is the presence, amber
/// only where something needs you (a missing binary is quiet, not amber).
fn print_harnesses(infos: &[HarnessInfo], all: bool) {
    if infos.is_empty() {
        println!("No harness manifests. This is a confirmed read, not a read failure.");
        return;
    }
    let header = format!("   {:<10} {:<14} {:<8} {}", "NAME", "DISPLAY", "STATE", "BINARY");
    println!("{}", paint(&header, |h| h.bold().to_string()));
    for h in infos {
        let glyph = if h.installed {
            paint("●", |g| g.bold().to_string())
        } else {
            paint("○", |g| g.dimmed().to_string())
        };
        let tail = match (&h.binary_path, &h.reason) {
            (Some(p), _) => p.clone(),
            (None, Some(r)) => paint(r, |t| t.dimmed().to_string()),
            (None, None) => String::new(),
        };
        let hidden = if h.hidden && all {
            paint(" (hidden)", |t| t.dimmed().to_string())
        } else {
            String::new()
        };
        println!(
            "{glyph}  {:<10} {:<14} {:<8} {tail}{hidden}",
            h.name,
            short(&h.display_name, 14),
            h.state_source
        );
    }
}

fn short(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= n {
        s.to_string()
    } else {
        format!("{}…", c[..n.saturating_sub(1)].iter().collect::<String>())
    }
}

fn show(home: &Path, name: &str, json: bool) -> Result<()> {
    let reg = local_registry(home);
    let m = reg.get(name).ok_or_else(|| anyhow!("no harness named `{name}`\n  → th harness list --all"))?;
    let binary = m.resolve_binary_in(home, &std::env::var_os("PATH").unwrap_or_default());
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "manifest": m,
                "origin": m.origin.label(),
                "path": m.origin.path(),
                "binary_path": binary,
                "installed": binary.is_some(),
            }))?
        );
        return Ok(());
    }
    println!("{}", paint(&format!("== {} ({})", m.name, m.display_name), |t| t.bold().to_string()));
    println!(
        "   origin: {}{}",
        m.origin.label(),
        m.origin.path().map(|p| format!(" {}", p.display())).unwrap_or_default()
    );
    match &binary {
        Some(p) => println!("   binary: {}", p.display()),
        None => println!(
            "   binary: {} — `{}` not found on PATH",
            paint("missing", |t| t.dimmed().to_string()),
            m.binary.names.join("`/`")
        ),
    }
    println!(
        "   state:  {:?} — {}",
        m.state.source,
        if m.state.hooks.install.is_empty() {
            "(no install note)"
        } else {
            &m.state.hooks.install
        }
    );
    println!();
    print!("{}", toml::to_string_pretty(m).unwrap_or_default());
    Ok(())
}

async fn set_hidden(name: &str, hide: bool) -> Result<()> {
    let current = infos_of(&crate::flow::call(reqwest::Method::GET, "/api/flow/harnesses", None).await?);
    if !current.iter().any(|h| h.name == name) {
        bail!("no harness named `{name}`\n  → th harness list --all");
    }
    let mut hidden: Vec<String> = current.iter().filter(|h| h.hidden).map(|h| h.name.clone()).collect();
    hidden.retain(|n| n != name);
    if hide {
        hidden.push(name.to_string());
    }
    let v = crate::flow::call(reqwest::Method::PUT, "/api/flow/harnesses/prefs", Some(json!({ "hidden": hidden }))).await?;
    print_harnesses(&infos_of(&v), true);
    Ok(())
}

/// Where `th harness add` puts manifests.
fn user_manifests_dir(home: &Path) -> PathBuf {
    home.join(".smooth").join("harnesses")
}

/// Resolve SOURCE to manifest files: a `.toml`, a dir (its `harness.toml`
/// or `harness/*/harness.toml`), or a GitHub `owner/repo[/subdir][#ref]`
/// shallow-cloned into a temp dir.
fn manifest_files(source: &str) -> Result<(Vec<PathBuf>, Option<tempfile::TempDir>)> {
    let p = Path::new(source);
    if p.is_file() {
        return Ok((vec![p.to_path_buf()], None));
    }
    if p.is_dir() {
        return Ok((manifests_in(p), None));
    }
    let github = source.split('#').next().unwrap_or(source);
    let mut parts = github.splitn(3, '/');
    let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else {
        bail!("{source}: not a file, a directory, or owner/repo[/subdir][#ref]");
    };
    let subdir = parts.next();
    let git_ref = source.split_once('#').map(|(_, r)| r);
    let tmp = tempfile::tempdir()?;
    let mut cmd = Command::new("git");
    cmd.args(["clone", "--depth", "1", "--quiet"]);
    if let Some(r) = git_ref {
        cmd.args(["--branch", r]);
    }
    cmd.arg(format!("https://github.com/{owner}/{repo}.git")).arg(tmp.path());
    let out = cmd.output().context("run git clone")?;
    if !out.status.success() {
        bail!("git clone {owner}/{repo} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let root = subdir.map_or_else(|| tmp.path().to_path_buf(), |s| tmp.path().join(s));
    let files = manifests_in(&root);
    if files.is_empty() {
        bail!("{source}: no harness.toml (or harness/<name>/harness.toml) in {}", root.display());
    }
    Ok((files, Some(tmp)))
}

fn manifests_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if dir.join("harness.toml").is_file() {
        out.push(dir.join("harness.toml"));
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("harness")) {
        let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path().join("harness.toml")).filter(|p| p.is_file()).collect();
        found.sort();
        out.extend(found);
    }
    out
}

fn add(home: &Path, source: &str, force: bool) -> Result<()> {
    let (files, _tmp) = manifest_files(source)?;
    if files.is_empty() {
        bail!("{source}: no harness.toml found");
    }
    let dir = user_manifests_dir(home);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    for f in files {
        let m = manifests::load_file(&f)?;
        let dest = dir.join(format!("{}.toml", m.name));
        if dest.exists() && !force {
            bail!("{} exists — pass --force to replace it", dest.display());
        }
        std::fs::copy(&f, &dest).with_context(|| format!("copy {} → {}", f.display(), dest.display()))?;
        let binary = m.resolve_binary_in(home, &std::env::var_os("PATH").unwrap_or_default());
        println!(
            "{} {} → {}  ({})",
            paint("●", |g| g.bold().to_string()),
            m.name,
            dest.display(),
            binary.map_or_else(|| format!("`{}` not on PATH yet", m.binary.names.join("`/`")), |b| b.display().to_string())
        );
    }
    println!(
        "{}",
        paint("  pickers pick it up on their next flow.hello; th flow new --kind <name>", |t| t
            .dimmed()
            .to_string())
    );
    Ok(())
}

/// `th harness add --agentic <name>`: the provider gate, then one turn of
/// Big Smooth calling its `add_harness` tool; the manifest lands in
/// ~/.smooth/harnesses via the daemon (pearl th-473294).
async fn add_agentic(args: crate::harness_agentic::AgenticArgs) -> Result<()> {
    if !manifests::Manifest::parse(&format!(
        "name = \"{}\"\n[binary]\nnames = [\"x\"]\n[launch]\nargv = [\"{{prompt}}\"]\n",
        args.name
    ))
    .is_ok()
    {
        bail!(
            "`{}` is not a valid harness name (lowercase letters, digits, dashes)\n  → th harness add --agentic gemini",
            args.name
        );
    }
    if !crate::harness_agentic::ensure_provider().await? {
        return Ok(());
    }
    let reply = crate::harness_agentic::run_turn(&args).await?;
    // The report is authoritative: if the daemon installed it, it is on disk now.
    let home = mcp_install::harness_home()?;
    let dest = user_manifests_dir(&home).join(format!("{}.toml", args.name));
    if dest.is_file() {
        let m = manifests::load_file(&dest)?;
        let binary = m.resolve_binary_in(&home, &std::env::var_os("PATH").unwrap_or_default());
        println!(
            "{} {} → {}  ({})",
            paint("●", |g| g.bold().to_string()),
            m.name,
            dest.display(),
            binary.map_or_else(|| format!("`{}` not on PATH", m.binary.names.join("`/`")), |b| b.display().to_string())
        );
        println!(
            "{}",
            paint(
                &format!("  th flow new --kind {} --prompt \"say hi\"   ·   th harness show {}", m.name, m.name),
                |t| t.dimmed().to_string()
            )
        );
    } else if !reply.to_ascii_lowercase().contains("needs_provider") {
        println!(
            "{}",
            paint(
                "  not installed — the report above has the draft; th harness add <file> installs an edited one",
                |t| t.dimmed().to_string()
            )
        );
    }
    Ok(())
}

fn providers(spec: &str) -> Result<Vec<Harness>> {
    if spec.trim().eq_ignore_ascii_case("all") {
        Ok(Harness::ALL.to_vec())
    } else {
        Ok(vec![Harness::parse(spec)?])
    }
}

fn enable(h: Harness, home: &Path) {
    println!("{}", format!("== {h}").bold().bright_cyan());
    if !h.marker_dir(home).is_dir() {
        println!("   not installed on this machine (no {}) — skipped", h.marker_dir(home).display());
        return;
    }

    // 1. MCP server — the shared mailbox/pearl surface, all harnesses.
    match mcp_install::install_into(h, home, false) {
        Ok(o) => println!("   mcp: {}", describe_mcp(&o)),
        Err(e) => println!("   mcp: {} {e:#}", "FAILED".bright_red()),
    }

    // 2. Per-harness extras.
    match h {
        Harness::ClaudeCode => {
            claude_plugin_step(home);
            statusline_step(home);
        }
        Harness::Codex => {
            pkg_step(h, home);
            codex_plugin_step(home);
            codex_sandbox_step(home);
        }
        Harness::OpenCode | Harness::Cursor => pkg_step(h, home),
    }
}

/// `th pkg install <smooth-agent checkout> --harness <h>` — skills and the
/// per-harness overlay (OpenCode lifecycle plugin), with index provenance.
fn pkg_step(h: Harness, home: &Path) {
    let Some(root) = package_root(home) else {
        println!("   package: no smooth-agent checkout found — enable claude-code first (the plugin checkout is the canonical source)");
        return;
    };
    match pkg::install(&pkg::Paths::new(home.to_path_buf()), &pkg::Source::Path(root.clone()), &[h]) {
        Ok(_) => println!("   package: smooth-agent rendered for {h} from {} (th pkg status smooth-agent)", root.display()),
        Err(e) => println!("   package: {} {e:#}", "FAILED".bright_red()),
    }
}

/// Codex's `workspace-write` sandbox only lets a session write under its
/// cwd, so every `th pearls` / `th agent` write inside Codex fails with
/// "unable to open database file" (sqlite error 14) until `~/.smooth` is a
/// `writable_roots` entry. Added once, with a comment saying why.
fn codex_sandbox_step(home: &Path) {
    match ensure_codex_smooth_writable(home) {
        Ok(true) => println!(
            "   sandbox: added {} to [sandbox_workspace_write].writable_roots (pearls + agent mail writes)",
            home.join(".smooth").display()
        ),
        Ok(false) => println!("   sandbox: ~/.smooth already in [sandbox_workspace_write].writable_roots"),
        Err(e) => println!("   sandbox: {} {e:#}", "FAILED".bright_red()),
    }
}

fn describe_mcp(o: &Outcome) -> String {
    match o {
        Outcome::Added => "registered `th mcp serve`".to_string(),
        Outcome::AlreadyPresent => "already registered".to_string(),
        Outcome::Updated => "repointed a stale entry at `th mcp serve`".to_string(),
        Outcome::NotInstalled => "harness not installed".to_string(),
    }
}

/// Install or update the smooth-agent plugin through the `claude` CLI.
/// Network + external binary — failures are reported, never fatal, because
/// the rest of enable (MCP, statusline) is still worth doing.
fn claude_plugin_step(home: &Path) {
    if which("claude").is_none() {
        println!("   plugin: `claude` not on PATH — install the plugin from a Claude Code session: /plugin install smooth-agent@smooth");
        return;
    }
    if claude_plugin_cache(home).is_some() {
        run_step("plugin", "claude", &["plugin", "update", "smooth-agent@smooth"]);
    } else {
        // Marketplace add is idempotent-ish; an "already exists" failure is fine
        // because the install right after is the step that matters.
        let _ = Command::new("claude").args(["plugin", "marketplace", "add", "SmooAI/smooth"]).output();
        run_step("plugin", "claude", &["plugin", "install", "smooth-agent@smooth"]);
    }
}

fn run_step(label: &str, bin: &str, args: &[&str]) {
    match Command::new(bin).args(args).output() {
        Ok(out) if out.status.success() => println!("   {label}: {} {}", bin, args.join(" ")),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            println!(
                "   {label}: {} `{bin} {}` — {}",
                "FAILED".bright_red(),
                args.join(" "),
                err.trim().lines().next().unwrap_or("(no output)")
            );
        }
        Err(e) => println!("   {label}: {} could not run {bin}: {e}", "FAILED".bright_red()),
    }
}

/// Codex installs plugins through its own session/plugin system; detect and
/// instruct rather than editing its plugin state behind its back.
fn codex_plugin_step(home: &Path) {
    match codex_plugin_enabled(home) {
        Ok(true) => println!("   plugin: smooth-agent@smooth enabled"),
        Ok(false) => println!(
            "   plugin: not installed — in a Codex session run: /plugin install smooth-agent@smooth (marketplace: https://github.com/SmooAI/smooth.git)"
        ),
        Err(e) => println!("   plugin: could not read codex config — {e:#}"),
    }
}

fn statusline_step(home: &Path) {
    if claude_statusline_wired(home) {
        println!("   statusline: wired (settings.json has a statusLine entry — left alone)");
    } else {
        println!("   statusline: not set — run `th doctor --setup-statusline` for the th-mail handle + unread count line");
    }
}

fn status(h: Harness, home: &Path) {
    println!("{}", format!("== {h}").bold().bright_cyan());
    if !h.marker_dir(home).is_dir() {
        println!("   not installed");
        return;
    }
    // dry_run classifies without writing: AlreadyPresent = current,
    // Added = missing, Updated = stale entry pointing elsewhere.
    match mcp_install::install_into(h, home, true) {
        Ok(Outcome::AlreadyPresent) => println!("   mcp: ok"),
        Ok(Outcome::Added) => println!("   mcp: missing — run `th harness enable {h}`"),
        Ok(Outcome::Updated) => println!("   mcp: stale (points elsewhere) — run `th harness enable {h}`"),
        Ok(Outcome::NotInstalled) => println!("   mcp: harness not installed"),
        Err(e) => println!("   mcp: unreadable config — {e:#}"),
    }
    match h {
        Harness::ClaudeCode => {
            match claude_plugin_cache(home) {
                Some(v) => println!("   plugin: smooth-agent {v}"),
                None => println!("   plugin: not installed — `th harness enable claude-code`"),
            }
            statusline_step(home);
        }
        Harness::Codex => {
            match codex_plugin_enabled(home) {
                Ok(true) => println!("   plugin: smooth-agent@smooth enabled"),
                Ok(false) => println!("   plugin: not installed"),
                Err(e) => println!("   plugin: could not read codex config — {e:#}"),
            }
            match codex_flow_hooks(home) {
                0 => println!("   hooks: SmoothFlow flow hook not in ~/.codex/hooks.json — `th harness enable codex` (state falls back to pane scraping)"),
                n => println!("   hooks: SmoothFlow flow hook wired ({n} events in ~/.codex/hooks.json)"),
            }
            match codex_smooth_writable(home) {
                Ok(true) => println!("   sandbox: ~/.smooth writable (pearls + agent mail work under workspace-write)"),
                Ok(false) => println!(
                    "   sandbox: ~/.smooth NOT in [sandbox_workspace_write].writable_roots — th pearls/agent writes fail in Codex; `th harness enable codex`"
                ),
                Err(e) => println!("   sandbox: could not read codex config — {e:#}"),
            }
        }
        Harness::Cursor => {
            let dir = home.join(".cursor").join("rules").join("smooth-agent");
            let n = std::fs::read_dir(&dir).map_or(0, |d| d.flatten().count());
            if n == 0 {
                println!("   rules: none rendered — `th harness enable cursor`");
            } else {
                println!("   rules: {n} rendered in {}", dir.display());
            }
        }
        Harness::OpenCode => {
            let n = smooth_skill_links(home).len();
            if n == 0 {
                println!("   skills: none linked — `th harness enable opencode`");
            } else {
                println!("   skills: {n} linked");
            }
            if smooth_owned_link(&opencode_plugin_link(home), home) {
                println!("   plugin: lifecycle plugin linked");
            } else {
                println!("   plugin: not linked — `th harness enable opencode`");
            }
        }
    }
}

fn disable(h: Harness, home: &Path) -> Result<()> {
    println!("{}", format!("== {h}").bold().bright_cyan());
    let removed = remove_mcp_entry(h, home)?;
    println!("   mcp: {}", if removed { "entry removed" } else { "no entry (nothing to remove)" });
    if h == Harness::OpenCode {
        let links = smooth_skill_links(home);
        for l in &links {
            std::fs::remove_file(l).with_context(|| format!("remove {}", l.display()))?;
        }
        println!("   skills: {} smooth-owned links removed", links.len());
        let plugin = opencode_plugin_link(home);
        if smooth_owned_link(&plugin, home) {
            std::fs::remove_file(&plugin).with_context(|| format!("remove {}", plugin.display()))?;
            println!("   plugin: lifecycle plugin link removed");
        }
    }
    if h == Harness::ClaudeCode {
        println!("   plugin: left installed — remove with `claude plugin uninstall smooth-agent@smooth` if you mean it");
    } else {
        // Everything `th pkg` recorded for this harness: skills, hooks it
        // merged into hooks.json, config keys, rules, AGENTS.md sections.
        let warnings = pkg::rm_harness(&pkg::Paths::new(home.to_path_buf()), "smooth-agent", h)?;
        for w in warnings {
            println!("   {} {w}", "!".bright_yellow());
        }
        println!("   package: smooth-agent's {h} rendering removed (th pkg index)");
    }
    if h == Harness::Codex {
        println!("   sandbox: ~/.smooth left in writable_roots (harmless without th; remove by hand if you mean it)");
    }
    println!("   (th pkg rm smooth-agent removes the package from every harness at once)");
    Ok(())
}

// ---------------------------------------------------------------- skills ----

/// Where the canonical smooth-agent package lives on this machine: the path
/// a previous `th pkg install <path>` of smooth-agent used (a repo checkout —
/// the freshest source, and what `enable` should re-render from), else the
/// newest installed Claude plugin cache, else the marketplace checkout.
fn package_root(home: &Path) -> Option<PathBuf> {
    if let Some(p) = pkg::Index::load(&pkg::Paths::new(home.to_path_buf()))
        .ok()
        .and_then(|i| i.packages.get("smooth-agent").and_then(|r| r.source.strip_prefix("path:").map(PathBuf::from)))
        .filter(|p| p.join("skills").is_dir())
    {
        return Some(p);
    }
    if let Some(version) = claude_plugin_cache(home) {
        let p = home.join(".claude/plugins/cache/smooth/smooth-agent").join(version);
        if p.join("skills").is_dir() {
            return Some(p);
        }
    }
    let market = home.join(".claude/plugins/marketplaces/smooth/claude-plugins/smooth-agent");
    market.join("skills").is_dir().then_some(market)
}

/// Newest version directory in the Claude plugin cache, by semver-ish sort.
fn claude_plugin_cache(home: &Path) -> Option<String> {
    let dir = home.join(".claude/plugins/cache/smooth/smooth-agent");
    let mut versions: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    versions.sort_by_key(|v| v.split('.').filter_map(|p| p.parse::<u64>().ok()).collect::<Vec<_>>());
    versions.pop()
}

/// The directory OpenCode-side skills are linked into. `~/.opencode/skills/`
/// is what `th skills` already discovers.
// ponytail: if OpenCode's own skill scan turns out to live elsewhere, the
// lifecycle plugin work (th-cc50cd) is where that gets reconciled.
fn opencode_skills_dir(home: &Path) -> PathBuf {
    home.join(".opencode").join("skills")
}

/// Where the OpenCode lifecycle plugin (th-cc50cd) gets linked: OpenCode
/// auto-loads plugin files from `~/.config/opencode/plugins/`.
fn opencode_plugin_link(home: &Path) -> PathBuf {
    home.join(".config").join("opencode").join("plugins").join("smooth-agent.js")
}

/// Is this path a symlink resolving into smooth-owned sources — the Claude
/// plugin cache / marketplace checkout (pre-th-55b2c7 links) or the `th pkg`
/// cache? The ownership rule for everything `disable` may remove.
fn smooth_owned_link(path: &Path, home: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) && std::fs::read_link(path).is_ok_and(|t| smooth_owned_target(&t, home))
}

fn smooth_owned_target(target: &Path, home: &Path) -> bool {
    target.starts_with(home.join(".claude").join("plugins")) || target.starts_with(home.join(".smooth").join("pkg").join("cache"))
}

/// Symlinks in the OpenCode skills dir that resolve into smooth-owned sources
/// (the plugin cache or marketplace checkout) — the only things disable removes.
fn smooth_skill_links(home: &Path) -> Vec<PathBuf> {
    let dir = opencode_skills_dir(home);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    for entry in entries.flatten() {
        let p = entry.path();
        if smooth_owned_link(&p, home) {
            out.push(p);
        }
    }
    out
}

// ------------------------------------------------------------- detection ----

fn codex_plugin_enabled(home: &Path) -> Result<bool> {
    let path = Harness::Codex.config_path(home);
    if !path.exists() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let doc: toml_edit::DocumentMut = raw.parse().with_context(|| format!("parse {}", path.display()))?;
    Ok(doc
        .get("plugins")
        .and_then(|p| p.get("smooth-agent@smooth"))
        .and_then(|e| e.get("enabled"))
        .and_then(toml_edit::Item::as_bool)
        .unwrap_or(false))
}

/// How many `flow-hook.sh` entries `~/.codex/hooks.json` carries (0 = not wired).
fn codex_flow_hooks(home: &Path) -> usize {
    fn count(v: &Value) -> usize {
        match v {
            Value::String(s) => usize::from(s.contains("flow-hook.sh")),
            Value::Array(a) => a.iter().map(count).sum(),
            Value::Object(m) => m.values().map(count).sum(),
            _ => 0,
        }
    }
    std::fs::read_to_string(home.join(".codex").join("hooks.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .map_or(0, |v| count(&v))
}

/// The spellings of `~/.smooth` a user might already have in `writable_roots`.
fn smooth_root_spellings(home: &Path) -> [String; 2] {
    [home.join(".smooth").display().to_string(), "~/.smooth".to_string()]
}

/// Is `~/.smooth` (either spelling) in `[sandbox_workspace_write].writable_roots`?
fn codex_smooth_writable(home: &Path) -> Result<bool> {
    let path = Harness::Codex.config_path(home);
    if !path.exists() {
        return Ok(false);
    }
    let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path)?.parse().with_context(|| format!("parse {}", path.display()))?;
    let spellings = smooth_root_spellings(home);
    Ok(doc
        .get("sandbox_workspace_write")
        .and_then(|t| t.get("writable_roots"))
        .and_then(toml_edit::Item::as_array)
        .is_some_and(|a| a.iter().filter_map(toml_edit::Value::as_str).any(|s| spellings.iter().any(|x| x == s))))
}

/// Add `~/.smooth` (absolute — Codex does not document tilde expansion there)
/// to `writable_roots`, creating the table with an explanatory comment.
/// Returns whether anything was written. Edits in place: the user's other
/// roots, comments and layout survive.
fn ensure_codex_smooth_writable(home: &Path) -> Result<bool> {
    if codex_smooth_writable(home)? {
        return Ok(false);
    }
    let path = Harness::Codex.config_path(home);
    let raw = if path.exists() { std::fs::read_to_string(&path)? } else { String::new() };
    let mut doc: toml_edit::DocumentMut = raw.parse().with_context(|| format!("parse {} — fix or move it, then re-run", path.display()))?;
    let fresh = doc.get("sandbox_workspace_write").is_none();
    let item = doc["sandbox_workspace_write"].or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    if fresh {
        if let Some(t) = item.as_table_mut() {
            t.decor_mut().set_prefix(
                "\n# th (pearls, agent mail) keeps its state under ~/.smooth; without this the\n# workspace-write sandbox fails every `th agent`/`th pearls` write with\n# \"unable to open database file\" (sqlite error 14). Added by `th harness enable codex`.\n",
            );
        }
    }
    let Some(table) = item.as_table_like_mut() else {
        bail!("[sandbox_workspace_write] in {} is not a table", path.display());
    };
    let roots = table.entry("writable_roots").or_insert(toml_edit::value(toml_edit::Array::new()));
    let Some(arr) = roots.as_array_mut() else {
        bail!("[sandbox_workspace_write].writable_roots in {} is not an array", path.display());
    };
    arr.push(home.join(".smooth").display().to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, doc.to_string()).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

fn claude_statusline_wired(home: &Path) -> bool {
    let settings = home.join(".claude").join("settings.json");
    std::fs::read_to_string(settings)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .is_some_and(|v| v.get("statusLine").is_some())
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

// -------------------------------------------------------------- removal ----

/// Remove the `smooth` MCP entry from a harness config. Preserving edits only
/// — same guarantees as the installers in `mcp_install`.
fn remove_mcp_entry(h: Harness, home: &Path) -> Result<bool> {
    mcp_install::remove_server(h, home, mcp_install::SERVER_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A fake home with all marker dirs and a canonical smooth-agent checkout
    /// (marketplace layout) holding two skills + the OpenCode overlay.
    fn home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        for h in Harness::ALL {
            std::fs::create_dir_all(h.marker_dir(tmp.path())).unwrap();
        }
        let root = tmp.path().join(".claude/plugins/marketplaces/smooth/claude-plugins/smooth-agent");
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(root.join(".claude-plugin/plugin.json"), r#"{"name":"smooth-agent","version":"0.0.1"}"#).unwrap();
        for skill in ["agent-comms", "pearls-flow"] {
            let d = root.join("skills").join(skill);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("SKILL.md"), "x").unwrap();
        }
        let oc = root.join("harness/opencode");
        std::fs::create_dir_all(&oc).unwrap();
        std::fs::write(oc.join("plugin.js"), "export const SmoothAgent = 1;").unwrap();
        let cx = root.join("harness/codex");
        std::fs::create_dir_all(&cx).unwrap();
        std::fs::write(
            cx.join("hooks.json"),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/hooks/flow-hook.sh SessionStart codex"}]}],"Stop":[{"hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/hooks/flow-hook.sh Stop codex"}]}]}}"#,
        )
        .unwrap();
        tmp
    }

    /// th-4ad334: `enable codex` merges the flow hook into the user's
    /// hooks.json and adds ~/.smooth to the sandbox once; `disable` takes
    /// back only what we added.
    #[test]
    #[cfg(unix)]
    fn enable_codex_wires_flow_hooks_and_sandbox_and_disable_removes_only_ours() {
        let tmp = home();
        let hooks = tmp.path().join(".codex/hooks.json");
        let user_hooks = serde_json::json!({"hooks":{
            "PreCompact":[{"matcher":"","hooks":[{"type":"command","command":"th prime"}]}],
            "SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"th prime"}]}]
        }});
        std::fs::write(&hooks, serde_json::to_string_pretty(&user_hooks).unwrap()).unwrap();
        let cfg = Harness::Codex.config_path(tmp.path());
        std::fs::write(
            &cfg,
            "# mine\nmodel = \"gpt-5.5\"\n\n[sandbox_workspace_write]\nwritable_roots = [\"/tmp/other\"]\n",
        )
        .unwrap();

        enable(Harness::Codex, tmp.path());
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&hooks).unwrap()).unwrap();
        let ss: Vec<&str> = doc["hooks"]["SessionStart"][0]["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["command"].as_str().unwrap())
            .collect();
        assert_eq!(ss[0], "th prime", "the user's hook stays first");
        assert!(
            ss[1].ends_with("/hooks/flow-hook.sh SessionStart codex") && ss[1].contains("/.smooth/pkg/cache/"),
            "{ss:?}"
        );
        assert_eq!(ss.len(), 2);
        assert_eq!(doc["hooks"]["PreCompact"][0]["hooks"][0]["command"], "th prime");
        assert_eq!(codex_flow_hooks(tmp.path()), 2);
        let raw = std::fs::read_to_string(&cfg).unwrap();
        assert!(raw.contains("# mine") && raw.contains("\"/tmp/other\""), "{raw}");
        assert!(codex_smooth_writable(tmp.path()).unwrap(), "{raw}");
        assert!(raw.contains(&format!("\"{}\"", tmp.path().join(".smooth").display())), "{raw}");
        assert!(!raw.contains("sqlite error 14"), "no comment when the table already existed: {raw}");

        // Idempotent: nothing doubles.
        enable(Harness::Codex, tmp.path());
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&hooks).unwrap()).unwrap();
        assert_eq!(doc["hooks"]["SessionStart"][0]["hooks"].as_array().unwrap().len(), 2);
        let abs = format!("\"{}\"", tmp.path().join(".smooth").display());
        assert_eq!(std::fs::read_to_string(&cfg).unwrap().matches(&abs).count(), 1);
        status(Harness::Codex, tmp.path());

        disable(Harness::Codex, tmp.path()).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&hooks).unwrap()).unwrap();
        assert_eq!(doc, user_hooks, "exactly the user's hooks again");
        assert_eq!(codex_flow_hooks(tmp.path()), 0);
        let raw = std::fs::read_to_string(&cfg).unwrap();
        assert!(!raw.contains("mcp_servers.smooth") && raw.contains("# mine"), "{raw}");
        assert!(codex_smooth_writable(tmp.path()).unwrap(), "the sandbox root is left in place");
        assert!(
            std::fs::symlink_metadata(tmp.path().join(".codex/skills/pearls-flow")).is_err(),
            "skills links removed"
        );
    }

    #[test]
    fn codex_sandbox_root_is_added_once_with_a_comment_and_both_spellings_count() {
        let tmp = home();
        let cfg = Harness::Codex.config_path(tmp.path());
        assert!(!codex_smooth_writable(tmp.path()).unwrap(), "no file yet");
        std::fs::write(&cfg, "model = \"m\"\n").unwrap();
        assert!(ensure_codex_smooth_writable(tmp.path()).unwrap());
        let raw = std::fs::read_to_string(&cfg).unwrap();
        assert!(raw.starts_with("model = \"m\"\n"), "{raw}");
        assert!(raw.contains("sqlite error 14") && raw.contains("[sandbox_workspace_write]"), "{raw}");
        let doc: toml_edit::DocumentMut = raw.parse().unwrap();
        assert_eq!(doc["sandbox_workspace_write"]["writable_roots"].as_array().unwrap().len(), 1);
        assert!(!ensure_codex_smooth_writable(tmp.path()).unwrap(), "second call is a no-op");
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), raw);

        // The tilde spelling counts as present too.
        std::fs::write(&cfg, "[sandbox_workspace_write]\nwritable_roots = [\"~/.smooth\"]\n").unwrap();
        assert!(codex_smooth_writable(tmp.path()).unwrap());
        assert!(!ensure_codex_smooth_writable(tmp.path()).unwrap());
        // A table without the key gets the key; a non-array key is refused.
        std::fs::write(&cfg, "[sandbox_workspace_write]\nnetwork_access = true\n").unwrap();
        assert!(ensure_codex_smooth_writable(tmp.path()).unwrap());
        let raw = std::fs::read_to_string(&cfg).unwrap();
        assert!(raw.contains("network_access = true") && raw.contains("writable_roots"), "{raw}");
        std::fs::write(&cfg, "[sandbox_workspace_write]\nwritable_roots = \"nope\"\n").unwrap();
        assert!(ensure_codex_smooth_writable(tmp.path()).is_err());
        std::fs::write(&cfg, "[[[").unwrap();
        assert!(codex_smooth_writable(tmp.path()).is_err());
    }

    #[test]
    fn package_root_prefers_the_index_source_path_when_it_still_exists() {
        let tmp = home();
        let paths = pkg::Paths::new(tmp.path().to_path_buf());
        let src = tmp.path().join("checkout");
        std::fs::create_dir_all(src.join("skills/x")).unwrap();
        std::fs::create_dir_all(src.join(".claude-plugin")).unwrap();
        std::fs::write(src.join(".claude-plugin/plugin.json"), r#"{"name":"smooth-agent"}"#).unwrap();
        pkg::install(&paths, &pkg::Source::Path(src.clone()), &[Harness::OpenCode]).unwrap();
        assert_eq!(package_root(tmp.path()).unwrap(), src.canonicalize().unwrap());
        std::fs::remove_dir_all(&src).unwrap();
        assert!(
            package_root(tmp.path()).unwrap().ends_with("marketplaces/smooth/claude-plugins/smooth-agent"),
            "falls back"
        );
    }

    #[test]
    fn package_root_prefers_the_newest_cache_version_over_the_marketplace() {
        let tmp = home();
        assert!(package_root(tmp.path()).unwrap().ends_with("marketplaces/smooth/claude-plugins/smooth-agent"));
        for v in ["0.4.0", "0.31.1"] {
            std::fs::create_dir_all(tmp.path().join(".claude/plugins/cache/smooth/smooth-agent").join(v).join("skills")).unwrap();
        }
        // 0.31.1 > 0.4.0 numerically even though it sorts lower lexically.
        assert!(package_root(tmp.path()).unwrap().to_string_lossy().contains("0.31.1"));
    }

    #[test]
    #[cfg(unix)] // exercises real symlinks
    fn enable_opencode_renders_the_package_and_disable_removes_only_ours() {
        let tmp = home();
        enable(Harness::OpenCode, tmp.path());
        let links = smooth_skill_links(tmp.path());
        assert_eq!(links.len(), 2, "{links:?}");
        assert!(
            smooth_owned_link(&opencode_plugin_link(tmp.path()), tmp.path()),
            "lifecycle plugin linked from the package overlay"
        );
        // Idempotent.
        enable(Harness::OpenCode, tmp.path());
        assert_eq!(smooth_skill_links(tmp.path()).len(), 2);

        // User adds their own skill dir + their own MCP server alongside ours.
        let user_dir = opencode_skills_dir(tmp.path()).join("my-own-skill");
        std::fs::create_dir_all(&user_dir).unwrap();
        let cfg = Harness::OpenCode.config_path(tmp.path());
        let mut doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        doc["mcp"]["other"] = serde_json::json!({"type":"local","command":["x"]});
        std::fs::write(&cfg, doc.to_string()).unwrap();

        disable(Harness::OpenCode, tmp.path()).unwrap();

        assert!(user_dir.is_dir(), "user skill dir must survive");
        assert!(smooth_skill_links(tmp.path()).is_empty());
        assert!(
            std::fs::symlink_metadata(opencode_plugin_link(tmp.path())).is_err(),
            "disable must remove the plugin link"
        );
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert!(doc["mcp"].get("smooth").is_none());
        assert_eq!(doc["mcp"]["other"]["command"], serde_json::json!(["x"]));
    }

    #[test]
    fn remove_mcp_entry_handles_all_three_formats_and_missing_entries() {
        let tmp = home();
        for h in Harness::ALL {
            assert!(!remove_mcp_entry(h, tmp.path()).unwrap(), "{h}: nothing to remove yet");
            mcp_install::install_into(h, tmp.path(), false).unwrap();
            assert!(remove_mcp_entry(h, tmp.path()).unwrap(), "{h}: entry should be removed");
            assert!(!remove_mcp_entry(h, tmp.path()).unwrap(), "{h}: second remove is a no-op");
        }
        // Codex file keeps its other content.
        std::fs::write(
            Harness::Codex.config_path(tmp.path()),
            "model = \"gpt-5.5\"\n[mcp_servers.smooth]\ncommand = \"th\"\n",
        )
        .unwrap();
        assert!(remove_mcp_entry(Harness::Codex, tmp.path()).unwrap());
        let raw = std::fs::read_to_string(Harness::Codex.config_path(tmp.path())).unwrap();
        assert!(raw.contains("model = \"gpt-5.5\""));
        assert!(!raw.contains("mcp_servers.smooth"));
    }

    #[test]
    fn codex_plugin_and_statusline_detection() {
        let tmp = home();
        assert!(!codex_plugin_enabled(tmp.path()).unwrap());
        std::fs::write(Harness::Codex.config_path(tmp.path()), "[plugins.\"smooth-agent@smooth\"]\nenabled = true\n").unwrap();
        assert!(codex_plugin_enabled(tmp.path()).unwrap());

        assert!(!claude_statusline_wired(tmp.path()));
        std::fs::write(tmp.path().join(".claude/settings.json"), r#"{"statusLine":{"type":"command","command":"x"}}"#).unwrap();
        assert!(claude_statusline_wired(tmp.path()));
    }

    #[test]
    #[cfg(unix)]
    fn a_users_real_plugin_file_is_never_ours_to_remove() {
        let tmp = home();
        let link = opencode_plugin_link(tmp.path());
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::fs::write(&link, "my own plugin").unwrap();
        enable(Harness::OpenCode, tmp.path());
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "my own plugin");
        assert!(!smooth_owned_link(&link, tmp.path()));
        disable(Harness::OpenCode, tmp.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "my own plugin");
    }

    /// th-0f6126: `add` validates then copies into ~/.smooth/harnesses;
    /// refuses to clobber without --force; a dir source finds every overlay.
    #[test]
    fn add_copies_valid_manifests_and_refuses_to_clobber() {
        let tmp = home();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("harness/amp")).unwrap();
        let good = "name=\"aider\"\n[binary]\nnames=[\"aider\"]\n[launch]\nargv=[\"{prompt}\"]\n";
        std::fs::write(src.join("harness.toml"), good).unwrap();
        std::fs::write(src.join("harness/amp/harness.toml"), good.replace("aider", "amp")).unwrap();
        add(tmp.path(), src.to_str().unwrap(), false).unwrap();
        let dir = user_manifests_dir(tmp.path());
        assert!(dir.join("aider.toml").is_file() && dir.join("amp.toml").is_file());
        assert!(add(tmp.path(), src.join("harness.toml").to_str().unwrap(), false)
            .unwrap_err()
            .to_string()
            .contains("--force"));
        add(tmp.path(), src.join("harness.toml").to_str().unwrap(), true).unwrap();
        // Invalid never lands.
        std::fs::write(src.join("bad.toml"), "name = \"bad\"\n").unwrap();
        let err = format!("{:#}", add(tmp.path(), src.join("bad.toml").to_str().unwrap(), false).unwrap_err());
        assert!(err.contains("binary.names"), "{err}");
        assert!(!dir.join("bad.toml").exists());
        assert!(add(tmp.path(), "not-a-source", false).unwrap_err().to_string().contains("owner/repo"));
        // The local registry now lists them with origin user, and show finds one.
        let reg = Registry::load(tmp.path(), None);
        assert_eq!(reg.get("aider").unwrap().origin.label(), "user");
        show(tmp.path(), "amp", true).unwrap();
        assert!(show(tmp.path(), "nope", false).is_err());
    }

    /// th-0f6126: the list JSON shape (what the pickers and `--json` consumers read).
    #[test]
    fn list_rows_render_and_serialize() {
        let tmp = home();
        let reg = Registry::load(tmp.path(), None);
        let rows = reg.infos(&Prefs::default(), true, tmp.path(), &std::ffi::OsString::new());
        assert_eq!(rows.len(), 4);
        let v = serde_json::to_value(&rows).unwrap();
        assert_eq!(v[0]["name"], "claude");
        assert_eq!(v[0]["installed"], false);
        assert_eq!(v[0]["order_index"], 0);
        assert_eq!(infos_of(&json!({ "harnesses": v })).len(), 4);
        print_harnesses(&rows, true);
        print_harnesses(&[], false);
    }

    #[test]
    fn providers_expands_all_and_rejects_junk() {
        assert_eq!(providers("all").unwrap(), Harness::ALL.to_vec());
        assert_eq!(providers("claude").unwrap(), vec![Harness::ClaudeCode]);
        assert_eq!(providers("cursor").unwrap(), vec![Harness::Cursor]);
        assert!(providers("copilot").is_err());
    }
}

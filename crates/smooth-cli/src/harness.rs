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
//! | codex | `~/.codex/config.toml` | skills → `~/.codex/skills/` | plugin state detection + instructions |
//! | opencode | `~/.config/opencode/opencode.json` | skills → `~/.opencode/skills/`, lifecycle plugin → `~/.config/opencode/plugins/` | — |

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
    Add {
        source: String,
        /// Overwrite an existing ~/.smooth/harnesses/<name>.toml.
        #[arg(long)]
        force: bool,
    },
    /// Set up (or update) a harness: register the `th mcp serve` MCP server,
    /// install/update the smooth-agent plugin where the harness has a plugin
    /// system, and link the shared skills where it doesn't.
    ///
    /// Idempotent — re-run after upgrading `th` or the plugin. This is the
    /// install AND the update command.
    Enable {
        /// claude-code | codex | opencode | all
        provider: String,
    },
    /// Show, per harness: installed?, MCP entry state, plugin/skills state,
    /// and (Claude Code) whether a statusline is wired.
    Status,
    /// Remove what smooth wrote for a harness: the MCP entry and any skill
    /// symlinks that resolve into smooth-owned sources. Never touches
    /// user-owned config; plugin uninstall stays with the harness's own CLI.
    Disable {
        /// claude-code | codex | opencode | all
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
        Cmd::Add { source, force } => add(&home, &source, force),
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
        }
        Harness::OpenCode => pkg_step(h, home),
    }
}

/// `th pkg install <smooth-agent checkout> --harness <h>` — skills and the
/// per-harness overlay (OpenCode lifecycle plugin), with index provenance.
fn pkg_step(h: Harness, home: &Path) {
    let Some(root) = package_root(home) else {
        println!("   package: no smooth-agent checkout found — enable claude-code first (the plugin checkout is the canonical source)");
        return;
    };
    match pkg::install(&pkg::Paths::new(home.to_path_buf()), &pkg::Source::Path(root), &[h]) {
        Ok(_) => println!("   package: smooth-agent rendered for {h} (th pkg status smooth-agent)"),
        Err(e) => println!("   package: {} {e:#}", "FAILED".bright_red()),
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
        Harness::Codex => match codex_plugin_enabled(home) {
            Ok(true) => println!("   plugin: smooth-agent@smooth enabled"),
            Ok(false) => println!("   plugin: not installed"),
            Err(e) => println!("   plugin: could not read codex config — {e:#}"),
        },
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
    }
    println!("   (th pkg rm smooth-agent removes the package from every harness at once)");
    Ok(())
}

// ---------------------------------------------------------------- skills ----

/// Where the canonical smooth-agent package lives on this machine: the newest
/// installed Claude plugin cache, else the marketplace checkout.
fn package_root(home: &Path) -> Option<PathBuf> {
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
        tmp
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
        assert!(providers("cursor").is_err());
    }
}

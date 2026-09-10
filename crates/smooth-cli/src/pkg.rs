//! `th pkg` — one package, N harness renderings (EPIC th-55b2c7, M0 + M1).
//!
//! A package is a Claude Code plugin checkout used as the SHARED CORE
//! (`.claude-plugin/plugin.json`, `skills/`, `commands/`, `agents/`, `hooks/`,
//! `.mcp.json`) plus `rules/*.md` and per-harness OVERLAYS under
//! `harness/<name>/` holding what only that harness understands. `install`
//! fetches the source into `~/.smooth/pkg/cache/`, composes core + overlay per
//! target harness and renders each harness's NATIVE shape:
//!
//! | Harness | Rendering |
//! |---|---|
//! | claude-code | handed to Claude's own plugin system: composed plugin (core `hooks/hooks.json` KEY-MERGED with `harness/claude-code/hooks/hooks.json`) under the local `th-pkg` marketplace + `enabledPlugins`/`extraKnownMarketplaces` in `~/.claude/settings.json`; `rules/` → `~/.claude/rules/<pkg>/` |
//! | codex | `skills/` → `~/.codex/skills/`, `.mcp.json` → `[mcp_servers.*]`, `harness/codex/config.toml` key-merged into `~/.codex/config.toml`, `harness/codex/hooks.json` key-merged into `~/.codex/hooks.json` (Codex ≥ 0.153 reads Claude-style hooks), `rules/` → a managed section in `~/.codex/AGENTS.md` |
//! | opencode | `skills/` → `~/.opencode/skills/`, `.mcp.json` → `mcp.*`, `harness/opencode/plugin.js` → `~/.config/opencode/plugins/<pkg>.js`, `rules/` → a managed section in `~/.config/opencode/AGENTS.md` |
//! | cursor | `rules/*.md` → `~/.cursor/rules/<pkg>/*.mdc` (Cursor frontmatter; `harness/cursor/rules/<stem>.mdc` replaces a rendering), `.mcp.json` → `mcpServers.*` in `~/.cursor/mcp.json` |
//! | (all) | `skills/` → `~/.smooth/skills/` so `th` itself discovers them |
//!
//! Every written path (+ sha256), every owned dotted key in a merged config
//! file, every hook entry merged into a `hooks.json`, and every managed
//! `AGENTS.md` section is recorded in `~/.smooth/pkg/index.toml`, so `rm`
//! removes exactly what was installed and `status` reports drift. Hooks are
//! never translated between harnesses — they are per-harness customization
//! points (a `hooks.json` overlay is an explicit shim), and `status` says
//! which ones a package provides.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::mcp_install::{self, Harness, McpServer};

/// The local Claude marketplace `th pkg` publishes composed plugins into.
pub const CLAUDE_MARKETPLACE: &str = "th-pkg";

#[derive(Subcommand)]
pub enum Cmd {
    /// Install a package into one or more harnesses.
    ///
    /// SOURCE is a local path, `owner/repo[/subdir][#ref]` on GitHub, or a
    /// marketplace.json (path or URL) whose plugins are all installed.
    Install {
        source: String,
        /// claude-code | codex | opencode | cursor | all (comma-separated)
        #[arg(long, default_value = "all")]
        harness: String,
        /// Install for this user (the only scope in M0).
        #[arg(long, default_value_t = true)]
        global: bool,
    },
    /// List installed packages.
    List,
    /// Remove a package: every file, link and config key it installed.
    Rm { name: String },
    /// Report per-package health: drift, missing files, and which
    /// per-harness customization points the package provides.
    Status { name: Option<String> },
    /// Scaffold the package layout (core + harness overlays) in DIR.
    Init { dir: Option<PathBuf> },
}

/// # Errors
/// Returns an error when the source can't be fetched or parsed, or a
/// harness config is malformed (never silently clobbered).
pub fn cmd(cmd: Cmd) -> Result<()> {
    let paths = Paths::new(mcp_install::harness_home()?);
    match cmd {
        Cmd::Install { source, harness, global: _ } => {
            let harnesses = parse_harnesses(&harness)?;
            let names = install(&paths, &Source::parse(&source)?, &harnesses)?;
            for n in names {
                println!("{} installed {n}", "✓".bright_green());
            }
            Ok(())
        }
        Cmd::List => {
            let index = Index::load(&paths)?;
            if index.packages.is_empty() {
                println!("no packages installed — th pkg install <source>");
            }
            for (name, rec) in &index.packages {
                println!(
                    "{} {} [{}] {}",
                    name.bold(),
                    rec.version.as_deref().unwrap_or("-"),
                    rec.harnesses.join(","),
                    rec.source.dimmed()
                );
            }
            Ok(())
        }
        Cmd::Rm { name } => {
            let warnings = rm(&paths, &name)?;
            for w in warnings {
                println!("   {} {w}", "!".bright_yellow());
            }
            println!("{} removed {name}", "✓".bright_green());
            Ok(())
        }
        Cmd::Status { name } => {
            let index = Index::load(&paths)?;
            let mut any = false;
            for (n, rec) in &index.packages {
                if name.as_deref().is_some_and(|want| want != n) {
                    continue;
                }
                any = true;
                println!("{}", format!("== {n} {}", rec.version.as_deref().unwrap_or("")).bold().bright_cyan());
                for line in status_lines(rec) {
                    println!("   {line}");
                }
            }
            if !any {
                println!("no such package");
            }
            Ok(())
        }
        Cmd::Init { dir } => {
            let dir = dir.unwrap_or_else(|| PathBuf::from("."));
            init(&dir)?;
            println!("{} scaffolded package at {}", "✓".bright_green(), dir.display());
            Ok(())
        }
    }
}

fn parse_harnesses(spec: &str) -> Result<Vec<Harness>> {
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if part.eq_ignore_ascii_case("all") {
            return Ok(Harness::ALL.to_vec());
        }
        let h = Harness::parse(part)?;
        if !out.contains(&h) {
            out.push(h);
        }
    }
    if out.is_empty() {
        bail!("no harness given (expected claude-code|codex|opencode|cursor|all)");
    }
    Ok(out)
}

// ------------------------------------------------------------------ paths ----

/// Every location `th pkg` reads or writes, rooted at one home directory so
/// tests (and `$SMOOTH_HARNESS_HOME`) can relocate the whole tree.
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    #[must_use]
    pub const fn new(home: PathBuf) -> Self {
        Self { home }
    }
    fn root(&self) -> PathBuf {
        self.home.join(".smooth").join("pkg")
    }
    fn cache(&self) -> PathBuf {
        self.root().join("cache")
    }
    fn index_file(&self) -> PathBuf {
        self.root().join("index.toml")
    }
    fn claude_marketplace(&self) -> PathBuf {
        self.root().join("claude-marketplace")
    }
    fn claude_settings(&self) -> PathBuf {
        self.home.join(".claude").join("settings.json")
    }
    fn claude_rules(&self) -> PathBuf {
        self.home.join(".claude").join("rules")
    }
    fn smooth_skills(&self) -> PathBuf {
        self.home.join(".smooth").join("skills")
    }
    fn codex_skills(&self) -> PathBuf {
        self.home.join(".codex").join("skills")
    }
    /// `~/.opencode/skills/` — what `th skills` and `th harness` already use.
    fn opencode_skills(&self) -> PathBuf {
        self.home.join(".opencode").join("skills")
    }
    fn opencode_plugins(&self) -> PathBuf {
        self.home.join(".config").join("opencode").join("plugins")
    }
    /// Codex ≥ 0.153 reads Claude-style hooks from here.
    fn codex_hooks(&self) -> PathBuf {
        self.home.join(".codex").join("hooks.json")
    }
    /// Codex's personal global guidance file.
    fn codex_agents_md(&self) -> PathBuf {
        self.home.join(".codex").join("AGENTS.md")
    }
    /// OpenCode's global rules file.
    fn opencode_agents_md(&self) -> PathBuf {
        self.home.join(".config").join("opencode").join("AGENTS.md")
    }
    /// Cursor scans nested `.mdc` rules under here.
    fn cursor_rules(&self) -> PathBuf {
        self.home.join(".cursor").join("rules")
    }
}

// ----------------------------------------------------------------- source ----

/// Where a package comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A plugin directory, a marketplace directory, or a marketplace.json file.
    Path(PathBuf),
    /// `owner/repo[/subdir][#ref]` on GitHub.
    GitHub {
        owner: String,
        repo: String,
        subdir: Option<String>,
        git_ref: Option<String>,
    },
    /// A marketplace.json served over HTTP(S).
    MarketplaceUrl(String),
}

impl Source {
    /// Parse a CLI source spec. An existing local path wins; otherwise a URL
    /// ending in `marketplace.json` is a marketplace, a `github.com` URL or a
    /// bare `owner/repo[/subdir][#ref]` is GitHub.
    ///
    /// # Errors
    /// Returns an error for anything that is none of those.
    pub fn parse(spec: &str) -> Result<Self> {
        let spec = spec.trim();
        if spec.is_empty() {
            bail!("empty source");
        }
        let as_path = Path::new(spec);
        if as_path.exists() || spec.starts_with('.') || spec.starts_with('/') || spec.starts_with('~') {
            let p = if let Some(rest) = spec.strip_prefix("~/") {
                mcp_install::harness_home()?.join(rest)
            } else {
                as_path.to_path_buf()
            };
            return Ok(Self::Path(p));
        }
        if spec.starts_with("http://") || spec.starts_with("https://") {
            if spec.ends_with("marketplace.json") {
                return Ok(Self::MarketplaceUrl(spec.to_string()));
            }
            let rest = spec
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .strip_prefix("github.com/")
                .with_context(|| format!("unsupported URL '{spec}' (expected a github.com repo or a marketplace.json)"))?;
            return Self::parse_github(rest.trim_end_matches(".git"));
        }
        Self::parse_github(spec)
    }

    fn parse_github(spec: &str) -> Result<Self> {
        let (path, git_ref) = match spec.split_once('#') {
            Some((p, r)) => (p, (!r.is_empty()).then(|| r.to_string())),
            None => (spec, None),
        };
        let mut parts = path.split('/').filter(|s| !s.is_empty());
        let owner = parts
            .next()
            .filter(|s| valid_segment(s))
            .with_context(|| format!("'{spec}' is not owner/repo[/subdir][#ref] or an existing path"))?;
        let repo = parts
            .next()
            .filter(|s| valid_segment(s))
            .with_context(|| format!("'{spec}' is missing the repo (owner/repo[/subdir][#ref])"))?;
        let subdir: Vec<&str> = parts.collect();
        if subdir.contains(&"..") {
            bail!("'{spec}': subdir may not contain '..'");
        }
        Ok(Self::GitHub {
            owner: owner.to_string(),
            repo: repo.to_string(),
            subdir: (!subdir.is_empty()).then(|| subdir.join("/")),
            git_ref,
        })
    }
}

fn valid_segment(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(p) => write!(f, "path:{}", p.display()),
            Self::GitHub { owner, repo, subdir, git_ref } => {
                write!(f, "github:{owner}/{repo}")?;
                if let Some(s) = subdir {
                    write!(f, "/{s}")?;
                }
                if let Some(r) = git_ref {
                    write!(f, "#{r}")?;
                }
                Ok(())
            }
            Self::MarketplaceUrl(u) => write!(f, "marketplace:{u}"),
        }
    }
}

// --------------------------------------------------------------- manifest ----

/// What `.claude-plugin/plugin.json` + `.mcp.json` tell us about a package.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub mcp_servers: Vec<McpServer>,
}

/// Validate a package root and read its manifest.
///
/// # Errors
/// Returns an error if `plugin.json` is missing, malformed, or has an invalid
/// name.
pub fn load_manifest(root: &Path) -> Result<Manifest> {
    let pj = root.join(".claude-plugin").join("plugin.json");
    let raw = std::fs::read_to_string(&pj).with_context(|| format!("{} is not a package: no {}", root.display(), pj.display()))?;
    let doc: serde_json::Value = serde_json::from_str(&raw).with_context(|| format!("parse {}", pj.display()))?;
    let name = doc
        .get("name")
        .and_then(serde_json::Value::as_str)
        .context("plugin.json has no `name`")?
        .to_string();
    if !valid_segment(&name) || !name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
        bail!("plugin.json name '{name}' must be [a-z0-9][a-z0-9._-]*");
    }
    let mut mcp_servers = Vec::new();
    for servers in [doc.get("mcpServers"), read_mcp_json(root)?.as_ref().and_then(|d| d.get("mcpServers"))]
        .into_iter()
        .flatten()
    {
        if let Some(map) = servers.as_object() {
            for (sname, entry) in map {
                let Some(command) = entry.get("command").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let subst = |s: &str| s.replace("${CLAUDE_PLUGIN_ROOT}", &root.display().to_string());
                let server = McpServer {
                    name: sname.clone(),
                    command: subst(command),
                    args: entry
                        .get("args")
                        .and_then(serde_json::Value::as_array)
                        .map(|a| a.iter().filter_map(serde_json::Value::as_str).map(subst).collect())
                        .unwrap_or_default(),
                    env: entry
                        .get("env")
                        .and_then(serde_json::Value::as_object)
                        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), subst(v)))).collect())
                        .unwrap_or_default(),
                };
                mcp_servers.retain(|s: &McpServer| s.name != server.name);
                mcp_servers.push(server);
            }
        }
    }
    Ok(Manifest {
        name,
        version: doc.get("version").and_then(serde_json::Value::as_str).map(str::to_string),
        description: doc.get("description").and_then(serde_json::Value::as_str).map(str::to_string),
        mcp_servers,
    })
}

fn read_mcp_json(root: &Path) -> Result<Option<serde_json::Value>> {
    let p = root.join(".mcp.json");
    if !p.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&p)?;
    Ok(Some(serde_json::from_str(&raw).with_context(|| format!("parse {}", p.display()))?))
}

/// Is this directory a package root?
fn is_package(dir: &Path) -> bool {
    dir.join(".claude-plugin").join("plugin.json").is_file()
}

/// A marketplace.json's plugin list, resolved to sources.
///
/// Relative `source` strings resolve against `base` (the marketplace's
/// directory); `{source:"github",repo}` objects and `owner/repo` strings go
/// to GitHub. Anything else is reported, not guessed.
///
/// # Errors
/// Returns an error if the document is malformed.
pub fn marketplace_sources(doc: &serde_json::Value, base: Option<&Path>) -> Result<Vec<(String, Source)>> {
    let plugins = doc
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .context("marketplace.json has no `plugins` array")?;
    let mut out = Vec::new();
    for p in plugins {
        let name = p
            .get("name")
            .and_then(serde_json::Value::as_str)
            .context("marketplace plugin without a name")?
            .to_string();
        let src = match p.get("source") {
            Some(serde_json::Value::String(s)) if s.starts_with("./") || s.starts_with("../") => {
                let base = base.with_context(|| format!("plugin '{name}' has a relative source but the marketplace was fetched by URL"))?;
                Source::Path(base.join(s))
            }
            Some(serde_json::Value::String(s)) => Source::parse(s)?,
            Some(serde_json::Value::Object(o)) => match o.get("source").and_then(serde_json::Value::as_str) {
                Some("github") => Source::parse_github(&format!(
                    "{}{}",
                    o.get("repo").and_then(serde_json::Value::as_str).context("github source without repo")?,
                    o.get("ref").and_then(serde_json::Value::as_str).map(|r| format!("#{r}")).unwrap_or_default()
                ))?,
                Some("directory") => Source::Path(PathBuf::from(
                    o.get("path").and_then(serde_json::Value::as_str).context("directory source without path")?,
                )),
                other => bail!("plugin '{name}': unsupported marketplace source kind {other:?}"),
            },
            _ => bail!("plugin '{name}' has no source"),
        };
        out.push((name, src));
    }
    Ok(out)
}

// ------------------------------------------------------------------ fetch ----

/// A package root in the cache, plus where it came from.
struct Fetched {
    root: PathBuf,
    source: String,
    /// `(marketplace name, owner/repo)` when the package was found through a
    /// GitHub repo's own marketplace.json — Claude can then be handed the real
    /// thing instead of our local copy.
    github_marketplace: Option<(String, String)>,
}

/// Fetch every package a source names into the cache.
fn fetch(paths: &Paths, source: &Source) -> Result<Vec<Fetched>> {
    std::fs::create_dir_all(paths.cache())?;
    match source {
        Source::Path(p) => {
            let p = p.canonicalize().with_context(|| format!("{} does not exist", p.display()))?;
            if is_package(&p) {
                let m = load_manifest(&p)?;
                let dest = paths.cache().join(format!("{}@local", m.name));
                copy_dir(&p, &dest, true)?;
                return Ok(vec![Fetched {
                    root: dest,
                    source: Source::Path(p).to_string(),
                    github_marketplace: None,
                }]);
            }
            let mp = if p.is_file() {
                p.clone()
            } else {
                p.join(".claude-plugin").join("marketplace.json")
            };
            if !mp.is_file() {
                bail!(
                    "{} is neither a package (.claude-plugin/plugin.json) nor a marketplace (.claude-plugin/marketplace.json)",
                    p.display()
                );
            }
            let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&mp)?).with_context(|| format!("parse {}", mp.display()))?;
            let base = mp.parent().and_then(Path::parent).map_or_else(|| p.clone(), Path::to_path_buf);
            let mut out = Vec::new();
            for (_, src) in marketplace_sources(&doc, Some(&base))? {
                out.extend(fetch(paths, &src)?);
            }
            Ok(out)
        }
        Source::GitHub { owner, repo, subdir, git_ref } => fetch_github(paths, source, owner, repo, subdir.as_deref(), git_ref.as_deref()),
        Source::MarketplaceUrl(url) => {
            let body = reqwest::blocking::get(url).with_context(|| format!("GET {url}"))?.error_for_status()?.text()?;
            let doc: serde_json::Value = serde_json::from_str(&body).with_context(|| format!("parse {url} as marketplace.json"))?;
            let mut out = Vec::new();
            for (_, src) in marketplace_sources(&doc, None)? {
                out.extend(fetch(paths, &src)?);
            }
            Ok(out)
        }
    }
}

/// `git clone --depth 1` into a temp dir under the cache, then copy the
/// package (or every plugin its marketplace lists) into `cache/<name>@<ref>`.
fn fetch_github(paths: &Paths, source: &Source, owner: &str, repo: &str, subdir: Option<&str>, git_ref: Option<&str>) -> Result<Vec<Fetched>> {
    let tmp = paths.cache().join(format!(".clone-{}-{}", std::process::id(), rand::random::<u32>()));
    let _ = std::fs::remove_dir_all(&tmp);
    let url = format!("https://github.com/{owner}/{repo}.git");
    let mut cmd = std::process::Command::new("git");
    cmd.args(["clone", "--depth", "1", "--quiet"]);
    if let Some(r) = git_ref {
        cmd.args(["--branch", r]);
    }
    let out = cmd.arg(&url).arg(&tmp).output().context("run git clone")?;
    if !out.status.success() {
        bail!("git clone {url} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let result = (|| {
        let at = subdir.map_or_else(|| tmp.clone(), |s| tmp.join(s));
        let tag = git_ref.unwrap_or("HEAD");
        let mut out = Vec::new();
        if is_package(&at) {
            let m = load_manifest(&at)?;
            let dest = paths.cache().join(format!("{}@{tag}", m.name));
            copy_dir(&at, &dest, true)?;
            // Handed through the repo's own marketplace when it lists this plugin.
            let github_marketplace = repo_marketplace(&tmp)?
                .filter(|(_, names)| names.contains(&m.name))
                .map(|(mname, _)| (mname, format!("{owner}/{repo}")));
            out.push(Fetched {
                root: dest,
                source: source.to_string(),
                github_marketplace,
            });
        } else {
            let mp = at.join(".claude-plugin").join("marketplace.json");
            if !mp.is_file() {
                bail!(
                    "{url}{} has neither a plugin.json nor a marketplace.json",
                    subdir.map(|s| format!("/{s}")).unwrap_or_default()
                );
            }
            let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&mp)?)?;
            let mname = doc.get("name").and_then(serde_json::Value::as_str).unwrap_or(repo).to_string();
            for (pname, src) in marketplace_sources(&doc, Some(&at))? {
                match src {
                    Source::Path(p) if p.starts_with(&tmp) => {
                        let m = load_manifest(&p)?;
                        let dest = paths.cache().join(format!("{}@{tag}", m.name));
                        copy_dir(&p, &dest, true)?;
                        out.push(Fetched {
                            root: dest,
                            source: format!("{source}/{pname}"),
                            github_marketplace: Some((mname.clone(), format!("{owner}/{repo}"))),
                        });
                    }
                    other => out.extend(fetch(paths, &other)?),
                }
            }
        }
        Ok(out)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// `(marketplace name, plugin names)` from a clone root's marketplace.json.
fn repo_marketplace(root: &Path) -> Result<Option<(String, Vec<String>)>> {
    let mp = root.join(".claude-plugin").join("marketplace.json");
    if !mp.is_file() {
        return Ok(None);
    }
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&mp)?).with_context(|| format!("parse {}", mp.display()))?;
    let name = doc.get("name").and_then(serde_json::Value::as_str).map(str::to_string);
    let names = doc
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| p.get("name").and_then(serde_json::Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(name.map(|n| (n, names)))
}

// ------------------------------------------------------------------ index ----

/// `~/.smooth/pkg/index.toml` — provenance for everything `th pkg` wrote.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Index {
    #[serde(default)]
    pub packages: BTreeMap<String, Installed>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[allow(clippy::struct_field_names)] // `installed_at` reads right in index.toml
pub struct Installed {
    pub version: Option<String>,
    pub source: String,
    pub installed_at: String,
    /// The cached package root the renderings link into.
    pub root: PathBuf,
    pub harnesses: Vec<String>,
    /// `enabledPlugins` key when Claude's plugin system owns the rendering.
    pub claude_plugin: Option<String>,
    /// Human-readable decisions made at install time (skips, handoffs).
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub files: Vec<OwnedFile>,
    #[serde(default)]
    pub keys: Vec<OwnedKey>,
    /// Hook entries key-merged into a harness `hooks.json` (M1).
    #[serde(default)]
    pub hooks: Vec<OwnedHook>,
    /// Managed `AGENTS.md` sections (M1).
    #[serde(default)]
    pub sections: Vec<OwnedSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnedFile {
    pub harness: String,
    pub path: PathBuf,
    /// `symlink` (sha256 of the link target) | `file` (sha256 of content) | `dir` (copied tree, no hash)
    pub kind: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnedKey {
    pub harness: String,
    pub file: PathBuf,
    /// Key path, one segment per element (segments may contain dots).
    pub key: Vec<String>,
}

/// One hook entry we appended to `hooks.<event>[matcher].hooks` in a
/// Claude-style `hooks.json`. Identified by its command — an identical
/// command that was already there is the user's, never ours.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnedHook {
    pub harness: String,
    pub file: PathBuf,
    pub event: String,
    /// `""` when the group has no matcher (Claude treats absent and empty alike).
    #[serde(default)]
    pub matcher: String,
    pub command: String,
}

/// A `<!-- th-pkg:<name> -->` … `<!-- /th-pkg:<name> -->` block in an
/// `AGENTS.md`. `created` = the file did not exist before we wrote it, so
/// `rm` may delete it again once the block is gone and nothing else is left.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnedSection {
    pub harness: String,
    pub file: PathBuf,
    pub name: String,
    pub sha256: String,
    #[serde(default)]
    pub created: bool,
}

impl Index {
    /// # Errors
    /// Returns an error if the index exists but can't be parsed.
    pub fn load(paths: &Paths) -> Result<Self> {
        let p = paths.index_file();
        if !p.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        toml::from_str(&raw).with_context(|| format!("parse {} — fix or delete it", p.display()))
    }

    /// # Errors
    /// Returns an error if the index can't be written.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        std::fs::create_dir_all(paths.root())?;
        std::fs::write(paths.index_file(), toml::to_string_pretty(self)?).with_context(|| format!("write {}", paths.index_file().display()))
    }
}

// ---------------------------------------------------------------- install ----

/// Install every package `source` names for `harnesses`. Returns the names.
///
/// # Errors
/// See [`cmd`].
pub fn install(paths: &Paths, source: &Source, harnesses: &[Harness]) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for f in fetch(paths, source)? {
        names.push(install_root(paths, &f, harnesses)?);
    }
    Ok(names)
}

fn install_root(paths: &Paths, fetched: &Fetched, harnesses: &[Harness]) -> Result<String> {
    let root = &fetched.root;
    let m = load_manifest(root)?;
    let mut index = Index::load(paths)?;
    // Reinstall = remove the previous rendering first, so a package that
    // dropped a skill doesn't leave its link behind. Harnesses accumulate:
    // `--harness codex` on a package already rendered for opencode keeps
    // opencode (that is how `th harness enable` adds one harness at a time).
    let mut harnesses = harnesses.to_vec();
    let mut prev_sections = Vec::new();
    if let Some(prev) = index.packages.remove(&m.name) {
        // Managed AGENTS.md blocks are left for the re-render to replace IN
        // PLACE (so a block in the middle of the file stays there); any block
        // the new version no longer renders is removed below.
        for w in remove_rendered(paths, &prev, true)? {
            println!("   {} {w}", "!".bright_yellow());
        }
        for h in prev.harnesses.iter().filter_map(|h| Harness::parse(h).ok()) {
            if !harnesses.contains(&h) {
                harnesses.push(h);
            }
        }
        prev_sections = prev.sections;
    }
    let harnesses = &harnesses;
    let mut rec = Installed {
        version: m.version.clone(),
        source: fetched.source.clone(),
        installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        root: root.clone(),
        harnesses: harnesses.iter().map(ToString::to_string).collect(),
        ..Installed::default()
    };
    println!("{}", format!("== {} {}", m.name, m.version.as_deref().unwrap_or("")).bold().bright_cyan());

    // Smooth's own discovery, regardless of harness.
    link_skills(root, &paths.smooth_skills(), "smooth", &mut rec)?;

    for h in harnesses {
        if !h.marker_dir(&paths.home).is_dir() {
            note(&mut rec, format!("{h}: not installed on this machine — skipped"));
            continue;
        }
        match h {
            Harness::ClaudeCode => render_claude_code(paths, root, &m, fetched, &mut rec)?,
            Harness::Codex => render_codex(paths, root, &m, &mut rec)?,
            Harness::OpenCode => render_opencode(paths, root, &m, &mut rec)?,
            Harness::Cursor => render_cursor(paths, root, &m, &mut rec)?,
        }
    }
    for old in prev_sections {
        match rec.sections.iter_mut().find(|s| s.file == old.file && s.name == old.name) {
            // Still rendered: a file WE created stays ours to delete on rm.
            Some(cur) => cur.created |= old.created,
            None if old.file.exists() => {
                remove_section(&old.file, &old.name, old.created)?;
            }
            None => {}
        }
    }
    index.packages.insert(m.name.clone(), rec);
    index.save(paths)?;
    write_claude_marketplace(paths, &index)?;
    Ok(m.name)
}

fn note(rec: &mut Installed, msg: String) {
    println!("   {msg}");
    rec.notes.push(msg);
}

/// Symlink every `skills/<n>` into `dir` (copy on non-unix). Never clobbers a
/// real file or directory the user owns; repairs stale symlinks.
fn link_skills(root: &Path, dir: &Path, harness: &str, rec: &mut Installed) -> Result<()> {
    let skills = root.join("skills");
    let Ok(entries) = std::fs::read_dir(&skills) else { return Ok(()) };
    let mut n = 0;
    for e in entries.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        if let Some(f) = place_link(&e.path(), &dir.join(e.file_name()), harness)? {
            rec.files.push(f);
            n += 1;
        } else {
            note(
                rec,
                format!("{harness}: {} exists and is not ours — left alone", dir.join(e.file_name()).display()),
            );
        }
    }
    if n > 0 {
        println!("   {harness}: {n} skills → {}", dir.display());
    }
    Ok(())
}

/// Symlink `src` at `dst`, replacing only an existing symlink. `None` when a
/// real file/dir is in the way.
fn place_link(src: &Path, dst: &Path, harness: &str) -> Result<Option<OwnedFile>> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    match std::fs::symlink_metadata(dst) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(dst)?,
        Ok(_) => return Ok(None),
        Err(_) => {}
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst).with_context(|| format!("link {}", dst.display()))?;
        Ok(Some(OwnedFile {
            harness: harness.to_string(),
            path: dst.to_path_buf(),
            kind: "symlink".into(),
            sha256: sha256_str(&src.display().to_string()),
        }))
    }
    #[cfg(not(unix))]
    {
        // ponytail: no symlinks without privileges on Windows — copy, and
        // `status` can't detect drift inside a copied tree.
        if src.is_dir() {
            copy_dir(src, dst, true)?;
            Ok(Some(OwnedFile {
                harness: harness.to_string(),
                path: dst.to_path_buf(),
                kind: "dir".into(),
                sha256: String::new(),
            }))
        } else {
            std::fs::copy(src, dst)?;
            Ok(Some(OwnedFile {
                harness: harness.to_string(),
                path: dst.to_path_buf(),
                kind: "file".into(),
                sha256: sha256_file(dst)?,
            }))
        }
    }
}

/// Copy `src` to `dst` (overwriting) and record it.
fn place_copy(src: &Path, dst: &Path, harness: &str) -> Result<OwnedFile> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dst).with_context(|| format!("copy {} → {}", src.display(), dst.display()))?;
    Ok(OwnedFile {
        harness: harness.to_string(),
        path: dst.to_path_buf(),
        kind: "file".into(),
        sha256: sha256_file(dst)?,
    })
}

fn render_claude_code(paths: &Paths, root: &Path, m: &Manifest, fetched: &Fetched, rec: &mut Installed) -> Result<()> {
    let h = "claude-code";
    // rules/ is ours to render — Claude's plugin layout has no rules dir.
    if let Ok(entries) = std::fs::read_dir(root.join("rules")) {
        let mut n = 0;
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "md") {
                rec.files
                    .push(place_copy(&e.path(), &paths.claude_rules().join(&m.name).join(e.file_name()), h)?);
                n += 1;
            }
        }
        if n > 0 {
            println!("   {h}: {n} rules → {}", paths.claude_rules().join(&m.name).display());
        }
    }

    let settings = paths.claude_settings();
    let doc = load_json(&settings)?;
    if let Some(existing) = doc.get("enabledPlugins").and_then(serde_json::Value::as_object).and_then(|plugins| {
        plugins
            .keys()
            .find(|k| k.split_once('@').is_some_and(|(n, _)| n == m.name) && !k.ends_with(&format!("@{CLAUDE_MARKETPLACE}")))
    }) {
        note(rec, format!("{h}: already enabled in Claude as {existing} — not registering a second copy"));
        return Ok(());
    }

    let plugin_key = if let Some((mname, repo)) = &fetched.github_marketplace {
        // The repo ships its own marketplace: hand Claude the real thing.
        let mut doc = doc;
        // Only a marketplace WE registered is ours to remove on `rm`.
        if doc.get("extraKnownMarketplaces").and_then(|m| m.get(mname)).is_none() {
            json_set(
                &mut doc,
                &["extraKnownMarketplaces", mname],
                serde_json::json!({"source": {"source": "github", "repo": repo}}),
            );
            rec.keys.push(OwnedKey {
                harness: h.into(),
                file: settings.clone(),
                key: vec!["extraKnownMarketplaces".into(), mname.clone()],
            });
        }
        let key = format!("{}@{mname}", m.name);
        json_set(&mut doc, &["enabledPlugins", &key], serde_json::json!(true));
        save_json(&settings, &doc)?;
        key
    } else {
        // Compose core + harness/claude-code overlay into the local marketplace.
        let composed = paths.claude_marketplace().join("plugins").join(&m.name);
        copy_dir(root, &composed, false)?;
        let overlay = root.join("harness").join(h);
        if overlay.is_dir() {
            overlay_dir(&overlay, &composed, true)?;
            // M1: hooks.json is KEY-MERGED, not replaced — the overlay adds to
            // the core's events/matchers and drops nothing.
            let (core_hooks, over_hooks) = (root.join("hooks").join("hooks.json"), overlay.join("hooks").join("hooks.json"));
            if core_hooks.is_file() && over_hooks.is_file() {
                let mut merged = load_json(&core_hooks)?;
                let added = merge_hooks(&mut merged, &load_json(&over_hooks)?);
                save_json(&composed.join("hooks").join("hooks.json"), &merged)?;
                println!("   {h}: hooks.json = core + overlay ({} hooks added by the overlay)", added.len());
            }
        }
        let mut doc = doc;
        json_set(
            &mut doc,
            &["extraKnownMarketplaces", CLAUDE_MARKETPLACE],
            serde_json::json!({"source": {"source": "directory", "path": paths.claude_marketplace()}}),
        );
        let key = format!("{}@{CLAUDE_MARKETPLACE}", m.name);
        json_set(&mut doc, &["enabledPlugins", &key], serde_json::json!(true));
        save_json(&settings, &doc)?;
        rec.files.push(OwnedFile {
            harness: h.into(),
            path: composed,
            kind: "dir".into(),
            sha256: String::new(),
        });
        key
    };
    rec.keys.push(OwnedKey {
        harness: h.into(),
        file: settings,
        key: vec!["enabledPlugins".into(), plugin_key.clone()],
    });
    rec.claude_plugin = Some(plugin_key.clone());
    note(
        rec,
        format!("{h}: handed to Claude's plugin system as {plugin_key} (skills, commands, agents, hooks, MCP load natively)"),
    );
    Ok(())
}

fn render_codex(paths: &Paths, root: &Path, m: &Manifest, rec: &mut Installed) -> Result<()> {
    let h = "codex";
    link_skills(root, &paths.codex_skills(), h, rec)?;
    render_mcp(paths, Harness::Codex, m, rec)?;
    let frag = root.join("harness").join(h).join("config.toml");
    if frag.is_file() {
        let target = Harness::Codex.config_path(&paths.home);
        let keys = toml_merge_fragment(&target, &std::fs::read_to_string(&frag)?)?;
        println!("   {h}: {} keys merged into {}", keys.len(), target.display());
        rec.keys.extend(keys.into_iter().map(|key| OwnedKey {
            harness: h.into(),
            file: target.clone(),
            key,
        }));
    }
    // Codex ≥ 0.153 loads Claude-style hooks from ~/.codex/hooks.json. The
    // overlay is an explicit per-harness shim (never the core hooks.json —
    // hooks are not translated), key-merged so the user's own entries and
    // any identical command already there stay theirs.
    render_hooks_overlay(root, h, &paths.codex_hooks(), rec)?;
    render_agents_section(root, m, h, &paths.codex_agents_md(), rec)?;
    Ok(())
}

fn render_opencode(paths: &Paths, root: &Path, m: &Manifest, rec: &mut Installed) -> Result<()> {
    let h = "opencode";
    link_skills(root, &paths.opencode_skills(), h, rec)?;
    render_mcp(paths, Harness::OpenCode, m, rec)?;
    let plugin = root.join("harness").join(h).join("plugin.js");
    if plugin.is_file() {
        let dst = paths.opencode_plugins().join(format!("{}.js", m.name));
        match place_link(&plugin, &dst, h)? {
            Some(f) => {
                println!("   {h}: plugin → {}", dst.display());
                rec.files.push(f);
            }
            None => note(rec, format!("{h}: {} exists and is not ours — left alone", dst.display())),
        }
    }
    render_agents_section(root, m, h, &paths.opencode_agents_md(), rec)?;
    Ok(())
}

/// Cursor has no plugin system: `rules/*.md` become `.mdc` rules with Cursor
/// frontmatter under `~/.cursor/rules/<pkg>/`; a `harness/cursor/rules/<stem>.mdc`
/// overlay replaces the rendering for that stem (and extra `.mdc` files there
/// are copied as they are). MCP goes to `~/.cursor/mcp.json`.
fn render_cursor(paths: &Paths, root: &Path, m: &Manifest, rec: &mut Installed) -> Result<()> {
    let h = "cursor";
    render_mcp(paths, Harness::Cursor, m, rec)?;
    let dir = paths.cursor_rules().join(&m.name);
    let overlay = root.join("harness").join(h).join("rules");
    let mut n = 0;
    for rule in load_rules(root)? {
        let dst = dir.join(format!("{}.mdc", rule.stem));
        let over = overlay.join(format!("{}.mdc", rule.stem));
        if over.is_file() {
            rec.files.push(place_copy(&over, &dst, h)?);
        } else {
            rec.files.push(write_owned(&dst, &rule.to_mdc(&m.name), h)?);
        }
        n += 1;
    }
    if let Ok(entries) = std::fs::read_dir(&overlay) {
        for e in entries.flatten() {
            let dst = dir.join(e.file_name());
            if e.path().extension().is_some_and(|x| x == "mdc") && !rec.files.iter().any(|f| f.path == dst) {
                rec.files.push(place_copy(&e.path(), &dst, h)?);
                n += 1;
            }
        }
    }
    if n > 0 {
        println!("   {h}: {n} rules → {}", dir.display());
    }
    Ok(())
}

/// Key-merge `harness/<h>/hooks.json` into a harness's Claude-style hooks
/// file, substituting `${CLAUDE_PLUGIN_ROOT}` with the cached package root.
/// Only the entries that were actually added become ours.
fn render_hooks_overlay(root: &Path, h: &str, target: &Path, rec: &mut Installed) -> Result<()> {
    let overlay = root.join("harness").join(h).join("hooks.json");
    if !overlay.is_file() {
        return Ok(());
    }
    let mut add = load_json(&overlay)?;
    subst_plugin_root(&mut add, root);
    let mut doc = load_json(target)?;
    let added = merge_hooks(&mut doc, &add);
    save_json(target, &doc)?;
    println!("   {h}: {} hooks merged into {}", added.len(), target.display());
    rec.hooks.extend(added.into_iter().map(|(event, matcher, command)| OwnedHook {
        harness: h.into(),
        file: target.to_path_buf(),
        event,
        matcher,
        command,
    }));
    Ok(())
}

/// Render `rules/*.md` as one marker-delimited block in a harness's global
/// `AGENTS.md`. Idempotent: an existing block for this package is replaced
/// in place; text outside the markers is never touched.
fn render_agents_section(root: &Path, m: &Manifest, h: &str, file: &Path, rec: &mut Installed) -> Result<()> {
    let rules = load_rules(root)?;
    if rules.is_empty() {
        return Ok(());
    }
    let block = render_section(&m.name, &rules);
    let created = upsert_section(file, &m.name, &block)?;
    println!("   {h}: {} rules → managed section in {}", rules.len(), file.display());
    rec.sections.push(OwnedSection {
        harness: h.into(),
        file: file.to_path_buf(),
        name: m.name.clone(),
        sha256: sha256_str(&block),
        created,
    });
    Ok(())
}

fn render_mcp(paths: &Paths, harness: Harness, m: &Manifest, rec: &mut Installed) -> Result<()> {
    for s in &m.mcp_servers {
        mcp_install::install_server_into(harness, &paths.home, s, false)?;
        let table = match harness {
            Harness::Codex => "mcp_servers",
            Harness::OpenCode => "mcp",
            Harness::ClaudeCode | Harness::Cursor => "mcpServers",
        };
        rec.keys.push(OwnedKey {
            harness: harness.to_string(),
            file: harness.config_path(&paths.home),
            key: vec![table.into(), s.name.clone()],
        });
        println!("   {harness}: mcp server `{}` → {}", s.name, harness.config_path(&paths.home).display());
    }
    Ok(())
}

/// Regenerate the local Claude marketplace from the index: one entry per
/// package Claude loads from our composed copy. Removes the marketplace
/// registration when nothing is left in it.
fn write_claude_marketplace(paths: &Paths, index: &Index) -> Result<()> {
    let suffix = format!("@{CLAUDE_MARKETPLACE}");
    let plugins: Vec<serde_json::Value> = index
        .packages
        .iter()
        .filter(|(_, r)| r.claude_plugin.as_deref().is_some_and(|k| k.ends_with(&suffix)))
        .map(|(name, r)| {
            serde_json::json!({
                "name": name,
                "source": format!("./plugins/{name}"),
                "version": r.version,
                "description": load_manifest(&r.root).ok().and_then(|m| m.description),
            })
        })
        .collect();
    let dir = paths.claude_marketplace();
    if plugins.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        let settings = paths.claude_settings();
        if settings.is_file() {
            let mut doc = load_json(&settings)?;
            if json_remove(&mut doc, &["extraKnownMarketplaces", CLAUDE_MARKETPLACE]) {
                save_json(&settings, &doc)?;
            }
        }
        return Ok(());
    }
    let doc = serde_json::json!({
        "name": CLAUDE_MARKETPLACE,
        "owner": {"name": "th pkg"},
        "metadata": {"description": "Packages composed by `th pkg install` on this machine."},
        "plugins": plugins,
    });
    std::fs::create_dir_all(dir.join(".claude-plugin"))?;
    save_json(&dir.join(".claude-plugin").join("marketplace.json"), &doc)
}

// --------------------------------------------------------------------- rm ----

/// Remove a package and everything it rendered. Returns warnings for things
/// left in place (user-modified copies).
///
/// # Errors
/// Returns an error when the package is unknown or a config can't be edited.
pub fn rm(paths: &Paths, name: &str) -> Result<Vec<String>> {
    let mut index = Index::load(paths)?;
    let rec = index
        .packages
        .remove(name)
        .with_context(|| format!("'{name}' is not installed (th pkg list)"))?;
    let warnings = remove_rendered(paths, &rec, false)?;
    index.save(paths)?;
    write_claude_marketplace(paths, &index)?;
    let _ = std::fs::remove_dir_all(&rec.root);
    Ok(warnings)
}

/// Take back everything `rec` says we wrote. `keep_sections` leaves managed
/// AGENTS.md blocks for a reinstall to replace in place; otherwise they are
/// removed, except a block the user edited, which stays with a warning.
fn remove_rendered(paths: &Paths, rec: &Installed, keep_sections: bool) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    for f in &rec.files {
        match f.kind.as_str() {
            "symlink" => {
                if std::fs::symlink_metadata(&f.path).is_ok_and(|m| m.file_type().is_symlink()) {
                    std::fs::remove_file(&f.path).with_context(|| format!("remove {}", f.path.display()))?;
                }
            }
            "file" => {
                if f.path.is_file() {
                    if sha256_file(&f.path)? == f.sha256 {
                        std::fs::remove_file(&f.path)?;
                        if let Some(p) = f.path.parent() {
                            let _ = std::fs::remove_dir(p); // only succeeds when empty
                        }
                    } else {
                        warnings.push(format!("{} was modified after install — left in place", f.path.display()));
                    }
                }
            }
            _ => {
                if f.path.is_dir() {
                    std::fs::remove_dir_all(&f.path)?;
                }
            }
        }
    }
    for k in &rec.keys {
        if !k.file.exists() {
            continue;
        }
        let segs: Vec<&str> = k.key.iter().map(String::as_str).collect();
        if k.file.extension().is_some_and(|x| x == "toml") {
            toml_remove_key(&k.file, &segs)?;
        } else {
            let mut doc = load_json(&k.file)?;
            if json_remove(&mut doc, &segs) {
                save_json(&k.file, &doc)?;
            }
        }
    }
    for hk in &rec.hooks {
        if !hk.file.exists() {
            continue;
        }
        let mut doc = load_json(&hk.file)?;
        if remove_hook(&mut doc, &hk.event, &hk.matcher, &hk.command) {
            save_json(&hk.file, &doc)?;
        }
    }
    for sec in &rec.sections {
        if keep_sections || !sec.file.exists() {
            continue;
        }
        match section_state(sec) {
            Err(why) if why.starts_with("modified") => {
                warnings.push(format!(
                    "{}: managed section for {} was edited after install — left in place",
                    sec.file.display(),
                    sec.name
                ));
                continue;
            }
            _ => {}
        }
        remove_section(&sec.file, &sec.name, sec.created)?;
    }
    let _ = paths;
    Ok(warnings)
}

/// Remove one harness's rendering of a package (what `th harness disable <h>`
/// needs) and forget that harness in the index. When no harness is left the
/// whole package goes, cache included.
///
/// # Errors
/// Returns an error when a config can't be edited. An unknown package is not
/// an error — there is nothing to remove.
pub fn rm_harness(paths: &Paths, name: &str, harness: Harness) -> Result<Vec<String>> {
    let mut index = Index::load(paths)?;
    let Some(mut rec) = index.packages.remove(name) else {
        return Ok(Vec::new());
    };
    let h = harness.as_str();
    let part = Installed {
        files: rec.files.iter().filter(|f| f.harness == h).cloned().collect(),
        keys: rec.keys.iter().filter(|k| k.harness == h).cloned().collect(),
        hooks: rec.hooks.iter().filter(|k| k.harness == h).cloned().collect(),
        sections: rec.sections.iter().filter(|s| s.harness == h).cloned().collect(),
        ..Installed::default()
    };
    let mut warnings = remove_rendered(paths, &part, false)?;
    rec.files.retain(|f| f.harness != h);
    rec.keys.retain(|k| k.harness != h);
    rec.hooks.retain(|k| k.harness != h);
    rec.sections.retain(|s| s.harness != h);
    rec.notes.retain(|n| !n.starts_with(&format!("{h}:")));
    rec.harnesses.retain(|x| x != h);
    if harness == Harness::ClaudeCode {
        rec.claude_plugin = None;
    }
    if rec.harnesses.is_empty() {
        warnings.extend(remove_rendered(paths, &rec, false)?);
        let _ = std::fs::remove_dir_all(&rec.root);
    } else {
        index.packages.insert(name.to_string(), rec);
    }
    index.save(paths)?;
    write_claude_marketplace(paths, &index)?;
    Ok(warnings)
}

// ----------------------------------------------------------------- status ----

/// One line per rendered artifact and customization point.
#[must_use]
pub fn status_lines(rec: &Installed) -> Vec<String> {
    let mut out = vec![format!("source: {}  installed: {}", rec.source, rec.installed_at)];
    let (mut ok, mut bad) = (0, 0);
    for f in &rec.files {
        match file_state(f) {
            Ok(()) => ok += 1,
            Err(why) => {
                bad += 1;
                out.push(format!("{} {} — {why}", "✗".bright_red(), f.path.display()));
            }
        }
    }
    for k in &rec.keys {
        let present = key_present(k).unwrap_or(false);
        if present {
            ok += 1;
        } else {
            bad += 1;
            out.push(format!("{} {}:{} — key missing", "✗".bright_red(), k.file.display(), k.key.join(".")));
        }
    }
    for hk in &rec.hooks {
        if hook_present(hk).unwrap_or(false) {
            ok += 1;
        } else {
            bad += 1;
            out.push(format!(
                "{} {}:{}[{}] — hook `{}` missing",
                "✗".bright_red(),
                hk.file.display(),
                hk.event,
                hk.matcher,
                hk.command
            ));
        }
    }
    for sec in &rec.sections {
        match section_state(sec) {
            Ok(()) => ok += 1,
            Err(why) => {
                bad += 1;
                out.push(format!("{} {} — managed section {} {why}", "✗".bright_red(), sec.file.display(), sec.name));
            }
        }
    }
    out.push(format!(
        "{ok} artifacts ok, {bad} drifted/missing{}",
        if bad > 0 { " — reinstall to repair" } else { "" }
    ));
    if let Some(k) = &rec.claude_plugin {
        out.push(format!("claude-code: plugin {k}"));
    }
    out.push(format!("customization points: {}", customization_points(&rec.root).join(", ")));
    for n in &rec.notes {
        out.push(format!("note: {n}"));
    }
    out
}

/// Which per-harness customization points this package provides.
fn customization_points(root: &Path) -> Vec<String> {
    let has = |rel: &str| root.join(rel).exists();
    let point = |label: &str, present: bool| format!("{label}={}", if present { "present" } else { "absent" });
    vec![
        point("claude-code/hooks", has("hooks/hooks.json") || has("harness/claude-code/hooks/hooks.json")),
        point("codex/config.toml", has("harness/codex/config.toml")),
        point("codex/hooks.json", has("harness/codex/hooks.json")),
        point("opencode/plugin.js", has("harness/opencode/plugin.js")),
        point("cursor/rules", has("harness/cursor/rules")),
        point("rules", has("rules")),
    ]
}

fn file_state(f: &OwnedFile) -> std::result::Result<(), String> {
    match f.kind.as_str() {
        "symlink" => {
            let meta = std::fs::symlink_metadata(&f.path).map_err(|_| "missing".to_string())?;
            if !meta.file_type().is_symlink() {
                return Err("replaced by a real file".into());
            }
            let target = std::fs::read_link(&f.path).map_err(|e| e.to_string())?;
            if sha256_str(&target.display().to_string()) != f.sha256 {
                return Err(format!("points elsewhere ({})", target.display()));
            }
            if !target.exists() {
                return Err("dangling (package cache gone)".into());
            }
            Ok(())
        }
        "file" => {
            if !f.path.is_file() {
                return Err("missing".into());
            }
            if sha256_file(&f.path).map_err(|e| e.to_string())? != f.sha256 {
                return Err("modified (hash mismatch)".into());
            }
            Ok(())
        }
        _ => f.path.is_dir().then_some(()).ok_or_else(|| "missing".into()),
    }
}

fn hook_present(hk: &OwnedHook) -> Result<bool> {
    let doc = load_json(&hk.file)?;
    Ok(doc
        .get("hooks")
        .and_then(|h| h.get(&hk.event))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|groups| {
            groups.iter().any(|g| {
                hook_matcher(g) == hk.matcher
                    && g.get("hooks")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|hs| hs.iter().any(|h| hook_id(h) == hk.command))
            })
        }))
}

fn section_state(sec: &OwnedSection) -> std::result::Result<(), String> {
    let text = std::fs::read_to_string(&sec.file).map_err(|_| "missing (file gone)".to_string())?;
    let (a, b) = find_section(&text, &sec.name).ok_or_else(|| "missing".to_string())?;
    if sha256_str(&text[a..b]) == sec.sha256 {
        Ok(())
    } else {
        Err("modified (hash mismatch)".into())
    }
}

fn key_present(k: &OwnedKey) -> Result<bool> {
    let segs: Vec<&str> = k.key.iter().map(String::as_str).collect();
    if k.file.extension().is_some_and(|x| x == "toml") {
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&k.file)?.parse()?;
        let mut cur: &toml_edit::Item = doc.as_item();
        for s in segs {
            cur = match cur.get(s) {
                Some(i) => i,
                None => return Ok(false),
            };
        }
        Ok(true)
    } else {
        let doc = load_json(&k.file)?;
        let mut cur = &doc;
        for s in segs {
            cur = match cur.get(s) {
                Some(v) => v,
                None => return Ok(false),
            };
        }
        Ok(true)
    }
}

// ------------------------------------------------------------------- init ----

/// Scaffold the package layout. Refuses to overwrite an existing plugin.json.
///
/// # Errors
/// Returns an error if `dir` already holds a package or can't be written.
pub fn init(dir: &Path) -> Result<()> {
    if is_package(dir) {
        bail!("{} already has .claude-plugin/plugin.json", dir.display());
    }
    let name = dir
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .filter(|n| valid_segment(n))
        .unwrap_or_else(|| "my-package".into());
    let files: [(&str, String); 7] = [
        (
            ".claude-plugin/plugin.json",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": name, "version": "0.1.0", "description": "What this package gives an agent."
            }))? + "\n",
        ),
        (
            "skills/example/SKILL.md",
            format!("---\nname: example\ndescription: When to reach for this skill.\n---\n\n# {name}: example\n\nSteps the agent follows.\n"),
        ),
        (
            "rules/conventions.md",
            "---\npaths: [\"**/*\"]\n---\n\nRules that apply to matching paths.\n".into(),
        ),
        ("harness/claude-code/hooks/hooks.json", "{\n    \"hooks\": {}\n}\n".into()),
        (
            "harness/codex/config.toml",
            "# Keys merged into ~/.codex/config.toml (th pkg records ownership per key).\n".into(),
        ),
        ("harness/codex/hooks.json", "{\n    \"hooks\": {}\n}\n".into()),
        (
            "harness/opencode/plugin.js",
            "// OpenCode lifecycle plugin — linked to ~/.config/opencode/plugins/<name>.js\nexport const Plugin = async () => ({});\n".into(),
        ),
    ];
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap_or(dir))?;
        std::fs::write(&p, body).with_context(|| format!("write {}", p.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- helpers ----

/// Copy a tree, replacing `dst`. Skips VCS/build noise. `with_overlays`
/// keeps `harness/` (the cache copy) or drops it (a composed native plugin).
fn copy_dir(src: &Path, dst: &Path, with_overlays: bool) -> Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst).with_context(|| format!("clear {}", dst.display()))?;
    }
    overlay_dir(src, dst, with_overlays)
}

/// Copy `src` over `dst` without clearing it (files replace, dirs merge).
fn overlay_dir(src: &Path, dst: &Path, with_overlays: bool) -> Result<()> {
    for entry in walkdir::WalkDir::new(src).min_depth(1).into_iter().filter_entry(|e| {
        let n = e.file_name().to_string_lossy();
        !(n == ".git" || n == "node_modules" || n == "target" || (!with_overlays && e.depth() == 1 && n == "harness"))
    }) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src)?;
        let to = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&to)?;
        } else {
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(entry.path(), &to).with_context(|| format!("copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn sha256_file(p: &Path) -> Result<String> {
    Ok(format!(
        "{:x}",
        sha2::Sha256::digest(std::fs::read(p).with_context(|| format!("read {}", p.display()))?)
    ))
}

fn sha256_str(s: &str) -> String {
    format!("{:x}", sha2::Sha256::digest(s.as_bytes()))
}

fn load_json(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&raw).with_context(|| format!("parse {} as JSON — fix or move it, then re-run", path.display()))
}

fn save_json(path: &Path, doc: &serde_json::Value) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(doc)?)).with_context(|| format!("write {}", path.display()))
}

/// Set a nested key, creating intermediate objects (a non-object in the way
/// is replaced — these are keys we own).
fn json_set(doc: &mut serde_json::Value, keys: &[&str], value: serde_json::Value) {
    let Some((last, parents)) = keys.split_last() else { return };
    let mut cur = doc;
    for k in parents {
        cur = as_object(cur).entry((*k).to_string()).or_insert_with(|| serde_json::json!({}));
    }
    as_object(cur).insert((*last).to_string(), value);
}

fn as_object(v: &mut serde_json::Value) -> &mut serde_json::Map<String, serde_json::Value> {
    if !v.is_object() {
        *v = serde_json::json!({});
    }
    match v {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("just made it an object"),
    }
}

/// Remove a nested key; prunes containers left empty. Returns whether it existed.
fn json_remove(doc: &mut serde_json::Value, keys: &[&str]) -> bool {
    let Some((last, parents)) = keys.split_last() else { return false };
    let mut cur = &mut *doc;
    for k in parents {
        match cur.get_mut(*k) {
            Some(v) => cur = v,
            None => return false,
        }
    }
    let removed = cur.as_object_mut().is_some_and(|m| m.remove(*last).is_some());
    if removed && cur.as_object().is_some_and(serde_json::Map::is_empty) && !parents.is_empty() {
        json_remove(doc, parents);
    }
    removed
}

/// Deep-merge a TOML fragment into `target` (created if missing), returning
/// the leaf key paths now owned by the package. Comments and layout of the
/// target survive (`toml_edit`).
fn toml_merge_fragment(target: &Path, fragment: &str) -> Result<Vec<Vec<String>>> {
    let raw = if target.exists() { std::fs::read_to_string(target)? } else { String::new() };
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .with_context(|| format!("parse {} as TOML — fix or move it, then re-run", target.display()))?;
    let frag: toml_edit::DocumentMut = fragment.parse().context("parse harness/codex/config.toml fragment")?;
    let mut keys = Vec::new();
    merge_item(doc.as_item_mut(), frag.as_item(), &mut Vec::new(), &mut keys);
    if let Some(p) = target.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(target, doc.to_string())?;
    Ok(keys)
}

fn merge_item(into: &mut toml_edit::Item, from: &toml_edit::Item, path: &mut Vec<String>, keys: &mut Vec<Vec<String>>) {
    if let Some(ft) = from.as_table_like() {
        if !into.is_table_like() {
            *into = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let Some(it) = into.as_table_like_mut() else { return };
        for (k, v) in ft.iter() {
            path.push(k.to_string());
            if v.is_table_like() {
                if it.get(k).is_none() {
                    let mut t = toml_edit::Table::new();
                    t.set_implicit(true);
                    it.insert(k, toml_edit::Item::Table(t));
                }
                if let Some(child) = it.get_mut(k) {
                    merge_item(child, v, path, keys);
                }
            } else {
                it.insert(k, v.clone());
                keys.push(path.clone());
            }
            path.pop();
        }
    }
}

/// Remove one key path from a TOML file; prunes tables left empty.
fn toml_remove_key(target: &Path, keys: &[&str]) -> Result<()> {
    let raw = std::fs::read_to_string(target)?;
    let mut doc: toml_edit::DocumentMut = raw.parse().with_context(|| format!("parse {}", target.display()))?;
    if toml_remove_in(doc.as_item_mut(), keys) {
        std::fs::write(target, doc.to_string())?;
    }
    Ok(())
}

fn toml_remove_in(item: &mut toml_edit::Item, keys: &[&str]) -> bool {
    let Some((first, rest)) = keys.split_first() else { return false };
    let Some(t) = item.as_table_like_mut() else { return false };
    if rest.is_empty() {
        return t.remove(first).is_some();
    }
    let Some(child) = t.get_mut(first) else { return false };
    let removed = toml_remove_in(child, rest);
    if removed && child.as_table_like().is_some_and(toml_edit::TableLike::is_empty) {
        t.remove(first);
    }
    removed
}

// ---------------------------------------------------------------- hooks ----

/// Key-merge a Claude-style `hooks.json` document (`{"hooks": {Event:
/// [{matcher?, hooks: [{type, command, …}]}]}}`) into `base`: events union,
/// matcher groups matched by (normalised) matcher, hooks appended unless an
/// identical command is already there. Returns `(event, matcher, command)`
/// for every hook actually added — the ones the caller now owns.
fn merge_hooks(base: &mut serde_json::Value, add: &serde_json::Value) -> Vec<(String, String, String)> {
    let mut added = Vec::new();
    let Some(events) = add.get("hooks").and_then(serde_json::Value::as_object) else {
        return added;
    };
    let base_events = as_object(as_object(base).entry("hooks").or_insert_with(|| serde_json::json!({})));
    for (event, groups) in events {
        let Some(groups) = groups.as_array() else { continue };
        let target = base_events.entry(event.clone()).or_insert_with(|| serde_json::json!([]));
        if !target.is_array() {
            *target = serde_json::json!([]);
        }
        let Some(target) = target.as_array_mut() else { continue };
        for group in groups {
            let matcher = hook_matcher(group);
            let Some(hooks) = group.get("hooks").and_then(serde_json::Value::as_array) else {
                continue;
            };
            let slot = match target.iter().position(|g| hook_matcher(g) == matcher) {
                Some(i) => i,
                None => {
                    let mut g = group.clone();
                    g["hooks"] = serde_json::json!([]);
                    target.push(g);
                    target.len() - 1
                }
            };
            let dst = as_object(&mut target[slot]).entry("hooks".to_string()).or_insert_with(|| serde_json::json!([]));
            if !dst.is_array() {
                *dst = serde_json::json!([]);
            }
            let Some(dst) = dst.as_array_mut() else { continue };
            for h in hooks {
                let id = hook_id(h);
                if dst.iter().any(|e| hook_id(e) == id) {
                    continue;
                }
                dst.push(h.clone());
                added.push((event.clone(), matcher.clone(), id));
            }
        }
    }
    added
}

/// A group's matcher; absent and `""` are the same thing to Claude/Codex.
fn hook_matcher(group: &serde_json::Value) -> String {
    group.get("matcher").and_then(serde_json::Value::as_str).unwrap_or("").to_string()
}

/// What makes two hooks "the same": the command for command hooks, the whole
/// value otherwise.
fn hook_id(hook: &serde_json::Value) -> String {
    hook.get("command")
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| hook.to_string(), str::to_string)
}

/// Remove one hook (by command) from `hooks.<event>` groups with `matcher`;
/// prunes groups and events left empty but keeps `"hooks": {}` so the file
/// stays a valid hooks document. Returns whether anything was removed.
fn remove_hook(doc: &mut serde_json::Value, event: &str, matcher: &str, command: &str) -> bool {
    let Some(events) = doc.get_mut("hooks").and_then(serde_json::Value::as_object_mut) else {
        return false;
    };
    let Some(groups) = events.get_mut(event).and_then(serde_json::Value::as_array_mut) else {
        return false;
    };
    let mut removed = false;
    for g in groups.iter_mut() {
        if hook_matcher(g) != matcher {
            continue;
        }
        if let Some(hs) = g.get_mut("hooks").and_then(serde_json::Value::as_array_mut) {
            let before = hs.len();
            hs.retain(|h| hook_id(h) != command);
            removed |= hs.len() != before;
        }
    }
    if removed {
        groups.retain(|g| g.get("hooks").and_then(serde_json::Value::as_array).is_some_and(|h| !h.is_empty()));
        if groups.is_empty() {
            events.remove(event);
        }
    }
    removed
}

/// Replace `${CLAUDE_PLUGIN_ROOT}` in every string of a document with the
/// cached package root (Claude substitutes it itself; nobody else does).
fn subst_plugin_root(v: &mut serde_json::Value, root: &Path) {
    match v {
        serde_json::Value::String(s) if s.contains("${CLAUDE_PLUGIN_ROOT}") => {
            *s = s.replace("${CLAUDE_PLUGIN_ROOT}", &root.display().to_string());
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| subst_plugin_root(x, root)),
        serde_json::Value::Object(m) => m.values_mut().for_each(|x| subst_plugin_root(x, root)),
        _ => {}
    }
}

// ---------------------------------------------------------------- rules ----

/// One `rules/<stem>.md`: optional `paths:` / `description:` frontmatter + body.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    stem: String,
    description: Option<String>,
    paths: Vec<String>,
    body: String,
}

impl Rule {
    fn parse(stem: &str, text: &str) -> Self {
        let (fm, body) = split_frontmatter(text);
        Self {
            stem: stem.to_string(),
            description: fm.and_then(|f| fm_scalar(f, "description")),
            paths: fm.map(|f| fm_list(f, "paths")).unwrap_or_default(),
            body: body.trim_matches('\n').to_string(),
        }
    }

    fn title(&self) -> &str {
        self.description.as_deref().unwrap_or(&self.stem)
    }

    /// Cursor `.mdc`: `description`, `globs` (comma-joined), `alwaysApply`
    /// when the rule has no paths.
    fn to_mdc(&self, pkg: &str) -> String {
        let mut out = String::from("---\n");
        out += &format!("description: {}\n", self.description.clone().unwrap_or_else(|| format!("{pkg}: {}", self.stem)));
        if !self.paths.is_empty() {
            out += &format!("globs: {}\n", self.paths.join(","));
        }
        out += &format!("alwaysApply: {}\n---\n\n{}\n", self.paths.is_empty(), self.body);
        out
    }
}

/// `rules/*.md` of a package root, sorted by stem.
fn load_rules(root: &Path) -> Result<Vec<Rule>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("rules")) else {
        return Ok(out);
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() || p.extension().is_none_or(|x| x != "md") {
            continue;
        }
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        out.push(Rule::parse(
            &stem,
            &std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?,
        ));
    }
    out.sort_by(|a, b| a.stem.cmp(&b.stem));
    Ok(out)
}

/// `(frontmatter, body)` for a `---`-fenced document; `(None, text)` otherwise.
fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return (None, text);
    };
    for end in ["\n---\n", "\n---\r\n"] {
        if let Some(i) = rest.find(end) {
            return (Some(&rest[..i]), &rest[i + end.len()..]);
        }
    }
    if let Some(fm) = rest.strip_suffix("\n---").or_else(|| rest.strip_suffix("\n---\n")) {
        return (Some(fm), "");
    }
    (None, text)
}

/// A scalar `key: value` line of the (tiny, YAML-ish) frontmatter.
fn fm_scalar(fm: &str, key: &str) -> Option<String> {
    fm.lines().find_map(|l| {
        let v = l.strip_prefix(key)?.trim_start().strip_prefix(':')?.trim();
        (!v.is_empty() && !v.starts_with('[')).then(|| unquote(v).to_string())
    })
}

/// A list value: inline `key: [a, "b"]` or a block of `- item` lines.
fn fm_list(fm: &str, key: &str) -> Vec<String> {
    let mut lines = fm.lines().peekable();
    while let Some(l) = lines.next() {
        let Some(v) = l.strip_prefix(key).and_then(|r| r.trim_start().strip_prefix(':')) else {
            continue;
        };
        let v = v.trim();
        if let Some(inner) = v.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            return inner.split(',').map(|s| unquote(s.trim()).to_string()).filter(|s| !s.is_empty()).collect();
        }
        if v.is_empty() {
            let mut items = Vec::new();
            while let Some(item) = lines.peek().and_then(|n| n.trim_start().strip_prefix("- ")) {
                items.push(unquote(item.trim()).to_string());
                lines.next();
            }
            return items;
        }
        return vec![unquote(v).to_string()];
    }
    Vec::new()
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
        .unwrap_or(s)
}

// ------------------------------------------------------- managed section ----

fn section_markers(name: &str) -> (String, String) {
    (format!("<!-- th-pkg:{name} -->"), format!("<!-- /th-pkg:{name} -->"))
}

/// The block `th pkg` owns in an `AGENTS.md`: markers, a one-line notice, one
/// `###` per rule with its path scope.
fn render_section(name: &str, rules: &[Rule]) -> String {
    let (open, close) = section_markers(name);
    let mut s = format!(
        "{open}\n<!-- managed by `th pkg install {name}` — edits inside these markers are overwritten on reinstall; `th pkg rm {name}` removes the block -->\n"
    );
    for r in rules {
        s += &format!("\n### {}\n", r.title());
        if !r.paths.is_empty() {
            s += &format!("\n_Applies to: {}_\n", r.paths.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", "));
        }
        s += &format!("\n{}\n", r.body);
    }
    s += &format!("\n{close}\n");
    s
}

/// Byte range of the managed block (through its trailing newline), if present.
fn find_section(text: &str, name: &str) -> Option<(usize, usize)> {
    let (open, close) = section_markers(name);
    let a = text.match_indices(&open).map(|(i, _)| i).find(|&i| i == 0 || text[..i].ends_with('\n'))?;
    let c = text[a..].find(&close)? + a;
    let mut b = c + close.len();
    if text[b..].starts_with('\n') {
        b += 1;
    }
    Some((a, b))
}

/// Write the block into `file`, replacing an existing one in place or
/// appending after a blank line. Returns whether the file was created.
fn upsert_section(file: &Path, name: &str, block: &str) -> Result<bool> {
    let existing = file.is_file().then(|| std::fs::read_to_string(file)).transpose()?;
    let out = match &existing {
        Some(text) => match find_section(text, name) {
            Some((a, b)) => format!("{}{block}{}", &text[..a], &text[b..]),
            None => {
                let mut t = text.clone();
                if !t.is_empty() && !t.ends_with('\n') {
                    t.push('\n');
                }
                if !t.is_empty() {
                    t.push('\n');
                }
                t + block
            }
        },
        None => block.to_string(),
    };
    if let Some(p) = file.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(file, out).with_context(|| format!("write {}", file.display()))?;
    Ok(existing.is_none())
}

/// Take the block out again. A file we created that is left empty is deleted.
fn remove_section(file: &Path, name: &str, created: bool) -> Result<bool> {
    let text = std::fs::read_to_string(file)?;
    let Some((a, b)) = find_section(&text, name) else {
        return Ok(false);
    };
    let (before, mut after) = (&text[..a], &text[b..]);
    // The blank line that separated the block from what came before it goes too.
    if before.ends_with("\n\n") && after.starts_with('\n') {
        after = &after[1..];
    }
    let mut out = format!("{before}{after}");
    while out.ends_with("\n\n") {
        out.pop();
    }
    if created && out.trim().is_empty() {
        std::fs::remove_file(file)?;
    } else {
        std::fs::write(file, out)?;
    }
    Ok(true)
}

/// Write `content` at `dst` and record it as ours.
fn write_owned(dst: &Path, content: &str, harness: &str) -> Result<OwnedFile> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dst, content).with_context(|| format!("write {}", dst.display()))?;
    Ok(OwnedFile {
        harness: harness.to_string(),
        path: dst.to_path_buf(),
        kind: "file".into(),
        sha256: sha256_str(content),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A fixture package with core + every overlay.
    fn fixture(dir: &Path) {
        let w = |rel: &str, body: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            ".claude-plugin/plugin.json",
            r#"{"name":"fix","version":"1.2.3","description":"fixture","mcpServers":{"fixmcp":{"command":"th","args":["mcp","serve"]}}}"#,
        );
        w(
            ".mcp.json",
            r#"{"mcpServers":{"other":{"command":"${CLAUDE_PLUGIN_ROOT}/bin/x","args":["--go"],"env":{"K":"v"}}}}"#,
        );
        w("skills/alpha/SKILL.md", "---\nname: alpha\ndescription: a\n---\n");
        w("skills/beta/SKILL.md", "---\nname: beta\ndescription: b\n---\n");
        w("commands/hello.md", "hi");
        w("rules/style.md", "---\npaths: [\"**/*.rs\"]\n---\nbe terse\n");
        w("rules/always.md", "---\ndescription: Always on\n---\n\nsay hi\n");
        w(
            "hooks/hooks.json",
            r#"{"hooks":{"SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/hooks/a.sh"}]}]}}"#,
        );
        w(
            "harness/claude-code/hooks/hooks.json",
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/hooks/b.sh"}]}],"SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/hooks/a.sh"},{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/hooks/c.sh"}]}]}}"#,
        );
        w("harness/codex/config.toml", "[features]\nfix = true\n[plugins.\"fix@th\"]\nenabled = true\n");
        w(
            "harness/codex/hooks.json",
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/hooks/flow.sh SessionStart codex"},{"type":"command","command":"th prime"}]}],"PreToolUse":[{"hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/hooks/flow.sh PreToolUse codex"}]}]}}"#,
        );
        w("harness/opencode/plugin.js", "export const Fix = 1;");
        w(
            "harness/cursor/rules/always.mdc",
            "---\ndescription: overridden\nalwaysApply: true\n---\n\nhi from the overlay\n",
        );
        w(
            "harness/cursor/rules/extra.mdc",
            "---\ndescription: extra\nalwaysApply: false\n---\n\nonly cursor gets this\n",
        );
    }

    fn home() -> (TempDir, Paths) {
        let tmp = TempDir::new().unwrap();
        for h in Harness::ALL {
            std::fs::create_dir_all(h.marker_dir(tmp.path())).unwrap();
        }
        let paths = Paths::new(tmp.path().to_path_buf());
        (tmp, paths)
    }

    fn pkg_dir(tmp: &TempDir) -> PathBuf {
        let d = tmp.path().join("src").join("fix");
        fixture(&d);
        d
    }

    #[test]
    fn source_parsing_covers_paths_github_and_marketplaces() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(Source::parse(tmp.path().to_str().unwrap()).unwrap(), Source::Path(tmp.path().to_path_buf()));
        assert_eq!(Source::parse("./x").unwrap(), Source::Path(PathBuf::from("./x")));
        assert_eq!(
            Source::parse("SmooAI/smooth/claude-plugins/smooth-agent#v1.2").unwrap(),
            Source::GitHub {
                owner: "SmooAI".into(),
                repo: "smooth".into(),
                subdir: Some("claude-plugins/smooth-agent".into()),
                git_ref: Some("v1.2".into())
            }
        );
        assert_eq!(
            Source::parse("https://github.com/o/r.git").unwrap(),
            Source::GitHub {
                owner: "o".into(),
                repo: "r".into(),
                subdir: None,
                git_ref: None
            }
        );
        assert_eq!(Source::parse("owner/repo#").unwrap().to_string(), "github:owner/repo");
        assert_eq!(
            Source::parse("https://x.dev/m/marketplace.json").unwrap(),
            Source::MarketplaceUrl("https://x.dev/m/marketplace.json".into())
        );
        assert!(Source::parse("justone").is_err());
        assert!(Source::parse("a/b/../c").is_err());
        assert!(Source::parse("https://gitlab.com/o/r").is_err());
        assert!(Source::parse("").is_err());
    }

    #[test]
    fn manifest_merges_plugin_json_and_mcp_json_servers() {
        let tmp = TempDir::new().unwrap();
        let d = pkg_dir(&tmp);
        let m = load_manifest(&d).unwrap();
        assert_eq!((m.name.as_str(), m.version.as_deref()), ("fix", Some("1.2.3")));
        assert_eq!(m.mcp_servers.len(), 2);
        let other = m.mcp_servers.iter().find(|s| s.name == "other").unwrap();
        assert_eq!(other.command, format!("{}/bin/x", d.display()));
        assert_eq!(other.env, vec![("K".to_string(), "v".to_string())]);

        std::fs::write(d.join(".claude-plugin/plugin.json"), r#"{"name":"Bad Name"}"#).unwrap();
        assert!(load_manifest(&d).is_err());
        assert!(load_manifest(tmp.path()).is_err(), "no plugin.json");
    }

    #[test]
    fn marketplace_parsing_resolves_relative_github_and_directory_sources() {
        let doc = serde_json::json!({"name":"m","plugins":[
            {"name":"a","source":"./plugins/a"},
            {"name":"b","source":{"source":"github","repo":"o/r","ref":"v2"}},
            {"name":"c","source":"o/r2/sub"},
            {"name":"d","source":{"source":"directory","path":"/abs/d"}},
        ]});
        let got = marketplace_sources(&doc, Some(Path::new("/mk"))).unwrap();
        assert_eq!(got[0].1, Source::Path(PathBuf::from("/mk/./plugins/a")));
        assert_eq!(got[1].1.to_string(), "github:o/r#v2");
        assert_eq!(got[2].1.to_string(), "github:o/r2/sub");
        assert_eq!(got[3].1, Source::Path(PathBuf::from("/abs/d")));
        // Relative sources need a base (a URL-fetched marketplace has none).
        assert!(marketplace_sources(&doc, None).is_err());
        assert!(marketplace_sources(&serde_json::json!({"plugins":[{"name":"x","source":{"source":"npm"}}]}), None).is_err());
    }

    #[test]
    fn install_marketplace_directory_installs_every_listed_plugin() {
        let (tmp, paths) = home();
        let mk = tmp.path().join("mk");
        fixture(&mk.join("plugins").join("fix"));
        std::fs::create_dir_all(mk.join(".claude-plugin")).unwrap();
        std::fs::write(
            mk.join(".claude-plugin/marketplace.json"),
            r#"{"name":"mk","plugins":[{"name":"fix","source":"./plugins/fix"}]}"#,
        )
        .unwrap();
        let names = install(&paths, &Source::Path(mk), &[Harness::Codex]).unwrap();
        assert_eq!(names, vec!["fix".to_string()]);
        assert!(Index::load(&paths).unwrap().packages.contains_key("fix"));
    }

    #[test]
    #[cfg(unix)]
    fn install_renders_every_harness_and_rm_removes_exactly_that() {
        let (tmp, paths) = home();
        let src = pkg_dir(&tmp);
        // User-owned things that must survive.
        std::fs::write(paths.claude_settings(), r#"{"enabledPlugins":{"mine@x":true},"model":"opus"}"#).unwrap();
        std::fs::write(Harness::Codex.config_path(tmp.path()), "# keep me\nmodel = \"gpt-5.5\"\n[features]\nmine = 1\n").unwrap();
        let user_skill = paths.codex_skills().join("alpha");
        std::fs::create_dir_all(&user_skill).unwrap();
        std::fs::write(user_skill.join("SKILL.md"), "mine").unwrap();

        install(&paths, &Source::Path(src.clone()), &Harness::ALL).unwrap();
        let index = Index::load(&paths).unwrap();
        let rec = &index.packages["fix"];
        assert_eq!(rec.version.as_deref(), Some("1.2.3"));
        assert!(rec.root.starts_with(paths.cache()), "copied into the cache");

        // smooth + opencode + codex skills (codex alpha is user-owned → skipped).
        for d in [paths.smooth_skills(), paths.opencode_skills()] {
            for s in ["alpha", "beta"] {
                assert!(std::fs::read_link(d.join(s)).unwrap().starts_with(&rec.root), "{s} linked in {}", d.display());
            }
        }
        assert!(!std::fs::symlink_metadata(&user_skill).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(user_skill.join("SKILL.md")).unwrap(), "mine");
        assert!(std::fs::read_link(paths.codex_skills().join("beta")).is_ok());
        assert!(rec.notes.iter().any(|n| n.contains("not ours")));

        // MCP servers in codex + opencode, with env, plus the codex fragment.
        let codex = std::fs::read_to_string(Harness::Codex.config_path(tmp.path())).unwrap();
        assert!(
            codex.contains("# keep me") && codex.contains("[mcp_servers.fixmcp]") && codex.contains("[mcp_servers.other]"),
            "{codex}"
        );
        assert!(codex.contains("K = \"v\""), "{codex}");
        assert!(codex.contains("fix = true") && codex.contains("mine = 1"), "{codex}");
        let oc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(Harness::OpenCode.config_path(tmp.path())).unwrap()).unwrap();
        assert_eq!(oc["mcp"]["other"]["command"][1], "--go");
        assert_eq!(oc["mcp"]["other"]["environment"]["K"], "v");

        // OpenCode plugin link, Claude rules copy, Claude plugin handoff.
        assert!(std::fs::read_link(paths.opencode_plugins().join("fix.js"))
            .unwrap()
            .ends_with("harness/opencode/plugin.js"));
        assert_eq!(
            std::fs::read_to_string(paths.claude_rules().join("fix/style.md")).unwrap().trim_end(),
            "---\npaths: [\"**/*.rs\"]\n---\nbe terse"
        );
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.claude_settings()).unwrap()).unwrap();
        assert_eq!(settings["enabledPlugins"]["fix@th-pkg"], true);
        assert_eq!(settings["enabledPlugins"]["mine@x"], true);
        assert_eq!(settings["model"], "opus");
        assert_eq!(settings["extraKnownMarketplaces"]["th-pkg"]["source"]["source"], "directory");
        let composed = paths.claude_marketplace().join("plugins/fix");
        assert!(composed.join("skills/alpha/SKILL.md").is_file());
        assert!(composed.join("commands/hello.md").is_file());
        assert!(!composed.join("harness").exists(), "overlays never ship inside the composed plugin");
        // M1: the claude-code overlay is KEY-MERGED into core hooks.json.
        let hooks: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(composed.join("hooks/hooks.json")).unwrap()).unwrap();
        assert_eq!(hooks["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        let ss = hooks["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(ss.len(), 1, "same matcher → one group: {ss:?}");
        let cmds: Vec<&str> = ss[0]["hooks"].as_array().unwrap().iter().map(|h| h["command"].as_str().unwrap()).collect();
        assert_eq!(
            cmds,
            ["${CLAUDE_PLUGIN_ROOT}/hooks/a.sh", "${CLAUDE_PLUGIN_ROOT}/hooks/c.sh"],
            "a.sh deduped, c.sh appended"
        );

        // Codex hooks overlay → ~/.codex/hooks.json with the cache root substituted; cursor rules; AGENTS.md sections.
        let ch: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.codex_hooks()).unwrap()).unwrap();
        assert_eq!(
            ch["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            format!("{}/hooks/flow.sh PreToolUse codex", rec.root.display())
        );
        assert_eq!(rec.hooks.len(), 3, "{:?}", rec.hooks);
        let mdc = std::fs::read_to_string(paths.cursor_rules().join("fix/style.mdc")).unwrap();
        assert!(
            mdc.contains("globs: **/*.rs") && mdc.contains("alwaysApply: false") && mdc.contains("be terse"),
            "{mdc}"
        );
        assert!(std::fs::read_to_string(paths.cursor_rules().join("fix/always.mdc"))
            .unwrap()
            .contains("hi from the overlay"));
        assert!(paths.cursor_rules().join("fix/extra.mdc").is_file());
        for f in [paths.codex_agents_md(), paths.opencode_agents_md()] {
            let text = std::fs::read_to_string(&f).unwrap();
            assert!(
                text.starts_with("<!-- th-pkg:fix -->") && text.contains("### Always on") && text.contains("`**/*.rs`"),
                "{text}"
            );
        }
        assert_eq!(rec.sections.len(), 2);
        assert!(rec.sections.iter().all(|s| s.created));
        let mk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(paths.claude_marketplace().join(".claude-plugin/marketplace.json")).unwrap()).unwrap();
        assert_eq!(mk["plugins"][0]["source"], "./plugins/fix");
        assert_eq!(rec.claude_plugin.as_deref(), Some("fix@th-pkg"));

        // Status: all green.
        let lines = status_lines(rec);
        assert!(lines.iter().any(|l| l.contains("0 drifted")), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("opencode/plugin.js=present") && l.contains("cursor/rules=present") && l.contains("codex/hooks.json=present")),
            "{lines:?}"
        );

        // Idempotent reinstall: same artifact count, no duplicates.
        install(&paths, &Source::Path(src.clone()), &Harness::ALL).unwrap();
        let again = Index::load(&paths).unwrap();
        assert_eq!(again.packages["fix"].files.len(), rec.files.len());
        assert_eq!(again.packages["fix"].keys.len(), rec.keys.len());
        assert_eq!(again.packages["fix"].hooks.len(), rec.hooks.len());
        assert_eq!(again.packages["fix"].sections.len(), rec.sections.len());
        let ch: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.codex_hooks()).unwrap()).unwrap();
        assert_eq!(
            ch["hooks"]["SessionStart"][0]["hooks"].as_array().unwrap().len(),
            2,
            "no duplicate hooks on reinstall"
        );
        assert_eq!(
            std::fs::read_to_string(paths.codex_agents_md()).unwrap().matches("<!-- th-pkg:fix -->").count(),
            1
        );
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.claude_settings()).unwrap()).unwrap();
        assert_eq!(settings["enabledPlugins"].as_object().unwrap().len(), 2);

        // rm: everything we wrote goes, everything the user owns stays.
        let warnings = rm(&paths, "fix").unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(Index::load(&paths).unwrap().packages.is_empty());
        assert!(std::fs::symlink_metadata(paths.smooth_skills().join("alpha")).is_err());
        assert!(std::fs::symlink_metadata(paths.opencode_plugins().join("fix.js")).is_err());
        assert!(!paths.claude_rules().join("fix").exists());
        assert!(user_skill.join("SKILL.md").is_file());
        let codex = std::fs::read_to_string(Harness::Codex.config_path(tmp.path())).unwrap();
        assert!(codex.contains("# keep me") && codex.contains("mine = 1"), "{codex}");
        assert!(
            !codex.contains("fixmcp") && !codex.contains("fix = true") && !codex.contains("fix@th"),
            "{codex}"
        );
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.claude_settings()).unwrap()).unwrap();
        assert_eq!(settings["enabledPlugins"]["mine@x"], true);
        assert!(settings["enabledPlugins"].get("fix@th-pkg").is_none());
        assert!(settings.get("extraKnownMarketplaces").is_none(), "empty marketplace registration pruned");
        assert!(!paths.claude_marketplace().exists());
        assert!(!paths.cache().join("fix@local").exists());
        assert!(!paths.cursor_rules().join("fix").exists());
        assert!(
            !paths.codex_agents_md().exists() && !paths.opencode_agents_md().exists(),
            "files we created go away"
        );
        let ch: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.codex_hooks()).unwrap()).unwrap();
        assert_eq!(ch, serde_json::json!({"hooks": {}}), "{ch}");
        assert!(rm(&paths, "fix").is_err(), "second rm reports unknown package");
    }

    #[test]
    fn merge_hooks_covers_every_case_and_reports_only_what_it_added() {
        let cmd = |c: &str| serde_json::json!({"type":"command","command":c});
        let mut base = serde_json::json!({"hooks":{
            "SessionStart":[{"matcher":"","hooks":[cmd("th prime")]}],
            "PreToolUse":[{"matcher":"Bash","hooks":[cmd("rtk hook claude")]}],
            "Stop":"not an array"
        }});
        let add = serde_json::json!({"hooks":{
            // absent matcher == "" → same group; th prime is deduped, flow appended
            "SessionStart":[{"hooks":[cmd("th prime"), cmd("flow SessionStart")]}],
            // existing event, new matcher group → appended as a group
            "PreToolUse":[{"matcher":"Edit","hooks":[cmd("guard")]}, {"matcher":"Bash","hooks":[cmd("rtk hook claude"), cmd("flow PreToolUse")]}],
            // new event
            "PostToolUse":[{"hooks":[cmd("flow PostToolUse")]}],
            // a non-array event in base is replaced (it was never a valid group list)
            "Stop":[{"hooks":[cmd("flow Stop")]}],
            // non-command hooks compare by value
            "Notification":[{"hooks":[{"type":"prompt","prompt":"p"}]}],
            // garbage groups are skipped
            "SessionEnd":[{"matcher":"x"}, "junk"]
        }});
        let added = merge_hooks(&mut base, &add);
        let mut got: Vec<String> = added.iter().map(|(e, m, c)| format!("{e}|{m}|{c}")).collect();
        got.sort();
        assert_eq!(
            got,
            [
                "Notification||{\"type\":\"prompt\",\"prompt\":\"p\"}",
                "PostToolUse||flow PostToolUse",
                "PreToolUse|Bash|flow PreToolUse",
                "PreToolUse|Edit|guard",
                "SessionStart||flow SessionStart",
                "Stop||flow Stop",
            ]
        );
        let ss = base["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss[0]["hooks"].as_array().unwrap().len(), 2);
        assert_eq!(base["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(
            base["hooks"]["PreToolUse"][0]["hooks"].as_array().unwrap().len(),
            2,
            "appended to the Bash group"
        );
        assert_eq!(base["hooks"]["PreToolUse"][1]["matcher"], "Edit");
        assert!(base["hooks"].get("SessionEnd").is_some_and(|v| v.as_array().unwrap().is_empty()));
        // Merging the same overlay again adds nothing.
        assert!(merge_hooks(&mut base, &add).is_empty());
        // A base without `hooks` at all, and an add without `hooks`.
        let mut empty = serde_json::json!({});
        assert_eq!(merge_hooks(&mut empty, &add).len(), 8, "everything, th prime and rtk included");
        assert!(merge_hooks(&mut empty, &serde_json::json!({"nope":1})).is_empty());

        // remove_hook: prunes the group and the event, keeps `hooks: {}`, never touches neighbours.
        assert!(remove_hook(&mut base, "SessionStart", "", "flow SessionStart"));
        assert_eq!(base["hooks"]["SessionStart"][0]["hooks"][0]["command"], "th prime");
        assert!(remove_hook(&mut base, "PostToolUse", "", "flow PostToolUse"));
        assert!(base["hooks"].get("PostToolUse").is_none(), "{base}");
        assert!(!remove_hook(&mut base, "PostToolUse", "", "flow PostToolUse"));
        assert!(!remove_hook(&mut base, "SessionStart", "Bash", "th prime"), "wrong matcher removes nothing");
        assert!(remove_hook(&mut base, "PreToolUse", "Edit", "guard"));
        assert_eq!(base["hooks"]["PreToolUse"].as_array().unwrap().len(), 1, "empty Edit group pruned");
        let mut only = serde_json::json!({"hooks":{"Stop":[{"hooks":[cmd("x")]}]}});
        assert!(remove_hook(&mut only, "Stop", "", "x"));
        assert_eq!(only, serde_json::json!({"hooks":{}}));
        assert!(!remove_hook(&mut serde_json::json!({}), "Stop", "", "x"));
    }

    #[test]
    fn codex_hooks_overlay_keeps_the_users_entries_and_rm_takes_back_only_ours() {
        let (tmp, paths) = home();
        let src = pkg_dir(&tmp);
        // Brent's real file: th prime on SessionStart + PreCompact.
        let user = serde_json::json!({"hooks":{
            "PreCompact":[{"matcher":"","hooks":[{"type":"command","command":"th prime"}]}],
            "SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"th prime"}]}]
        }});
        save_json(&paths.codex_hooks(), &user).unwrap();
        install(&paths, &Source::Path(src.clone()), &[Harness::Codex]).unwrap();
        let rec = Index::load(&paths).unwrap().packages["fix"].clone();
        let doc = load_json(&paths.codex_hooks()).unwrap();
        let ss: Vec<String> = doc["hooks"]["SessionStart"][0]["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["command"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ss[0], "th prime");
        assert!(
            ss[1].ends_with("/hooks/flow.sh SessionStart codex") && ss[1].starts_with(rec.root.to_str().unwrap()),
            "{ss:?}"
        );
        assert_eq!(ss.len(), 2, "the overlay's `th prime` is deduped against the user's");
        assert_eq!(doc["hooks"]["PreCompact"][0]["hooks"][0]["command"], "th prime");
        // Ownership: the two flow hooks, never the user's th prime.
        assert_eq!(rec.hooks.len(), 2, "{:?}", rec.hooks);
        assert!(rec
            .hooks
            .iter()
            .all(|h| h.command.contains("flow.sh") && h.matcher.is_empty() && h.harness == "codex"));

        // Drift: the user deletes one of ours.
        let mut edited = doc.clone();
        remove_hook(
            &mut edited,
            "PreToolUse",
            "",
            &rec.hooks.iter().find(|h| h.event == "PreToolUse").unwrap().command,
        );
        save_json(&paths.codex_hooks(), &edited).unwrap();
        let lines = status_lines(&rec).join("\n");
        assert!(lines.contains("PreToolUse[] — hook") && lines.contains("1 drifted"), "{lines}");

        rm(&paths, "fix").unwrap();
        assert_eq!(load_json(&paths.codex_hooks()).unwrap(), user, "back to exactly the user's file");
    }

    #[test]
    fn agents_md_section_is_idempotent_and_never_touches_text_outside_the_markers() {
        let (tmp, paths) = home();
        let src = pkg_dir(&tmp);
        let before = "# My global rules\n\nBe kind.\n";
        std::fs::write(paths.codex_agents_md(), before).unwrap();
        install(&paths, &Source::Path(src.clone()), &[Harness::Codex]).unwrap();
        let text = std::fs::read_to_string(paths.codex_agents_md()).unwrap();
        assert!(text.starts_with(before), "{text}");
        assert!(
            text[before.len()..].starts_with("\n<!-- th-pkg:fix -->\n"),
            "one blank line then the block: {text}"
        );
        assert!(text.trim_end().ends_with("<!-- /th-pkg:fix -->"));

        // The user writes below the block; the package changes a rule; reinstall.
        std::fs::write(paths.codex_agents_md(), format!("{text}\nUser text after.\n")).unwrap();
        std::fs::write(src.join("rules/style.md"), "---\npaths: [\"**/*.rs\", \"**/*.toml\"]\n---\nbe VERY terse\n").unwrap();
        install(&paths, &Source::Path(src.clone()), &[Harness::Codex]).unwrap();
        let text = std::fs::read_to_string(paths.codex_agents_md()).unwrap();
        assert!(text.starts_with(before));
        assert!(text.ends_with("<!-- /th-pkg:fix -->\n\nUser text after.\n"), "{text}");
        assert_eq!(text.matches("<!-- th-pkg:fix -->").count(), 1);
        assert!(
            text.contains("be VERY terse") && text.contains("`**/*.toml`") && !text.contains("be terse\n"),
            "{text}"
        );
        let rec = Index::load(&paths).unwrap().packages["fix"].clone();
        assert!(!rec.sections[0].created);
        assert!(status_lines(&rec).iter().any(|l| l.contains("0 drifted")));

        // A second package's block sits alongside untouched.
        let other = tmp.path().join("src/other");
        fixture(&other);
        std::fs::write(other.join(".claude-plugin/plugin.json"), r#"{"name":"other"}"#).unwrap();
        install(&paths, &Source::Path(other), &[Harness::Codex]).unwrap();
        let text = std::fs::read_to_string(paths.codex_agents_md()).unwrap();
        assert!(text.contains("<!-- th-pkg:other -->") && text.contains("<!-- th-pkg:fix -->"));
        rm(&paths, "other").unwrap();
        let text = std::fs::read_to_string(paths.codex_agents_md()).unwrap();
        assert!(!text.contains("th-pkg:other") && text.contains("<!-- th-pkg:fix -->"), "{text}");
        assert!(text.ends_with("User text after.\n"), "{text}");

        // A block the user edited is reported and left alone by rm.
        std::fs::write(paths.codex_agents_md(), text.replace("be VERY terse", "my own words")).unwrap();
        let lines = status_lines(&rec).join("\n");
        assert!(lines.contains("managed section fix modified"), "{lines}");
        let warnings = rm(&paths, "fix").unwrap();
        assert!(warnings.iter().any(|w| w.contains("managed section")), "{warnings:?}");
        let text = std::fs::read_to_string(paths.codex_agents_md()).unwrap();
        assert!(text.contains("my own words") && text.starts_with(before));

        // Marker inside a line is not a block start.
        assert!(find_section("text <!-- th-pkg:x --> more\n<!-- /th-pkg:x -->\n", "x").is_none());
        let t = "a\n<!-- th-pkg:x -->\nbody\n<!-- /th-pkg:x -->\nb\n";
        let (a, b) = find_section(t, "x").unwrap();
        assert_eq!(&t[a..b], "<!-- th-pkg:x -->\nbody\n<!-- /th-pkg:x -->\n");
    }

    #[test]
    fn rules_frontmatter_parses_inline_and_block_lists_and_renders_mdc() {
        let r = Rule::parse(
            "style",
            "---\ndescription: \"Rust style\"\npaths:\n  - \"**/*.rs\"\n  - src/**\n---\n\nbody\n\n",
        );
        assert_eq!(r.description.as_deref(), Some("Rust style"));
        assert_eq!(r.paths, ["**/*.rs", "src/**"]);
        assert_eq!(r.body, "body");
        assert_eq!(
            r.to_mdc("p"),
            "---\ndescription: Rust style\nglobs: **/*.rs,src/**\nalwaysApply: false\n---\n\nbody\n"
        );
        let r = Rule::parse("plain", "no frontmatter\n");
        assert_eq!((r.description.as_deref(), r.paths.len(), r.body.as_str()), (None, 0, "no frontmatter"));
        assert!(r.to_mdc("p").contains("description: p: plain\nalwaysApply: true\n"));
        let r = Rule::parse("one", "---\npaths: 'a/**'\n---\nx");
        assert_eq!(r.paths, ["a/**"]);
        assert_eq!(Rule::parse("bare", "---\ndescription: d\n---").body, "");
        assert_eq!(split_frontmatter("---\nunterminated"), (None, "---\nunterminated"));
    }

    #[test]
    #[cfg(unix)]
    fn rm_harness_removes_one_rendering_and_the_package_once_none_are_left() {
        let (tmp, paths) = home();
        let src = pkg_dir(&tmp);
        install(&paths, &Source::Path(src), &[Harness::Codex, Harness::OpenCode]).unwrap();
        let warnings = rm_harness(&paths, "fix", Harness::Codex).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let rec = Index::load(&paths).unwrap().packages["fix"].clone();
        assert_eq!(rec.harnesses, vec!["opencode".to_string()]);
        assert!(rec.files.iter().all(|f| f.harness != "codex") && rec.keys.iter().all(|k| k.harness != "codex"));
        assert!(rec.hooks.is_empty() && rec.sections.iter().all(|s| s.harness == "opencode"));
        assert!(std::fs::symlink_metadata(paths.codex_skills().join("alpha")).is_err());
        assert!(!paths.codex_agents_md().exists());
        assert!(!std::fs::read_to_string(Harness::Codex.config_path(tmp.path())).unwrap().contains("fixmcp"));
        assert!(std::fs::read_link(paths.opencode_skills().join("alpha")).is_ok(), "opencode untouched");
        assert!(
            std::fs::read_link(paths.smooth_skills().join("alpha")).is_ok(),
            "smooth's own links stay while a harness remains"
        );
        assert!(rm_harness(&paths, "fix", Harness::Codex).unwrap().is_empty(), "idempotent");
        rm_harness(&paths, "fix", Harness::OpenCode).unwrap();
        assert!(Index::load(&paths).unwrap().packages.is_empty());
        assert!(std::fs::symlink_metadata(paths.smooth_skills().join("alpha")).is_err());
        assert!(
            rm_harness(&paths, "fix", Harness::OpenCode).unwrap().is_empty(),
            "unknown package is not an error"
        );
    }

    #[test]
    #[cfg(unix)]
    fn status_reports_drift_and_rm_keeps_user_modified_copies() {
        let (tmp, paths) = home();
        let src = pkg_dir(&tmp);
        install(&paths, &Source::Path(src), &[Harness::ClaudeCode, Harness::OpenCode]).unwrap();
        let rec = Index::load(&paths).unwrap().packages["fix"].clone();
        assert!(status_lines(&rec).iter().any(|l| l.contains("0 drifted")));

        // User edits a rendered rule, repoints a skill link, deletes a key.
        let rule = paths.claude_rules().join("fix/style.md");
        std::fs::write(&rule, "my own rule").unwrap();
        let link = paths.opencode_skills().join("alpha");
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("/elsewhere", &link).unwrap();
        let mut doc = load_json(&paths.claude_settings()).unwrap();
        json_remove(&mut doc, &["enabledPlugins", "fix@th-pkg"]);
        save_json(&paths.claude_settings(), &doc).unwrap();

        let lines = status_lines(&rec).join("\n");
        assert!(lines.contains("modified (hash mismatch)"), "{lines}");
        assert!(lines.contains("points elsewhere"), "{lines}");
        assert!(lines.contains("enabledPlugins.fix@th-pkg — key missing"), "{lines}");
        assert!(lines.contains("3 drifted"), "{lines}");

        let warnings = rm(&paths, "fix").unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(std::fs::read_to_string(&rule).unwrap(), "my own rule");
    }

    #[test]
    fn claude_handoff_skips_when_the_plugin_is_already_enabled_elsewhere() {
        let (tmp, paths) = home();
        let src = pkg_dir(&tmp);
        std::fs::write(paths.claude_settings(), r#"{"enabledPlugins":{"fix@smooth":true}}"#).unwrap();
        install(&paths, &Source::Path(src), &[Harness::ClaudeCode]).unwrap();
        let rec = &Index::load(&paths).unwrap().packages["fix"];
        assert!(rec.claude_plugin.is_none());
        assert!(rec.notes.iter().any(|n| n.contains("already enabled in Claude as fix@smooth")));
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.claude_settings()).unwrap()).unwrap();
        assert!(settings["enabledPlugins"].get("fix@th-pkg").is_none());
        assert!(!paths.claude_marketplace().exists());
        // Rules are still ours to render.
        assert!(paths.claude_rules().join("fix/style.md").is_file());
    }

    #[test]
    fn github_marketplace_handoff_registers_the_real_marketplace_and_owns_only_what_it_added() {
        let (tmp, paths) = home();
        let src = pkg_dir(&tmp);
        let cached = paths.cache().join("fix@v1");
        copy_dir(&src, &cached, true).unwrap();
        let fetched = || Fetched {
            root: paths.cache().join("fix@v1"),
            source: "github:o/r/sub#v1".into(),
            github_marketplace: Some(("mk".into(), "o/r".into())),
        };
        // The user already knows this marketplace → not ours.
        std::fs::write(
            paths.claude_settings(),
            r#"{"extraKnownMarketplaces":{"mk":{"source":{"source":"github","repo":"o/r"}}}}"#,
        )
        .unwrap();
        install_root(&paths, &fetched(), &[Harness::ClaudeCode]).unwrap();
        let rec = Index::load(&paths).unwrap().packages["fix"].clone();
        assert_eq!(rec.claude_plugin.as_deref(), Some("fix@mk"));
        assert!(!paths.claude_marketplace().exists(), "no local copy when the repo's marketplace serves it");
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.claude_settings()).unwrap()).unwrap();
        assert_eq!(settings["enabledPlugins"]["fix@mk"], true);
        rm(&paths, "fix").unwrap();
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.claude_settings()).unwrap()).unwrap();
        assert_eq!(
            settings["extraKnownMarketplaces"]["mk"]["source"]["repo"], "o/r",
            "pre-existing marketplace survives rm"
        );
        assert!(settings.get("enabledPlugins").is_none());

        // Fresh settings → we register the marketplace, and rm takes it back out.
        std::fs::remove_file(paths.claude_settings()).unwrap();
        copy_dir(&src, &paths.cache().join("fix@v1"), true).unwrap();
        install_root(&paths, &fetched(), &[Harness::ClaudeCode]).unwrap();
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.claude_settings()).unwrap()).unwrap();
        assert_eq!(settings["extraKnownMarketplaces"]["mk"]["source"]["source"], "github");
        rm(&paths, "fix").unwrap();
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(paths.claude_settings()).unwrap()).unwrap();
        assert_eq!(settings, serde_json::json!({}));
    }

    #[test]
    #[cfg(unix)]
    fn installing_for_one_more_harness_keeps_the_earlier_renderings() {
        let (tmp, paths) = home();
        let src = pkg_dir(&tmp);
        install(&paths, &Source::Path(src.clone()), &[Harness::OpenCode]).unwrap();
        install(&paths, &Source::Path(src), &[Harness::Codex]).unwrap();
        let rec = &Index::load(&paths).unwrap().packages["fix"];
        assert_eq!(rec.harnesses, vec!["codex".to_string(), "opencode".to_string()]);
        assert!(std::fs::read_link(paths.opencode_skills().join("alpha")).is_ok(), "opencode rendering survived");
        assert!(std::fs::read_link(paths.codex_skills().join("alpha")).is_ok());
    }

    #[test]
    fn harness_not_installed_is_skipped_with_a_note() {
        let tmp = TempDir::new().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        std::fs::create_dir_all(Harness::OpenCode.marker_dir(tmp.path())).unwrap();
        let src = pkg_dir(&tmp);
        install(&paths, &Source::Path(src), &Harness::ALL).unwrap();
        let rec = &Index::load(&paths).unwrap().packages["fix"];
        assert!(rec.notes.iter().any(|n| n.starts_with("codex: not installed")));
        assert!(rec.notes.iter().any(|n| n.starts_with("claude-code: not installed")));
        assert!(!Harness::Codex.config_path(tmp.path()).exists());
        assert!(Harness::OpenCode.config_path(tmp.path()).exists());
    }

    #[test]
    fn index_round_trips_through_toml() {
        let (tmp, paths) = home();
        let mut index = Index::default();
        index.packages.insert(
            "p".into(),
            Installed {
                version: Some("1".into()),
                source: "github:o/r#v1".into(),
                installed_at: "2026-09-08T00:00:00Z".into(),
                root: tmp.path().join("root"),
                harnesses: vec!["codex".into()],
                claude_plugin: None,
                notes: vec!["n".into()],
                files: vec![OwnedFile {
                    harness: "codex".into(),
                    path: tmp.path().join("f"),
                    kind: "file".into(),
                    sha256: "ab".into(),
                }],
                keys: vec![OwnedKey {
                    harness: "codex".into(),
                    file: tmp.path().join("c.toml"),
                    key: vec!["plugins".into(), "x@y".into(), "enabled".into()],
                }],
                hooks: vec![OwnedHook {
                    harness: "codex".into(),
                    file: tmp.path().join("hooks.json"),
                    event: "SessionStart".into(),
                    matcher: String::new(),
                    command: "flow.sh SessionStart codex".into(),
                }],
                sections: vec![OwnedSection {
                    harness: "codex".into(),
                    file: tmp.path().join("AGENTS.md"),
                    name: "p".into(),
                    sha256: "cd".into(),
                    created: true,
                }],
            },
        );
        index.save(&paths).unwrap();
        assert_eq!(Index::load(&paths).unwrap(), index);
        std::fs::write(paths.index_file(), "not = [toml").unwrap();
        assert!(Index::load(&paths).is_err());
    }

    #[test]
    fn toml_fragment_merge_owns_leaves_and_removal_prunes_empty_tables() {
        let tmp = TempDir::new().unwrap();
        let t = tmp.path().join("config.toml");
        std::fs::write(&t, "# c\nmodel = \"m\"\n[features]\nmine = 1\n").unwrap();
        let keys = toml_merge_fragment(&t, "top = 1\n[features]\nours = true\n[plugins.\"a@b\"]\nenabled = true\n").unwrap();
        assert_eq!(
            keys,
            vec![
                vec!["top".to_string()],
                vec!["features".to_string(), "ours".to_string()],
                vec!["plugins".to_string(), "a@b".to_string(), "enabled".to_string()],
            ]
        );
        let out = std::fs::read_to_string(&t).unwrap();
        assert!(
            out.contains("# c") && out.contains("mine = 1") && out.contains("ours = true") && out.contains("[plugins.\"a@b\"]"),
            "{out}"
        );
        for k in &keys {
            let segs: Vec<&str> = k.iter().map(String::as_str).collect();
            toml_remove_key(&t, &segs).unwrap();
        }
        let out = std::fs::read_to_string(&t).unwrap();
        assert!(
            out.contains("mine = 1") && !out.contains("plugins") && !out.contains("ours") && !out.contains("top"),
            "{out}"
        );
        // Removing again is a no-op, not an error.
        toml_remove_key(&t, &["plugins", "a@b", "enabled"]).unwrap();
    }

    #[test]
    fn json_set_and_remove_prune_empty_parents() {
        let mut doc = serde_json::json!({"keep": 1});
        json_set(&mut doc, &["a", "b", "c"], serde_json::json!(true));
        assert_eq!(doc["a"]["b"]["c"], true);
        assert!(json_remove(&mut doc, &["a", "b", "c"]));
        assert!(doc.get("a").is_none(), "{doc}");
        assert_eq!(doc["keep"], 1);
        assert!(!json_remove(&mut doc, &["nope", "x"]));
    }

    #[test]
    fn init_scaffolds_the_layout_and_refuses_to_overwrite() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path().join("my-pkg");
        std::fs::create_dir_all(&d).unwrap();
        init(&d).unwrap();
        let m = load_manifest(&d).unwrap();
        assert_eq!(m.name, "my-pkg");
        for rel in [
            "skills/example/SKILL.md",
            "rules/conventions.md",
            "harness/claude-code/hooks/hooks.json",
            "harness/codex/config.toml",
            "harness/codex/hooks.json",
            "harness/opencode/plugin.js",
        ] {
            assert!(d.join(rel).is_file(), "{rel}");
        }
        assert!(init(&d).is_err());
    }

    #[test]
    fn parse_harnesses_accepts_lists_and_all() {
        assert_eq!(parse_harnesses("all").unwrap(), Harness::ALL.to_vec());
        assert_eq!(parse_harnesses("codex, claude-code,codex").unwrap(), vec![Harness::Codex, Harness::ClaudeCode]);
        assert_eq!(parse_harnesses("cursor").unwrap(), vec![Harness::Cursor]);
        assert!(parse_harnesses("copilot").is_err());
        assert!(parse_harnesses(",").is_err());
    }
}

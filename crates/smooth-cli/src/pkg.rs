//! `th pkg` — one package, N harness renderings (EPIC th-55b2c7, M0).
//!
//! A package is a Claude Code plugin checkout used as the SHARED CORE
//! (`.claude-plugin/plugin.json`, `skills/`, `commands/`, `agents/`, `hooks/`,
//! `.mcp.json`) plus `rules/*.md` and per-harness OVERLAYS under
//! `harness/<name>/` holding what only that harness understands. `install`
//! fetches the source into `~/.smooth/pkg/cache/`, composes core + overlay per
//! target harness and renders each harness's NATIVE shape:
//!
//! | Harness | Rendering (M0) |
//! |---|---|
//! | claude-code | handed to Claude's own plugin system: composed plugin under the local `th-pkg` marketplace + `enabledPlugins`/`extraKnownMarketplaces` in `~/.claude/settings.json`; `rules/` → `~/.claude/rules/<pkg>/` |
//! | codex | `skills/` → `~/.codex/skills/`, `.mcp.json` → `[mcp_servers.*]`, `harness/codex/config.toml` key-merged into `~/.codex/config.toml` |
//! | opencode | `skills/` → `~/.opencode/skills/`, `.mcp.json` → `mcp.*`, `harness/opencode/plugin.js` → `~/.config/opencode/plugins/<pkg>.js` |
//! | (all) | `skills/` → `~/.smooth/skills/` so `th` itself discovers them |
//!
//! Every written path (+ sha256) and every owned dotted key in a merged config
//! file is recorded in `~/.smooth/pkg/index.toml`, so `rm` removes exactly
//! what was installed and `status` reports drift. Hooks are never translated
//! between harnesses — they are per-harness customization points, and
//! `status` says which ones a package provides.

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
        /// claude-code | codex | opencode | all (comma-separated)
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
        bail!("no harness given (expected claude-code|codex|opencode|all)");
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
    if let Some(prev) = index.packages.remove(&m.name) {
        for w in remove_rendered(paths, &prev)? {
            println!("   {} {w}", "!".bright_yellow());
        }
        for h in prev.harnesses.iter().filter_map(|h| Harness::parse(h).ok()) {
            if !harnesses.contains(&h) {
                harnesses.push(h);
            }
        }
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
    Ok(())
}

fn render_mcp(paths: &Paths, harness: Harness, m: &Manifest, rec: &mut Installed) -> Result<()> {
    for s in &m.mcp_servers {
        mcp_install::install_server_into(harness, &paths.home, s, false)?;
        let table = if harness == Harness::Codex { "mcp_servers" } else { "mcp" };
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
    let warnings = remove_rendered(paths, &rec)?;
    index.save(paths)?;
    write_claude_marketplace(paths, &index)?;
    let _ = std::fs::remove_dir_all(&rec.root);
    Ok(warnings)
}

fn remove_rendered(paths: &Paths, rec: &Installed) -> Result<Vec<String>> {
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
    let _ = paths;
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

/// Which per-harness overlays this package provides vs. what M0 renders.
fn customization_points(root: &Path) -> Vec<String> {
    let has = |rel: &str| root.join(rel).exists();
    let point = |label: &str, present: bool| format!("{label}={}", if present { "present" } else { "absent" });
    vec![
        point("claude-code/hooks", has("hooks/hooks.json") || has("harness/claude-code/hooks/hooks.json")),
        point("codex/config.toml", has("harness/codex/config.toml")),
        point("opencode/plugin.js", has("harness/opencode/plugin.js")),
        point("cursor (M1)", has("harness/cursor")),
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
    let files: [(&str, String); 6] = [
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
        w("hooks/hooks.json", r#"{"hooks":{}}"#);
        w("harness/claude-code/hooks/hooks.json", r#"{"hooks":{"PreToolUse":[]}}"#);
        w("harness/codex/config.toml", "[features]\nfix = true\n[plugins.\"fix@th\"]\nenabled = true\n");
        w("harness/opencode/plugin.js", "export const Fix = 1;");
        w("harness/cursor/README.md", "later");
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
        // The claude-code overlay REPLACES core hooks.json.
        assert!(std::fs::read_to_string(composed.join("hooks/hooks.json")).unwrap().contains("PreToolUse"));
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
                .any(|l| l.contains("opencode/plugin.js=present") && l.contains("cursor (M1)=present")),
            "{lines:?}"
        );

        // Idempotent reinstall: same artifact count, no duplicates.
        install(&paths, &Source::Path(src.clone()), &Harness::ALL).unwrap();
        let again = Index::load(&paths).unwrap();
        assert_eq!(again.packages["fix"].files.len(), rec.files.len());
        assert_eq!(again.packages["fix"].keys.len(), rec.keys.len());
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
        assert!(rm(&paths, "fix").is_err(), "second rm reports unknown package");
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
        assert!(parse_harnesses("cursor").is_err());
        assert!(parse_harnesses(",").is_err());
    }
}

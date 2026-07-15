//! `th ext` — manage SEP (Smooth Extension Protocol) extensions.
//!
//! Install an extension from a local directory, an npm package
//! (`npm:@scope/pkg[@ver]`), or a git repo (`git:host/user/repo[@ref]`) into
//! `~/.smooth/extensions` (global) or `<repo>/.smooth/extensions` (project),
//! list what's installed with its trust state, trust one, update the packaged
//! ones, or remove it. The trust store + content hashing live in
//! [`smooth_policy::ext_trust`] (re-exported through [`smooth_code::sep_host`])
//! so the host reads exactly what this writes.
//!
//! ## Layout for packaged installs
//!
//! An npm package is vendored under `<store>/.npm` (an `npm install --prefix`
//! tree so the package's own deps resolve), a git repo under
//! `<store>/.git/<host>/<path>`. Neither vendor dir is itself an extension (no
//! top-level `extension.toml`), so the engine's discovery — which only scans the
//! immediate children of the store for an `extension.toml` — ignores them. We
//! then create a `<store>/<name>` symlink pointing at the vendored extension
//! directory; that symlink IS discovered, and the manifest's relative `run.args`
//! / `[resources]` resolve through it. This keeps the engine's discovery
//! unchanged while supporting packaged sources.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use smooth_operator::extension::manifest::{default_global_dir, project_dir};
use smooth_operator::extension::{Capabilities, ExtensionManifest};
use smooth_policy::ext_trust::{hash_extension, TrustStore};

#[derive(Subcommand)]
pub enum ExtCommands {
    /// Install an extension into the global (`~/.smooth/extensions`) or project
    /// (`--project`) store, then prompt to trust it. The source is a local
    /// directory containing `extension.toml`, an npm package
    /// (`npm:@scope/pkg[@version]`), or a git repo (`git:host/user/repo[@ref]`).
    Install {
        /// Local path, `npm:@scope/pkg[@version]`, or `git:host/user/repo[@ref]`.
        source: String,
        /// Install into the current repo's `.smooth/extensions` instead of the
        /// global store.
        #[arg(long)]
        project: bool,
        /// Trust without prompting (for scripts / CI).
        #[arg(long)]
        trust: bool,
    },
    /// Search the extension marketplace: the curated index shipped in `th` plus
    /// live npm packages tagged with the `smooth-extension` keyword. Prints the
    /// install command for each hit.
    Search {
        /// Words to match against name / description / keywords.
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// Re-fetch packaged (npm:/git:) extensions from their recorded source and
    /// reconcile. Names an extension to update just that one, or updates all.
    Update {
        /// Only update this extension.
        name: Option<String>,
        /// Operate on the project store instead of the global one.
        #[arg(long)]
        project: bool,
        /// Re-trust without prompting when the manifest changed (scripts / CI).
        #[arg(long)]
        trust: bool,
    },
    /// List installed extensions (global + project) with their trust state.
    List,
    /// Trust an installed extension by name (records its current content hash).
    Trust {
        name: String,
        #[arg(long)]
        project: bool,
    },
    /// Remove an installed extension and its trust record.
    Remove {
        name: String,
        #[arg(long)]
        project: bool,
    },
    /// Re-validate an installed extension after editing it: re-parse its
    /// manifest, re-hash it, and (if the manifest changed) re-confirm trust so
    /// the next host start picks up the new version. A live host reloads it
    /// in-session over the daemon relay (SEP Phase 6).
    Reload {
        name: String,
        #[arg(long)]
        project: bool,
        /// Re-trust without prompting (for scripts / CI).
        #[arg(long)]
        trust: bool,
    },
}

pub fn dispatch(cmd: ExtCommands) -> Result<()> {
    match cmd {
        ExtCommands::Install { source, project, trust } => install(&source, project, trust),
        ExtCommands::Search { query } => search(&query.join(" ")),
        ExtCommands::Update { name, project, trust } => update(name.as_deref(), project, trust),
        ExtCommands::List => list(),
        ExtCommands::Trust { name, project } => trust_cmd(&name, project),
        ExtCommands::Remove { name, project } => remove(&name, project),
        ExtCommands::Reload { name, project, trust } => reload(&name, project, trust),
    }
}

// ---------------------------------------------------------------------------
// Source grammar
// ---------------------------------------------------------------------------

/// A parsed `th ext install` source.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// A local directory containing `extension.toml`.
    Local(PathBuf),
    /// `npm:@scope/pkg[@version]` — `spec` is everything after `npm:`.
    Npm { spec: String },
    /// `git:host/user/repo[@ref]`.
    Git {
        url: String,
        host: String,
        path: String,
        reff: Option<String>,
    },
}

impl Source {
    /// The canonical label recorded in the trust store so `th ext update` can
    /// re-fetch. Local installs record their path; packaged installs record the
    /// original `npm:`/`git:` spec.
    fn label(&self) -> String {
        match self {
            Self::Local(p) => p.to_string_lossy().into_owned(),
            Self::Npm { spec } => format!("npm:{spec}"),
            Self::Git { host, path, reff, .. } => reff.as_ref().map_or_else(|| format!("git:{host}/{path}"), |r| format!("git:{host}/{path}@{r}")),
        }
    }
}

/// Parse an install source. Anything not prefixed `npm:`/`git:` is a local path.
fn parse_source(raw: &str) -> Result<Source> {
    let raw = raw.trim();
    if let Some(spec) = raw.strip_prefix("npm:") {
        let spec = spec.trim();
        if spec.is_empty() {
            bail!("empty npm spec — expected `npm:@scope/pkg[@version]`");
        }
        return Ok(Source::Npm { spec: spec.to_string() });
    }
    if let Some(rest) = raw.strip_prefix("git:") {
        return parse_git(rest.trim());
    }
    Ok(Source::Local(PathBuf::from(raw)))
}

/// Parse the `host/user/repo[@ref]` portion after `git:`.
fn parse_git(rest: &str) -> Result<Source> {
    let slash = rest.find('/').with_context(|| format!("git source `{rest}` must be `host/user/repo[@ref]`"))?;
    let host = rest[..slash].to_string();
    let path_and_ref = &rest[slash + 1..];
    let (path, reff) = match path_and_ref.split_once('@') {
        Some((p, r)) if !p.is_empty() && !r.is_empty() => (p.to_string(), Some(r.to_string())),
        _ => (path_and_ref.to_string(), None),
    };
    if host.is_empty() || path.is_empty() {
        bail!("git source `{rest}` must be `host/user/repo[@ref]`");
    }
    let url = format!("https://{host}/{path}");
    Ok(Source::Git { url, host, path, reff })
}

/// The npm package name from an install spec (drops the version, keeps a scope).
/// `@scope/pkg@1.2.3` → `@scope/pkg`; `pkg@1` → `pkg`.
fn npm_package_name(spec: &str) -> String {
    if let Some(scoped) = spec.strip_prefix('@') {
        match scoped.find('@') {
            Some(i) => format!("@{}", &scoped[..i]),
            None => format!("@{scoped}"),
        }
    } else {
        spec.split('@').next().unwrap_or(spec).to_string()
    }
}

// ---------------------------------------------------------------------------
// install / update
// ---------------------------------------------------------------------------

fn install(source_str: &str, project: bool, auto_trust: bool) -> Result<()> {
    let source = parse_source(source_str)?;
    let store = scope_dir(project)?;

    let (name, dest) = match &source {
        Source::Local(src) => {
            let manifest = ExtensionManifest::load_dir(src).with_context(|| format!("no valid extension.toml in {}", src.display()))?;
            let name = manifest.name.clone();
            let dest = store.join(&name);
            if exists(&dest) {
                bail!(
                    "extension `{name}` is already installed at {} — remove it first (`th ext remove {name}{}`)",
                    dest.display(),
                    if project { " --project" } else { "" }
                );
            }
            copy_dir(src, &dest).with_context(|| format!("copy {} → {}", src.display(), dest.display()))?;
            (name, dest)
        }
        Source::Npm { .. } | Source::Git { .. } => {
            // Peek at the name after vendoring; bail if it collides.
            let (name, dest) = vendor_and_link(&source, &store, None)?;
            (name, dest)
        }
    };

    let scope = if project { "project" } else { "global" };
    println!(
        "\n  {} Installed {} {} → {}",
        "✓".green().bold(),
        name.bold(),
        format!("({scope})").dimmed(),
        dest.display().to_string().dimmed()
    );
    let manifest = ExtensionManifest::load_dir(&dest)?;
    print_capabilities(&manifest.capabilities);

    // First-run trust: hash the installed copy and record the decision, keyed to
    // the source so `th ext update` can re-fetch.
    let hash = hash_extension(&dest)?;
    let trusted = auto_trust || prompt_trust(&name)?;
    let mut trust = TrustStore::load();
    trust.set(&name, &source.label(), &hash, trusted);
    trust.save()?;

    if trusted {
        println!("  {} Trusted. It will load on the next `th code` session.\n", "✓".green().bold());
    } else {
        println!(
            "  {} Not trusted — it will NOT load. Run {} to enable it.\n",
            "⚠".yellow(),
            format!("th ext trust {name}").cyan()
        );
    }
    Ok(())
}

fn update(only: Option<&str>, project: bool, auto_trust: bool) -> Result<()> {
    let store = scope_dir(project)?;
    let trust = TrustStore::load();

    // Packaged extensions are the ones whose recorded source is an npm:/git: spec.
    let targets: Vec<(String, String, bool)> = trust
        .extensions
        .iter()
        .filter(|(name, _)| only.is_none_or(|want| *name == want))
        .filter_map(|(name, rec)| {
            let is_pkg = rec.source.starts_with("npm:") || rec.source.starts_with("git:");
            is_pkg.then(|| (name.clone(), rec.source.clone(), rec.trusted))
        })
        .collect();

    if targets.is_empty() {
        match only {
            Some(name) => bail!("`{name}` is not a packaged (npm:/git:) extension, or is not installed"),
            None => {
                println!("\n  {} No packaged (npm:/git:) extensions to update.\n", "ℹ".cyan());
                return Ok(());
            }
        }
    }

    for (name, source_label, was_trusted) in targets {
        let source = parse_source(&source_label)?;
        println!("\n  {} Updating {} {}", "↻".cyan().bold(), name.bold(), format!("({source_label})").dimmed());

        // Remove the old symlink + vendored payload, then re-fetch.
        remove_installed(&store, &name).ok();
        let (new_name, dest) = vendor_and_link(&source, &store, Some(&name))?;

        let manifest = ExtensionManifest::load_dir(&dest)?;
        print_capabilities(&manifest.capabilities);

        // Reconcile trust: a byte-identical manifest keeps its prior decision; a
        // changed one is untrusted until re-confirmed (fail-safe).
        let hash = hash_extension(&dest)?;
        let mut trust = TrustStore::load();
        let prior = trust.extensions.get(&new_name).cloned();
        let unchanged = prior.as_ref().is_some_and(|r| r.hash == hash);
        let trusted = if unchanged {
            was_trusted
        } else if auto_trust {
            true
        } else if was_trusted {
            reconfirm_trust(&new_name)?
        } else {
            false
        };
        trust.set(&new_name, &source.label(), &hash, trusted);
        trust.save()?;

        if trusted {
            println!("  {} Updated and trusted.", "✓".green().bold());
        } else {
            println!("  {} Updated — left untrusted; run `th ext trust {new_name}` to enable it.", "⚠".yellow());
        }
    }
    println!();
    Ok(())
}

/// Fetch a packaged source into the store's vendor dir and create the
/// `<store>/<name>` discovery symlink. Returns `(name, symlink_path)`.
///
/// `expected` names an in-progress update: the symlink is allowed to be
/// replaced. For a fresh install (`None`) a name collision is an error.
fn vendor_and_link(source: &Source, store: &Path, expected: Option<&str>) -> Result<(String, PathBuf)> {
    let payload = match source {
        Source::Npm { spec } => vendor_npm(spec, store)?,
        Source::Git { url, host, path, reff } => vendor_git(url, host, path, reff.as_deref(), store)?,
        Source::Local(_) => bail!("vendor_and_link called with a local source"),
    };

    ensure_manifest(&payload)?;
    let manifest = ExtensionManifest::load_dir(&payload).with_context(|| format!("vendored extension at {} has no valid manifest", payload.display()))?;
    let name = manifest.name.clone();

    let link = store.join(&name);
    if expected.is_none() && exists(&link) {
        bail!(
            "extension `{name}` is already installed at {} — remove it first (`th ext remove {name}`)",
            link.display()
        );
    }
    symlink_dir(&payload, &link).with_context(|| format!("link {} → {}", link.display(), payload.display()))?;
    Ok((name, link))
}

/// `npm install <spec> --prefix <store>/.npm`, returning the installed package
/// directory (`<store>/.npm/node_modules/<pkg>`).
fn vendor_npm(spec: &str, store: &Path) -> Result<PathBuf> {
    let npm_root = store.join(".npm");
    std::fs::create_dir_all(&npm_root)?;
    run(
        "npm",
        &[
            "install",
            spec,
            "--prefix",
            &npm_root.to_string_lossy(),
            "--legacy-peer-deps",
            "--no-audit",
            "--no-fund",
        ],
        None,
    )
    .context("npm install failed — is `npm` on PATH?")?;
    let pkg = npm_root.join("node_modules").join(npm_package_name(spec));
    if !pkg.is_dir() {
        bail!("npm installed `{spec}` but package dir {} was not found", pkg.display());
    }
    Ok(pkg)
}

/// `git clone` (+ optional `checkout <ref>`, + `npm install` when the repo
/// carries a package.json) into `<store>/.git/<host>/<path>`.
fn vendor_git(url: &str, host: &str, path: &str, reff: Option<&str>, store: &Path) -> Result<PathBuf> {
    let dest = store.join(".git").join(host).join(path);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).ok();
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    run("git", &["clone", url, &dest.to_string_lossy()], None).with_context(|| format!("git clone {url} failed"))?;
    if let Some(r) = reff {
        run("git", &["checkout", r], Some(&dest)).with_context(|| format!("git checkout {r} failed"))?;
    }
    if dest.join("package.json").is_file() {
        run("npm", &["install", "--omit=dev", "--legacy-peer-deps", "--no-audit", "--no-fund"], Some(&dest))
            .context("npm install for the cloned extension failed")?;
    }
    Ok(dest)
}

/// If `<dir>` has no `extension.toml` but its `package.json` carries a `smooth`
/// key, synthesize `extension.toml` from it (name/version fall back to
/// package.json). This is the "package.json manifest key" path — downstream
/// discovery/trust/host all stay `extension.toml`-only.
fn ensure_manifest(dir: &Path) -> Result<()> {
    if dir.join("extension.toml").is_file() {
        return Ok(());
    }
    let pkg_path = dir.join("package.json");
    let pkg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pkg_path).with_context(|| format!("no extension.toml and no package.json in {}", dir.display()))?)
            .with_context(|| format!("parse {}", pkg_path.display()))?;
    let mut manifest_val = pkg
        .get("smooth")
        .cloned()
        .with_context(|| format!("{} has neither an extension.toml nor a `smooth` manifest key in package.json", dir.display()))?;
    for key in ["name", "version"] {
        if manifest_val.get(key).is_none() {
            if let Some(v) = pkg.get(key) {
                if let Some(obj) = manifest_val.as_object_mut() {
                    obj.insert(key.to_string(), v.clone());
                }
            }
        }
    }
    let manifest: ExtensionManifest =
        serde_json::from_value(manifest_val).with_context(|| format!("the `smooth` key in {} is not a valid extension manifest", pkg_path.display()))?;
    let toml_text = toml::to_string_pretty(&manifest).context("serialize synthesized extension.toml")?;
    std::fs::write(dir.join("extension.toml"), toml_text)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// marketplace search
// ---------------------------------------------------------------------------

/// The curated index shipped in the binary. Small on purpose — live npm keyword
/// search is the primary discovery path; this supplements it with hand-picked
/// entries (scaffolds, git-only extensions) npm can't surface.
const INDEX_JSON: &str = include_str!("ext_index.json");

#[derive(Debug, Clone, serde::Deserialize)]
struct IndexEntry {
    name: String,
    #[serde(default)]
    description: String,
    /// Install source (`npm:…`, `git:…`) or, for a scaffold, the command to run.
    source: String,
    #[serde(default)]
    keywords: Vec<String>,
    /// `extension` (installable via `th ext install`) or `scaffold` (run `source`).
    #[serde(default = "default_kind")]
    kind: String,
}

fn default_kind() -> String {
    "extension".to_string()
}

#[derive(Debug, serde::Deserialize)]
struct MarketplaceIndex {
    #[serde(default)]
    extensions: Vec<IndexEntry>,
}

fn search(query: &str) -> Result<()> {
    let q = query.trim().to_lowercase();
    let index: MarketplaceIndex = serde_json::from_str(INDEX_JSON).context("parse embedded extension index")?;
    let mut hits: Vec<IndexEntry> = index.extensions.into_iter().filter(|e| entry_matches(e, &q)).collect();

    // Best-effort live npm keyword search. Network failure just means fewer
    // results — never an error.
    match npm_search(&q) {
        Ok(npm) => {
            for e in npm {
                if !hits.iter().any(|h| h.name == e.name) {
                    hits.push(e);
                }
            }
        }
        Err(e) => tracing::debug!(error = %e, "npm marketplace search skipped"),
    }

    if hits.is_empty() {
        println!("\n  {} No extensions match {}.\n", "ℹ".cyan(), format!("`{query}`").bold());
        return Ok(());
    }

    println!("\n  {} for {}", "Extensions".cyan().bold(), format!("`{query}`").bold());
    for e in &hits {
        println!("\n    {} {}", e.name.bold(), format!("[{}]", e.kind).dimmed());
        if !e.description.is_empty() {
            println!("    {}", e.description.dimmed());
        }
        let hint = if e.kind == "scaffold" {
            e.source.clone()
        } else {
            format!("th ext install {}", e.source)
        };
        println!("    {} {}", "→".dimmed(), hint.cyan());
    }
    println!();
    Ok(())
}

/// Case-insensitive match of a lowercased query against name/description/keywords.
/// An empty query matches everything.
fn entry_matches(e: &IndexEntry, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    e.name.to_lowercase().contains(q) || e.description.to_lowercase().contains(q) || e.keywords.iter().any(|k| k.to_lowercase().contains(q))
}

/// Query the npm registry for packages tagged `smooth-extension`, narrowed by
/// the user's query. Maps each to an installable `npm:<name>` entry.
///
/// `th` dispatches inside a tokio runtime, and `reqwest::blocking` spins up its
/// own runtime that panics if dropped on an async worker thread — so run the
/// blocking request on a plain OS thread, off the async context.
fn npm_search(q: &str) -> Result<Vec<IndexEntry>> {
    let q = q.to_string();
    std::thread::scope(|s| s.spawn(|| npm_search_blocking(&q)).join()).map_err(|_| anyhow::anyhow!("npm search thread panicked"))?
}

fn npm_search_blocking(q: &str) -> Result<Vec<IndexEntry>> {
    let text = if q.is_empty() {
        "keywords:smooth-extension".to_string()
    } else {
        format!("keywords:smooth-extension {q}")
    };
    let url = format!("https://registry.npmjs.org/-/v1/search?text={}&size=20", urlencoding::encode(&text));
    let client = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(6)).build()?;
    let body: serde_json::Value = client.get(&url).send()?.error_for_status()?.json()?;

    let mut out = Vec::new();
    for obj in body.get("objects").and_then(|v| v.as_array()).into_iter().flatten() {
        let Some(pkg) = obj.get("package") else { continue };
        let Some(name) = pkg.get("name").and_then(|v| v.as_str()) else { continue };
        out.push(IndexEntry {
            name: name.to_string(),
            description: pkg.get("description").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            source: format!("npm:{name}"),
            keywords: pkg
                .get("keywords")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            kind: "extension".to_string(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// list / trust / reload / remove
// ---------------------------------------------------------------------------

fn list() -> Result<()> {
    let store = TrustStore::load();
    let mut any = false;
    for (project, label) in [(false, "global"), (true, "project")] {
        let Ok(dir) = scope_dir(project) else { continue };
        let mut entries = installed_in(&dir);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        if entries.is_empty() {
            continue;
        }
        any = true;
        println!("\n  {} {}", label.cyan().bold(), format!("({})", dir.display()).dimmed());
        for (name, root) in entries {
            let hash = hash_extension(&root).unwrap_or_default();
            let (marker, tag) = if store.is_trusted(&name, &hash) {
                ("✓".green().bold().to_string(), "[trusted]".dimmed().to_string())
            } else if store.extensions.contains_key(&name) {
                // Present but hash changed or explicitly distrusted.
                ("⚠".yellow().to_string(), "[untrusted — re-run `th ext trust`]".yellow().to_string())
            } else {
                ("○".dimmed().to_string(), "[untrusted]".yellow().to_string())
            };
            let src = store.extensions.get(&name).map(|r| r.source.clone()).unwrap_or_default();
            let src_tag = if src.starts_with("npm:") || src.starts_with("git:") {
                format!(" {}", format!("({src})").dimmed())
            } else {
                String::new()
            };
            println!("    {} {:<20} {}{}", marker, name.bold(), tag, src_tag);
        }
    }
    if any {
        println!();
    } else {
        println!(
            "\n  {} No extensions installed. {} {}\n",
            "ℹ".cyan(),
            "Install one:".dimmed(),
            "th ext install ./path".cyan()
        );
    }
    Ok(())
}

fn trust_cmd(name: &str, project: bool) -> Result<()> {
    let dir = scope_dir(project)?.join(name);
    if !dir.join("extension.toml").exists() {
        bail!("extension `{name}` is not installed{}", if project { " (project)" } else { "" });
    }
    let hash = hash_extension(&dir)?;
    let mut store = TrustStore::load();
    // Preserve the recorded source (npm:/git:/path) so updates keep working.
    let source = store
        .extensions
        .get(name)
        .map_or_else(|| dir.to_string_lossy().into_owned(), |r| r.source.clone());
    store.set(name, &source, &hash, true);
    store.save()?;
    println!("  {} Trusted {}.", "✓".green().bold(), name.bold());
    Ok(())
}

fn reload(name: &str, project: bool, auto_trust: bool) -> Result<()> {
    let dir = scope_dir(project)?.join(name);
    // Re-parse the manifest — surfaces a syntax error the edit may have introduced.
    let manifest = ExtensionManifest::load_dir(&dir).with_context(|| format!("extension `{name}` is not installed or its extension.toml is invalid"))?;

    let hash = hash_extension(&dir)?;
    let mut store = TrustStore::load();
    let record = store.extensions.get(name).cloned();
    let changed = record.as_ref().map(|r| r.hash != hash).unwrap_or(true);

    println!(
        "\n  {} Reloading {} {}",
        "↻".cyan().bold(),
        name.bold(),
        format!("v{}", manifest.version).dimmed()
    );
    print_capabilities(&manifest.capabilities);

    let mut trusted_now = record.as_ref().is_some_and(|r| r.trusted);
    if !changed {
        // Manifest unchanged since it was trusted → nothing to re-confirm.
        let state = if trusted_now {
            format!("{} still trusted", "✓".green().bold())
        } else {
            format!("{} still untrusted — run `th ext trust {name}`", "⚠".yellow())
        };
        println!("  {state} (manifest unchanged).");
    } else {
        // The manifest changed (or was never trusted) — fail-safe requires a
        // fresh trust decision before it will load.
        let source = record.map_or_else(|| dir.to_string_lossy().into_owned(), |r| r.source);
        trusted_now = auto_trust || prompt_trust(name)?;
        store.set(name, &source, &hash, trusted_now);
        store.save()?;
        if trusted_now {
            println!("  {} Manifest changed — re-trusted.", "✓".green().bold());
        } else {
            println!("  {} Manifest changed — left untrusted; it will NOT load.", "⚠".yellow());
        }
    }

    // Live-reload the running Big Smooth daemon's chat-loop host (pearl
    // th-6d8606). Best-effort: daemon down or the extension not loaded there
    // falls back to the next-session message.
    if trusted_now && daemon_reload(name) {
        println!("  {} Hot-reloaded in the running Big Smooth daemon.\n", "✓".green().bold());
    } else {
        println!("  {} Takes effect on the next {} session / daemon start.\n", "ℹ".cyan(), "th code".cyan());
    }
    Ok(())
}

/// POST `/api/ext/reload` on the local Big Smooth daemon so a live chat-loop
/// host respawns the extension immediately. Returns `false` on any failure
/// (daemon down, extension not loaded there) — reload is best-effort and the
/// caller prints the next-session fallback. Same scoped-thread
/// `reqwest::blocking` pattern as [`npm_search`].
fn daemon_reload(name: &str) -> bool {
    let url = format!(
        "{}/api/ext/reload",
        std::env::var("SMOOTH_BIGSMOOTH_URL").unwrap_or_else(|_| "http://127.0.0.1:4400".into())
    );
    let name = name.to_string();
    std::thread::scope(|s| {
        s.spawn(move || {
            let Ok(client) = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(5)).build() else {
                return false;
            };
            client
                .post(&url)
                .json(&serde_json::json!({ "name": name }))
                .send()
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        })
        .join()
    })
    .unwrap_or(false)
}

fn remove(name: &str, project: bool) -> Result<()> {
    let store = scope_dir(project)?;
    if !exists(&store.join(name)) {
        bail!("extension `{name}` is not installed{}", if project { " (project)" } else { "" });
    }
    remove_installed(&store, name)?;
    let mut trust = TrustStore::load();
    if trust.remove(name) {
        trust.save()?;
    }
    println!("  {} Removed {}.", "✓".green().bold(), name.bold());
    Ok(())
}

/// Remove `<store>/<name>` and, if it is a symlink into the vendor dir, the
/// payload it points at. Copied-in local installs are plain directories.
fn remove_installed(store: &Path, name: &str) -> Result<()> {
    let entry = store.join(name);
    let meta = std::fs::symlink_metadata(&entry).with_context(|| format!("stat {}", entry.display()))?;
    if meta.file_type().is_symlink() {
        if let Ok(target) = std::fs::read_link(&entry) {
            std::fs::remove_dir_all(&target).ok();
        }
        std::fs::remove_file(&entry).with_context(|| format!("remove link {}", entry.display()))?;
    } else {
        std::fs::remove_dir_all(&entry).with_context(|| format!("remove {}", entry.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// The extensions dir for the requested scope.
fn scope_dir(project: bool) -> Result<PathBuf> {
    if project {
        let cwd = std::env::current_dir().context("get current dir")?;
        Ok(project_dir(&cwd))
    } else {
        default_global_dir().context("no home dir for the global extensions store")
    }
}

/// True if `p` exists as a directory OR a (possibly dangling) symlink.
fn exists(p: &Path) -> bool {
    p.exists() || std::fs::symlink_metadata(p).is_ok()
}

/// Run a command, failing on non-zero exit with its stderr.
fn run(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd.output().with_context(|| format!("spawn `{program}`"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("`{program} {}` failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

/// Create a directory symlink at `link` pointing to `target`, replacing any
/// existing entry. Unix only — smooth targets macOS/Linux.
#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> Result<()> {
    if exists(link) {
        // Remove whatever is there (stale symlink or dir) before relinking.
        if std::fs::symlink_metadata(link).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            std::fs::remove_file(link).ok();
        } else {
            std::fs::remove_dir_all(link).ok();
        }
    }
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(not(unix))]
fn symlink_dir(_target: &Path, _link: &Path) -> Result<()> {
    bail!("packaged (npm:/git:) extension installs are only supported on Unix");
}

fn print_capabilities(caps: &Capabilities) {
    let mut on: Vec<&str> = Vec::new();
    for (flag, label) in [
        (caps.tools, "tools"),
        (caps.commands, "commands"),
        (caps.ui, "ui"),
        (caps.exec, "exec"),
        (caps.kv, "kv"),
        (caps.bus, "bus"),
        (caps.session, "session"),
    ] {
        if flag {
            on.push(label);
        }
    }
    let mut line = if on.is_empty() { "(none)".to_string() } else { on.join(", ") };
    if !caps.events.is_empty() {
        line.push_str(&format!("; events: {}", caps.events.join(", ")));
    }
    println!("  {} {}", "Capabilities:".dimmed(), line.yellow());
}

fn prompt_trust(name: &str) -> Result<bool> {
    // Non-interactive (piped/CI) → don't trust silently; require `th ext trust`.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(false);
    }
    let ok = dialoguer::Confirm::new()
        .with_prompt(format!("Trust `{name}` and its declared capabilities?"))
        .default(false)
        .interact()
        .unwrap_or(false);
    Ok(ok)
}

/// Like [`prompt_trust`] but phrased for an update to a previously-trusted
/// extension whose manifest changed.
fn reconfirm_trust(name: &str) -> Result<bool> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(false);
    }
    let ok = dialoguer::Confirm::new()
        .with_prompt(format!("`{name}` changed on update — re-trust it and its declared capabilities?"))
        .default(false)
        .interact()
        .unwrap_or(false);
    Ok(ok)
}

/// Every subdirectory (or symlink to one) of `dir` that carries an
/// `extension.toml`. The `.npm` / `.git` vendor dirs have no top-level
/// `extension.toml`, so they are skipped — only the discovery symlinks show up.
fn installed_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let root = entry.path();
        if root.join("extension.toml").is_file() {
            if let Some(name) = root.file_name().and_then(|n| n.to_str()) {
                out.push((name.to_string(), root));
            }
        }
    }
    out
}

/// Recursively copy `src` into `dest` (std has no recursive copy).
fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_source_classifies_grammar() {
        assert_eq!(parse_source("npm:@foo/bar@1.2.3").unwrap(), Source::Npm { spec: "@foo/bar@1.2.3".into() });
        assert_eq!(parse_source("./local/ext").unwrap(), Source::Local(PathBuf::from("./local/ext")));
        let g = parse_source("git:github.com/acme/ext@main").unwrap();
        assert_eq!(
            g,
            Source::Git {
                url: "https://github.com/acme/ext".into(),
                host: "github.com".into(),
                path: "acme/ext".into(),
                reff: Some("main".into()),
            }
        );
        // No ref.
        let g2 = parse_source("git:github.com/acme/ext").unwrap();
        assert!(matches!(g2, Source::Git { reff: None, .. }));
        // Empty npm spec is rejected.
        assert!(parse_source("npm:").is_err());
    }

    #[test]
    fn source_label_round_trips() {
        for raw in ["npm:@foo/bar@1.0.0", "git:github.com/acme/ext@v2", "git:github.com/acme/ext"] {
            assert_eq!(parse_source(raw).unwrap().label(), raw);
        }
    }

    #[test]
    fn npm_package_name_strips_version_keeps_scope() {
        assert_eq!(npm_package_name("@foo/bar@1.2.3"), "@foo/bar");
        assert_eq!(npm_package_name("@foo/bar"), "@foo/bar");
        assert_eq!(npm_package_name("bar@1"), "bar");
        assert_eq!(npm_package_name("bar"), "bar");
    }

    #[test]
    fn ensure_manifest_synthesizes_from_package_json_smooth_key() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // package.json with a `smooth` manifest key, no top-level name/version there.
        std::fs::write(
            dir.join("package.json"),
            r#"{ "name": "my-ext", "version": "3.1.0", "smooth": { "run": { "command": "node", "args": ["dist/index.js"] }, "capabilities": { "tools": true } } }"#,
        )
        .unwrap();
        ensure_manifest(dir).unwrap();
        let m = ExtensionManifest::load_dir(dir).unwrap();
        assert_eq!(m.name, "my-ext");
        assert_eq!(m.version, "3.1.0");
        assert_eq!(m.run.command, "node");
        assert!(m.capabilities.tools);
        // Idempotent — a second call is a no-op (extension.toml already present).
        ensure_manifest(dir).unwrap();
    }

    #[test]
    fn ensure_manifest_errors_without_manifest_or_smooth_key() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), r#"{ "name": "x", "version": "1" }"#).unwrap();
        assert!(ensure_manifest(tmp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_extension_is_discoverable_and_hashable() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = tmp.path().join(".npm/node_modules/pkg");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("extension.toml"), "name = \"pkg\"\nversion = \"1\"\n[run]\ncommand = \"node\"\n").unwrap();
        let link = tmp.path().join("pkg");
        symlink_dir(&payload, &link).unwrap();

        // Discovery (top-level scan) sees the symlink as an installed extension.
        let found = installed_in(tmp.path());
        assert!(found.iter().any(|(n, _)| n == "pkg"), "symlink should be discovered");
        // The manifest + hash resolve through the symlink.
        assert_eq!(ExtensionManifest::load_dir(&link).unwrap().name, "pkg");
        assert_eq!(hash_extension(&link).unwrap(), hash_extension(&payload).unwrap());

        // Relinking replaces cleanly (idempotent update path).
        symlink_dir(&payload, &link).unwrap();
        assert!(link.exists());
    }

    #[test]
    fn copy_dir_recurses() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("extension.toml"), "name=\"x\"").unwrap();
        std::fs::write(src.join("sub/a.txt"), "a").unwrap();
        let dest = tmp.path().join("dest");
        copy_dir(&src, &dest).unwrap();
        assert!(dest.join("extension.toml").is_file());
        assert!(dest.join("sub/a.txt").is_file());
    }

    #[test]
    fn reload_retrusts_only_when_the_manifest_changes() {
        // Isolate the global store to a temp SMOOTH_HOME (scope_dir + TrustStore
        // both resolve through it). Single test → no env race with siblings.
        let home = tempfile::tempdir().unwrap();
        let ext_dir = home.path().join("extensions").join("demo");
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("extension.toml"),
            "name = \"demo\"\nversion = \"0.1.0\"\n[run]\ncommand = \"node\"\n",
        )
        .unwrap();
        std::env::set_var("SMOOTH_HOME", home.path());

        let h0 = hash_extension(&ext_dir).unwrap();
        let mut store = TrustStore::load();
        store.set("demo", &ext_dir.to_string_lossy(), &h0, true);
        store.save().unwrap();
        reload("demo", false, false).unwrap();
        assert!(TrustStore::load().is_trusted("demo", &h0), "unchanged reload keeps trust");

        std::fs::write(
            ext_dir.join("extension.toml"),
            "name = \"demo\"\nversion = \"0.2.0\"\n[run]\ncommand = \"node\"\n",
        )
        .unwrap();
        let h1 = hash_extension(&ext_dir).unwrap();
        assert_ne!(h0, h1);
        assert!(!TrustStore::load().is_trusted("demo", &h1), "an edited manifest is untrusted until reloaded");
        reload("demo", false, true).unwrap();
        assert!(TrustStore::load().is_trusted("demo", &h1), "reload --trust re-trusts the new hash");

        std::env::remove_var("SMOOTH_HOME");
    }

    #[test]
    fn embedded_index_parses_and_defaults_kind() {
        let index: MarketplaceIndex = serde_json::from_str(INDEX_JSON).expect("embedded index must parse");
        assert!(index.extensions.iter().any(|e| e.name == "create-smooth-extension"));
        // Every entry has a kind (defaulted when absent).
        assert!(index.extensions.iter().all(|e| !e.kind.is_empty()));
    }

    #[test]
    fn entry_matches_name_description_keywords() {
        let e = IndexEntry {
            name: "todo-list".into(),
            description: "Track tasks in a widget".into(),
            source: "npm:@x/todo".into(),
            keywords: vec!["productivity".into()],
            kind: "extension".into(),
        };
        assert!(entry_matches(&e, "todo")); // name
        assert!(entry_matches(&e, "widget")); // description
        assert!(entry_matches(&e, "productivity")); // keyword
        assert!(entry_matches(&e, "")); // empty matches all
        assert!(!entry_matches(&e, "database"));
    }

    #[test]
    fn installed_in_finds_manifested_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("good")).unwrap();
        std::fs::write(tmp.path().join("good/extension.toml"), "name=\"good\"").unwrap();
        std::fs::create_dir_all(tmp.path().join("notext")).unwrap();
        let found = installed_in(tmp.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "good");
    }
}

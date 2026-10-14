//! `th chrome` — read and write Chrome bookmarks (account-synced included) and, when the user
//! enables it in the extension, drive tabs. Everything goes through the Smooth extension's
//! `chrome.bookmarks` / `chrome.tabs`, reached via the native-messaging host (`host.rs`) on a
//! Unix socket. The ops here back both the CLI and the MCP tools (`mcp_chrome.rs`).

pub mod client;
pub mod history;
pub mod host;
pub mod install;
pub mod native;
pub mod tree;

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use clap::Subcommand;
use serde_json::{json, Value};

use client::{Client, LiveApi};
use history::History;
use tree::{guard_parent, guard_writable, permanent, resolve_target, Api, MemTree, Node, Upserted, BAR};

/// `$SMOOTH_CHROME_DIR`, else `$SMOOTH_HOME/chrome`, else `~/.smooth/chrome`.
pub fn chrome_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("SMOOTH_CHROME_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d));
    }
    if let Some(h) = std::env::var_os("SMOOTH_HOME").filter(|h| !h.is_empty()) {
        return Ok(PathBuf::from(h).join("chrome"));
    }
    Ok(dirs_next::home_dir().context("no home dir")?.join(".smooth").join("chrome"))
}

pub fn sock_dir() -> Result<PathBuf> {
    Ok(chrome_dir()?.join("socks"))
}

fn history() -> Result<History> {
    History::open(&chrome_dir()?.join("history"))
}

// ── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum ChromeCommands {
    /// Unpack the extension and register the native-messaging host
    Install {
        /// chrome, chrome-beta, chrome-canary, chromium, brave, edge (default: every one installed)
        #[arg(long)]
        browser: Option<String>,
    },
    /// Remove the native-messaging host registration (history is kept)
    Uninstall {
        #[arg(long)]
        browser: Option<String>,
    },
    /// Connected Chrome instances and whether browsing is enabled; diagnoses setup when none is
    #[command(alias = "doctor")]
    Status,
    /// The native-messaging host Chrome launches (not for direct use)
    #[command(hide = true)]
    Host {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Read and change bookmarks, synced ones included
    #[command(visible_alias = "bm")]
    Bookmarks {
        /// Which connected Chrome (pid from `th chrome status`) when several are
        #[arg(long, global = true)]
        instance: Option<String>,
        #[command(subcommand)]
        cmd: BookmarkCommands,
    },
    /// Drive tabs: needs "Enable browsing" in the extension popup
    Browse {
        #[arg(long, global = true)]
        instance: Option<String>,
        #[command(subcommand)]
        cmd: BrowseCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum BookmarkCommands {
    /// Print the tree (or the subtree at PATH), labelled synced/local
    Tree {
        path: Option<String>,
        #[arg(long)]
        depth: Option<usize>,
        #[arg(long)]
        json: bool,
        /// Resolve PATH in the local (not synced) folders
        #[arg(long)]
        local: bool,
    },
    /// Search titles and URLs
    Search {
        query: String,
        #[arg(long)]
        json: bool,
    },
    /// Add a bookmark (default parent: the synced bookmarks bar)
    Add {
        title: String,
        url: String,
        /// Folder path, e.g. "Bookmarks bar/Work"
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        index: Option<usize>,
        #[arg(long)]
        local: bool,
    },
    /// Create a folder path (idempotent)
    Mkdir {
        path: String,
        #[arg(long)]
        local: bool,
    },
    /// Add a bookmark to a folder path unless that URL is already there (idempotent)
    Upsert {
        #[arg(long)]
        path: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        url: String,
        #[arg(long)]
        local: bool,
    },
    /// Move a bookmark or folder (id or path) into a folder
    Move {
        target: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        index: Option<usize>,
        #[arg(long)]
        local: bool,
        /// Required to move a folder
        #[arg(long)]
        yes: bool,
    },
    /// Change a bookmark's title and/or URL
    Rename {
        target: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        local: bool,
    },
    /// Remove a bookmark or folder (dry run without --yes)
    Rm {
        target: String,
        #[arg(long)]
        local: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Dump everything as JSON (default) or Netscape HTML
    Export {
        #[arg(long)]
        html: bool,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Git-backed bookmark history
    History {
        #[command(subcommand)]
        cmd: HistoryCommands,
    },
    /// Make bookmarks match a history snapshot (dry run without --yes)
    Restore {
        /// A sha from `history log`, HEAD~1, origin/<machine>, …
        rev: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum HistoryCommands {
    /// Recent snapshots
    Log {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Snapshot now
    Snapshot {
        #[arg(short, long, default_value = "manual snapshot")]
        message: String,
    },
    /// Set the git remote history pushes to
    Remote {
        url: String,
        /// Push after every snapshot
        #[arg(long)]
        auto_push: bool,
    },
    /// Push to origin/<this machine>
    Push,
    /// Fetch every machine's history from origin
    Fetch,
    /// Where the history repo lives
    Path,
}

#[derive(Subcommand, Debug)]
pub enum BrowseCommands {
    /// List open tabs
    Tabs,
    /// Open a URL in a new tab and wait for it to load
    Open { url: String },
    /// Point a tab at a URL
    Navigate { tab: i64, url: String },
    /// Go back in a tab's history
    Back { tab: i64 },
    /// Go forward in a tab's history
    Forward { tab: i64 },
    /// Reload a tab
    Reload { tab: i64 },
    /// A tab's visible text, or only the lines matching --find
    Text {
        tab: i64,
        #[arg(long)]
        find: Option<String>,
    },
    /// The page's interactive elements with refs, in context (headings, forms, dialogs)
    Snapshot { tab: i64 },
    /// Click an element from `snapshot`
    Click {
        tab: i64,
        r#ref: String,
        /// Print a fresh snapshot afterwards
        #[arg(long)]
        snapshot: bool,
    },
    /// Type into an element from `snapshot` (replaces its text)
    Type {
        tab: i64,
        r#ref: String,
        text: String,
        #[arg(long)]
        submit: bool,
        #[arg(long)]
        snapshot: bool,
    },
    /// Pick a <select> option by its text or value
    Select {
        tab: i64,
        r#ref: String,
        option: String,
        #[arg(long)]
        snapshot: bool,
    },
    /// Press a key (Enter, Escape, Tab, ArrowDown, Control+a, …) on an element or the focused one
    Key {
        tab: i64,
        key: String,
        #[arg(long)]
        r#ref: Option<String>,
        #[arg(long)]
        snapshot: bool,
    },
    /// Hover an element (opens hover menus)
    Hover {
        tab: i64,
        r#ref: String,
        #[arg(long)]
        snapshot: bool,
    },
    /// Scroll the page (or the --ref container) by pages, to --text, or --ref into view
    Scroll {
        tab: i64,
        #[arg(long, value_parser = ["up", "down"])]
        direction: Option<String>,
        #[arg(long)]
        pages: Option<f64>,
        #[arg(long)]
        r#ref: Option<String>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        snapshot: bool,
    },
    /// Fill several fields at once: ref=value … (checkboxes take true/false, selects an option)
    Fill {
        tab: i64,
        #[arg(required = true)]
        fields: Vec<String>,
        #[arg(long)]
        snapshot: bool,
    },
    /// Wait until any of the texts appears on the page
    Wait {
        tab: i64,
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(long, default_value_t = 10000)]
        timeout_ms: u64,
    },
    /// Save a PNG of the tab
    Screenshot {
        tab: i64,
        #[arg(short, long, default_value = "screenshot.png")]
        output: PathBuf,
    },
    /// Close a tab
    Close { tab: i64 },
}

/// Dispatch `th chrome …`.
///
/// # Errors
/// Whatever the op reports.
pub fn dispatch(cmd: ChromeCommands) -> Result<()> {
    let out = match cmd {
        ChromeCommands::Install { browser } => install_text(&browsers(browser.as_deref())?)?,
        ChromeCommands::Uninstall { browser } => {
            let gone = install::uninstall(&browsers(browser.as_deref())?)?;
            if gone.is_empty() {
                "Nothing to remove".into()
            } else {
                gone.iter().map(|p| format!("Removed {}", p.display())).collect::<Vec<_>>().join("\n")
            }
        }
        ChromeCommands::Status => status()?,
        ChromeCommands::Host { .. } => return host::run(),
        ChromeCommands::Bookmarks { instance, cmd } => bookmarks_cli(instance.as_deref(), cmd)?,
        ChromeCommands::Browse { instance, cmd } => browse_cli(instance.as_deref(), cmd)?,
    };
    println!("{}", out.trim_end());
    Ok(())
}

fn bookmarks_cli(inst: Option<&str>, cmd: BookmarkCommands) -> Result<String> {
    match cmd {
        BookmarkCommands::Tree { path, depth, json, local } => tree_view(inst, path.as_deref(), depth, json, local),
        BookmarkCommands::Search { query, json } => search(inst, &query, json),
        BookmarkCommands::Add {
            title,
            url,
            parent,
            index,
            local,
        } => add(inst, parent.as_deref(), &title, &url, index, local),
        BookmarkCommands::Mkdir { path, local } => mkdir(inst, &path, local),
        BookmarkCommands::Upsert { path, title, url, local } => upsert(inst, &path, &title, &url, local),
        BookmarkCommands::Move { target, to, index, local, yes } => move_node(inst, &target, &to, index, local, yes),
        BookmarkCommands::Rename { target, title, url, local } => rename(inst, &target, title.as_deref(), url.as_deref(), local),
        BookmarkCommands::Rm { target, local, yes } => remove(inst, &target, local, yes),
        BookmarkCommands::Export { html, output } => {
            let body = export(inst, html)?;
            match output {
                Some(p) => {
                    std::fs::write(&p, body)?;
                    Ok(format!("Wrote {}", p.display()))
                }
                None => Ok(body),
            }
        }
        BookmarkCommands::History { cmd } => history_cli(inst, cmd),
        BookmarkCommands::Restore { rev, yes } => restore(inst, &rev, yes),
    }
}

fn history_cli(inst: Option<&str>, cmd: HistoryCommands) -> Result<String> {
    let h = history()?;
    match cmd {
        HistoryCommands::Log { limit } => Ok(h.log(limit).map(|l| if l.is_empty() { "No snapshots yet.".into() } else { l })?),
        HistoryCommands::Snapshot { message } => snapshot_now(inst, &message),
        HistoryCommands::Remote { url, auto_push } => {
            h.set_remote(&url, auto_push)?;
            Ok(format!(
                "History remote set to {url}{}; this machine pushes to branch '{}'",
                if auto_push { " with auto-push" } else { "" },
                History::branch()
            ))
        }
        HistoryCommands::Push => Ok(format!("Pushed to origin/{}", h.push()?)),
        HistoryCommands::Fetch => Ok(format!("Fetched. Restore from any of:\n{}", h.fetch()?)),
        HistoryCommands::Path => Ok(chrome_dir()?.join("history").display().to_string()),
    }
}

fn browse_cli(inst: Option<&str>, cmd: BrowseCommands) -> Result<String> {
    let pretty = |v: Value| serde_json::to_string_pretty(&v).unwrap_or_default();
    match cmd {
        BrowseCommands::Tabs => tabs(inst),
        BrowseCommands::Open { url } => browse(inst, "tabs.open", json!({"url": url})).map(pretty),
        BrowseCommands::Navigate { tab, url } => browse(inst, "tabs.navigate", json!({"tabId": tab, "url": url})).map(pretty),
        BrowseCommands::Back { tab } => nav(inst, tab, "back"),
        BrowseCommands::Forward { tab } => nav(inst, tab, "forward"),
        BrowseCommands::Reload { tab } => nav(inst, tab, "reload"),
        BrowseCommands::Text { tab, find } => page_text(inst, tab, find.as_deref()),
        BrowseCommands::Snapshot { tab } => page_snapshot(inst, tab),
        BrowseCommands::Click { tab, r#ref: el, snapshot } => act(inst, tab, "click", json!({"ref": el}), snapshot),
        BrowseCommands::Type {
            tab,
            r#ref: el,
            text,
            submit,
            snapshot,
        } => act(inst, tab, "type", json!({"ref": el, "text": text, "submit": submit}), snapshot),
        BrowseCommands::Select {
            tab,
            r#ref: el,
            option,
            snapshot,
        } => act(inst, tab, "select", json!({"ref": el, "option": option}), snapshot),
        BrowseCommands::Key { tab, key, r#ref: el, snapshot } => act(inst, tab, "key", json!({"ref": el, "key": key}), snapshot),
        BrowseCommands::Hover { tab, r#ref: el, snapshot } => act(inst, tab, "hover", json!({"ref": el}), snapshot),
        BrowseCommands::Scroll {
            tab,
            direction,
            pages,
            r#ref: el,
            text,
            snapshot,
        } => act(
            inst,
            tab,
            "scroll",
            json!({"direction": direction, "pages": pages, "ref": el, "text": text}),
            snapshot,
        ),
        BrowseCommands::Fill { tab, fields, snapshot } => {
            let fields = fields
                .iter()
                .map(|f| {
                    f.split_once('=')
                        .map(|(r, v)| json!({"ref": r, "value": v}))
                        .with_context(|| format!("'{f}' isn't ref=value"))
                })
                .collect::<Result<Vec<_>>>()?;
            act(inst, tab, "fill", json!({ "fields": fields }), snapshot)
        }
        BrowseCommands::Wait { tab, text, timeout_ms } => wait_for(inst, tab, &text, timeout_ms),
        BrowseCommands::Screenshot { tab, output } => {
            std::fs::write(&output, screenshot(inst, tab)?)?;
            Ok(format!("Wrote {}", output.display()))
        }
        BrowseCommands::Close { tab } => browse(inst, "tabs.close", json!({"tabId": tab})).map(|_| format!("Closed tab {tab}")),
    }
}

// ── ops shared with MCP ─────────────────────────────────────────────────────

fn api(inst: Option<&str>) -> Result<LiveApi> {
    Ok(LiveApi(Client::connect(inst)?))
}

fn browsers(one: Option<&str>) -> Result<Vec<&str>> {
    one.map_or_else(|| Ok(install::detected()?.into_iter().collect()), |b| Ok(vec![b]))
}

fn install_text(browsers: &[&str]) -> Result<String> {
    let i = install::install(browsers)?;
    Ok(format!(
        "Registered native host {} for {}\n  (launches {} chrome host)\nUnpacked the extension to {}\n\n\
         Load it once per Chrome profile:\n  1. Open chrome://extensions in the profile whose bookmarks you want\n  \
         2. Turn on Developer mode (top right)\n  3. Load unpacked → {}\n  4. Check its id is {}\n\n\
         Then: th chrome status · th chrome bookmarks tree --depth 1\n\
         After upgrading th, run `th chrome install` again and press reload on the extension.",
        install::HOST_NAME,
        i.manifests.iter().map(|m| m.display().to_string()).collect::<Vec<_>>().join(", "),
        i.th.display(),
        i.extension_dir.display(),
        i.extension_dir.display(),
        install::EXTENSION_ID
    ))
}

fn status() -> Result<String> {
    let socks = client::instances(&sock_dir()?);
    if socks.is_empty() {
        let mut out = String::from("No Chrome instance connected. Setup checks:\n");
        for (ok, line) in install::diagnose()? {
            let _ = writeln!(out, "  {} {line}", if ok { "✓" } else { "✗" });
        }
        out.push_str("If all pass: open Chrome, check the extension is enabled, then click its icon to see the last connection error.");
        return Ok(out);
    }
    let mut out = String::new();
    for s in socks {
        let pid = s.file_stem().and_then(|p| p.to_str()).unwrap_or_default().to_string();
        let mut c = Client::at(&s)?;
        let info = c.call("host.info", Value::Null)?;
        let ext = &info["extension"];
        let bar = LiveApi(c).tree().ok().and_then(|t| {
            let b = permanent(&t, BAR, false).or_else(|_| permanent(&t, BAR, true)).ok()?;
            Some(
                b.children
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .take(4)
                    .map(|n| n.title.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        });
        let _ = writeln!(
            out,
            "instance {pid}: extension {} · browsing {} · bar: {}",
            ext["version"].as_str().unwrap_or("?"),
            if ext["browsing"].as_bool() == Some(true) { "on" } else { "off" },
            bar.unwrap_or_else(|| "?".into())
        );
    }
    Ok(out)
}

/// Where a node lives, e.g. "Bookmarks bar (synced)/Work/Jira".
fn path_of(root: &Node, id: &str) -> String {
    let Some(chain) = root.ancestry(id) else { return id.to_string() };
    chain
        .iter()
        .skip(1)
        .map(|n| match &n.folder_type {
            Some(_) => format!("{} ({})", n.title, if n.syncing { "synced" } else { "local" }),
            None => n.title.clone(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn where_note(syncing: bool) -> &'static str {
    if syncing {
        "synced to your Google account"
    } else {
        "LOCAL ONLY, not synced"
    }
}

/// Commit the live tree to history. Before destructive ops this must succeed.
fn record(api: &mut dyn Api, message: &str) -> Result<Option<String>> {
    history()?.snapshot(&api.tree()?, message)
}

fn record_after(api: &mut dyn Api, message: &str) -> String {
    match record(api, message) {
        Ok(Some(sha)) => format!("\nhistory: {sha}"),
        Ok(None) => String::new(),
        Err(e) => format!("\n(warning: history snapshot failed: {e})"),
    }
}

pub fn tree_view(inst: Option<&str>, path: Option<&str>, depth: Option<usize>, as_json: bool, local: bool) -> Result<String> {
    let root = api(inst)?.tree()?;
    let node = match path {
        Some(p) => resolve_target(&root, p, local)?,
        None => &root,
    };
    if as_json {
        return Ok(serde_json::to_string_pretty(node)?);
    }
    Ok(tree::render(node, depth))
}

pub fn search(inst: Option<&str>, query: &str, as_json: bool) -> Result<String> {
    let mut c = Client::connect(inst)?;
    let hits: Vec<Node> = serde_json::from_value(c.call("bookmarks.search", json!({"query": query}))?)?;
    let root = LiveApi(c).tree()?;
    if as_json {
        let rows: Vec<Value> = hits
            .iter()
            .map(|n| json!({"id": n.id, "title": n.title, "url": n.url, "syncing": n.syncing, "path": path_of(&root, &n.id)}))
            .collect();
        return Ok(serde_json::to_string_pretty(&rows)?);
    }
    if hits.is_empty() {
        return Ok(format!("No bookmarks match '{query}'."));
    }
    let mut out = String::new();
    for n in &hits {
        let _ = writeln!(
            out,
            "#{}  {}{}",
            n.id,
            path_of(&root, &n.id),
            n.url.as_deref().map(|u| format!("  <{u}>")).unwrap_or_default()
        );
    }
    Ok(out)
}

pub fn add(inst: Option<&str>, parent: Option<&str>, title: &str, url: &str, index: Option<usize>, local: bool) -> Result<String> {
    let mut a = api(inst)?;
    let root = a.tree()?;
    let p = match parent {
        Some(p) => resolve_target(&root, p, local)?,
        None => permanent(&root, BAR, local)?,
    };
    guard_parent(&root, &p.id)?;
    let syncing = p.syncing;
    let n = a.create(&p.id.clone(), title, Some(url), index)?;
    let root = a.tree()?;
    Ok(format!("Added #{} {} ({})", n.id, path_of(&root, &n.id), where_note(syncing)) + &record_after(&mut a, &format!("add {title}")))
}

pub fn mkdir(inst: Option<&str>, path: &str, local: bool) -> Result<String> {
    let mut a = api(inst)?;
    let e = tree::ensure_folder(&mut a, path, local)?;
    let root = a.tree()?;
    let what = if e.created.is_empty() { "Already there" } else { "Created" };
    let mut out = format!("{what}: #{} {} ({})", e.id, path_of(&root, &e.id), where_note(e.syncing));
    if !e.created.is_empty() {
        out += &record_after(&mut a, &format!("mkdir {path}"));
    }
    Ok(out)
}

pub fn upsert(inst: Option<&str>, path: &str, title: &str, url: &str, local: bool) -> Result<String> {
    let mut a = api(inst)?;
    let (r, folder) = tree::upsert(&mut a, path, title, url, local)?;
    let root = a.tree()?;
    let note = where_note(folder.syncing);
    Ok(match r {
        Upserted::Unchanged(id) => format!("Unchanged: #{id} {} ({note})", path_of(&root, &id)),
        Upserted::Retitled(id) => format!("Retitled: #{id} {} ({note})", path_of(&root, &id)) + &record_after(&mut a, &format!("upsert {title}")),
        Upserted::Created(id) => format!("Created: #{id} {} ({note})", path_of(&root, &id)) + &record_after(&mut a, &format!("upsert {title}")),
    })
}

fn describe(n: &Node) -> String {
    if n.is_folder() {
        let mut count = 0;
        let mut stack: Vec<&Node> = n.children.as_deref().unwrap_or_default().iter().collect();
        while let Some(c) = stack.pop() {
            count += 1;
            stack.extend(c.children.as_deref().unwrap_or_default());
        }
        format!("folder '{}' ({count} item{} inside)", n.title, if count == 1 { "" } else { "s" })
    } else {
        format!("bookmark '{}' <{}>", n.title, n.url.as_deref().unwrap_or_default())
    }
}

pub fn move_node(inst: Option<&str>, target: &str, to: &str, index: Option<usize>, local: bool, confirm: bool) -> Result<String> {
    let mut a = api(inst)?;
    let root = a.tree()?;
    let n = resolve_target(&root, target, local)?;
    let dest = resolve_target(&root, to, local)?;
    guard_writable(&root, &n.id, "move")?;
    guard_parent(&root, &dest.id)?;
    if root.ancestry(&dest.id).is_some_and(|c| c.iter().any(|x| x.id == n.id)) {
        bail!("can't move a folder into itself");
    }
    let (id, folder_id, summary, dest_path) = (n.id.clone(), dest.id.clone(), describe(n), path_of(&root, &dest.id));
    if n.is_folder() {
        if !confirm {
            return Ok(format!(
                "Dry run: would move {summary} into {dest_path}. Re-run with --yes (MCP: confirm=true)."
            ));
        }
        record(&mut a, &format!("before move {}", n.title))?;
    }
    a.move_to(&id, &folder_id, index)?;
    let note = where_note(dest.syncing);
    Ok(format!("Moved {summary} into {dest_path} ({note})") + &record_after(&mut a, &format!("move #{id}")))
}

pub fn rename(inst: Option<&str>, target: &str, title: Option<&str>, url: Option<&str>, local: bool) -> Result<String> {
    if title.is_none() && url.is_none() {
        bail!("give --title and/or --url");
    }
    let mut a = api(inst)?;
    let root = a.tree()?;
    let n = resolve_target(&root, target, local)?;
    guard_writable(&root, &n.id, "rename")?;
    if url.is_some() && n.is_folder() {
        bail!("'{}' is a folder; folders have no URL", n.title);
    }
    let id = n.id.clone();
    a.update(&id, title, url)?;
    let root = a.tree()?;
    Ok(format!("Updated #{id} {}", path_of(&root, &id)) + &record_after(&mut a, &format!("rename #{id}")))
}

pub fn remove(inst: Option<&str>, target: &str, local: bool, confirm: bool) -> Result<String> {
    let mut a = api(inst)?;
    let root = a.tree()?;
    let n = resolve_target(&root, target, local)?;
    guard_writable(&root, &n.id, "remove")?;
    let (desc, path, id, folder) = (describe(n), path_of(&root, &n.id), n.id.clone(), n.is_folder());
    if !confirm {
        return Ok(format!("Dry run: would remove {desc} at {path}. Re-run with --yes (MCP: confirm=true)."));
    }
    let before = record(&mut a, &format!("before remove {path}"))?;
    if folder {
        a.remove_tree(&id)?;
    } else {
        a.remove(&id)?;
    }
    let undo = before.map(|_| " Undo: th chrome bookmarks restore HEAD~1 --yes").unwrap_or_default();
    Ok(format!("Removed {desc} from {path}.{undo}") + &record_after(&mut a, &format!("remove {path}")))
}

pub fn export(inst: Option<&str>, html: bool) -> Result<String> {
    let root = api(inst)?.tree()?;
    Ok(if html {
        tree::to_netscape_html(&root)
    } else {
        serde_json::to_string_pretty(&root)?
    })
}

pub fn snapshot_now(inst: Option<&str>, message: &str) -> Result<String> {
    let mut a = api(inst)?;
    Ok(record(&mut a, message)?.map_or_else(|| "No changes since the last snapshot.".into(), |sha| format!("Snapshot {sha}")))
}

pub fn history_log(limit: usize) -> Result<String> {
    let l = history()?.log(limit)?;
    Ok(if l.is_empty() { "No snapshots yet.".into() } else { l })
}

pub fn restore(inst: Option<&str>, rev: &str, confirm: bool) -> Result<String> {
    let snap = history()?.at(rev)?;
    let mut a = api(inst)?;
    let mut dry = MemTree::new(a.tree()?);
    let plan = tree::restore(&mut dry, &snap)?;
    if plan.is_empty() {
        return Ok(format!("Bookmarks already match {rev}."));
    }
    let list = plan.iter().map(|o| format!("  {o}")).collect::<Vec<_>>().join("\n");
    if !confirm {
        return Ok(format!("Dry run: restoring {rev} would:\n{list}\nRe-run with --yes (MCP: confirm=true)."));
    }
    record(&mut a, &format!("before restore {rev}"))?;
    let done = tree::restore(&mut a, &snap)?;
    Ok(format!("Restored {rev} ({} change{}).", done.len(), if done.len() == 1 { "" } else { "s" }) + &record_after(&mut a, &format!("restore {rev}")))
}

pub fn browse(inst: Option<&str>, method: &str, params: Value) -> Result<Value> {
    Client::connect(inst)?.call(method, params)
}

pub fn tabs(inst: Option<&str>) -> Result<String> {
    let v = browse(inst, "tabs.list", Value::Null)?;
    let mut out = String::new();
    for t in v.as_array().map(Vec::as_slice).unwrap_or_default() {
        let _ = writeln!(
            out,
            "{}{}  {}  <{}>",
            t["tabId"],
            if t["active"] == true { "*" } else { "" },
            t["title"].as_str().unwrap_or_default(),
            t["url"].as_str().unwrap_or_default()
        );
    }
    Ok(out)
}

pub fn page_text(inst: Option<&str>, tab: i64, find: Option<&str>) -> Result<String> {
    let v = browse(inst, "page.text", json!({"tabId": tab}))?;
    let text = v["text"].as_str().unwrap_or_default();
    let body = find.map_or_else(|| text.to_string(), |needle| find_lines(text, needle));
    Ok(format!(
        "{}\n<{}>\n\n{body}",
        v["title"].as_str().unwrap_or_default(),
        v["url"].as_str().unwrap_or_default()
    ))
}

/// Lines containing `needle` (case-insensitive) with one line of context each side.
pub fn find_lines(text: &str, needle: &str) -> String {
    const MAX: usize = 50;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let n = needle.to_lowercase();
    let hits: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].to_lowercase().contains(&n)).collect();
    if hits.is_empty() {
        return format!("No line contains \"{needle}\".");
    }
    let mut out = String::new();
    let mut last = None;
    for &i in hits.iter().take(MAX) {
        let from = i.saturating_sub(1).max(last.map_or(0, |l: usize| l + 1));
        if last.is_some_and(|l| from > l + 1) {
            out.push_str("…\n");
        }
        for (j, line) in lines.iter().enumerate().take((i + 2).min(lines.len())).skip(from) {
            let _ = writeln!(out, "{}{}", if j == i { "> " } else { "  " }, line.trim());
            last = Some(j);
        }
    }
    if hits.len() > MAX {
        let _ = writeln!(out, "… {} more matches", hits.len() - MAX);
    }
    out
}

pub fn page_snapshot(inst: Option<&str>, tab: i64) -> Result<String> {
    Ok(format_snapshot(&browse(inst, "page.snapshot", json!({"tabId": tab}))?))
}

fn element_line(out: &mut String, e: &Value, extra_depth: u64) {
    let indent = "  ".repeat(usize::try_from(e["depth"].as_u64().unwrap_or(0) + extra_depth).unwrap_or(0));
    let kind = e["kind"].as_str().unwrap_or("?");
    let name = e["name"].as_str().unwrap_or_default();
    let Some(r) = e["ref"].as_str() else {
        let _ = writeln!(out, "{indent}{kind}{}", if name.is_empty() { String::new() } else { format!(" \"{name}\"") });
        return;
    };
    let mut line = format!("{indent}[{r}] {kind} \"{name}\"");
    if let Some(v) = e["value"].as_str() {
        let _ = write!(line, " value=\"{v}\"");
    }
    if let Some(opts) = e["options"].as_array() {
        let opts: Vec<&str> = opts.iter().filter_map(Value::as_str).collect();
        let _ = write!(line, " options=[{}]", opts.join(" | "));
    }
    for flag in ["checked", "selected", "disabled", "required"] {
        if e[flag] == true {
            let _ = write!(line, " {flag}");
        }
    }
    match e["expanded"].as_bool() {
        Some(true) => line.push_str(" expanded"),
        Some(false) => line.push_str(" collapsed"),
        None => {}
    }
    if let Some(p) = e["placeholder"].as_str() {
        let _ = write!(line, " placeholder=\"{p}\"");
    }
    if let Some(h) = e["href"].as_str() {
        let _ = write!(line, " → {h}");
    }
    if e["covered"] == true {
        line.push_str(" (covered by something; close it first)");
    } else if e["offscreen"] == true {
        line.push_str(" (offscreen)");
    }
    let _ = writeln!(out, "{line}");
}

/// The model-facing snapshot: where the viewport is, then an indented outline of
/// interactive elements (with refs) inside their headings, forms, dialogs and rows.
pub fn format_snapshot(v: &Value) -> String {
    let mut out = format!("{}\n<{}>\n", v["title"].as_str().unwrap_or_default(), v["url"].as_str().unwrap_or_default());
    #[allow(clippy::cast_precision_loss, reason = "page pixel counts")]
    if let (Some(y), Some(vh), Some(h)) = (v["scrollY"].as_u64(), v["viewport"].as_u64(), v["height"].as_u64()) {
        if vh > 0 {
            let above = y as f64 / vh as f64;
            let below = (h.saturating_sub(y + vh)) as f64 / vh as f64;
            if above < 0.05 && below < 0.05 {
                out.push_str("[whole page visible]\n");
            } else {
                let _ = writeln!(out, "[{above:.1} pages above, {below:.1} pages below; browse_scroll to see more]");
            }
        }
    }
    out.push('\n');
    let elements = |v: &Value| v["elements"].as_array().cloned().unwrap_or_default();
    for e in elements(v) {
        element_line(&mut out, &e, 0);
    }
    if v["truncated"] == true {
        out.push_str("… stopped here; scroll, or use browse_text with find, for the rest\n");
    }
    for f in v["frames"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let _ = writeln!(out, "iframe <{}>", f["url"].as_str().unwrap_or_default());
        for e in elements(f) {
            element_line(&mut out, &e, 1);
        }
        if f["truncated"] == true {
            out.push_str("  … stopped here\n");
        }
    }
    out
}

/// Run an in-page action (it waits for navigation or the DOM to settle) and say what changed;
/// with `then_snapshot`, append a fresh snapshot so the caller needn't ask for one.
pub fn act(inst: Option<&str>, tab: i64, op: &str, args: Value, then_snapshot: bool) -> Result<String> {
    let mut c = Client::connect(inst)?;
    let mut req = json!({"tabId": tab, "op": op});
    req["args"] = args;
    let v = c.call("page.act", req)?;
    let mut out = describe_act(&v);
    if then_snapshot {
        out.push_str("\n\n");
        out.push_str(&format_snapshot(&c.call("page.snapshot", json!({"tabId": tab}))?));
    }
    Ok(out)
}

fn describe_act(v: &Value) -> String {
    let mut out = match &v["result"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"),
        _ => "done".into(),
    };
    if v["navigated"] == true {
        let _ = write!(
            out,
            "\nnow at {} <{}>",
            v["title"].as_str().unwrap_or_default(),
            v["url"].as_str().unwrap_or_default()
        );
    }
    if let Some(ids) = v["opened"].as_array().filter(|a| !a.is_empty()) {
        let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let _ = write!(out, "\nopened new tab {} (see browse_tabs)", ids.join(", "));
    }
    out
}

pub fn nav(inst: Option<&str>, tab: i64, action: &str) -> Result<String> {
    let t = browse(inst, "tabs.history", json!({"tabId": tab, "action": action}))?;
    Ok(format!(
        "{} <{}>",
        t["title"].as_str().unwrap_or_default(),
        t["url"].as_str().unwrap_or_default()
    ))
}

pub fn wait_for(inst: Option<&str>, tab: i64, texts: &[String], timeout_ms: u64) -> Result<String> {
    let hit = browse(inst, "page.waitFor", json!({"tabId": tab, "texts": texts, "timeoutMs": timeout_ms}))?;
    Ok(format!("\"{}\" is on the page", hit.as_str().unwrap_or_default()))
}

pub fn screenshot(inst: Option<&str>, tab: i64) -> Result<Vec<u8>> {
    let v = browse(inst, "page.screenshot", json!({"tabId": tab}))?;
    let data = v["dataUrl"]
        .as_str()
        .and_then(|d| d.split_once(','))
        .map(|(_, b)| b)
        .context("no image from Chrome")?;
    Ok(base64::engine::general_purpose::STANDARD.decode(data)?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;
    use tree::tests::sample;

    #[test]
    fn paths_label_synced_and_local_roots() {
        let t = sample();
        assert_eq!(path_of(&t.root, "31"), "Bookmarks bar (synced)/Work/Jira");
        assert_eq!(path_of(&t.root, "10"), "Bookmarks bar (local)/Local thing");
        assert_eq!(path_of(&t.root, "nope"), "nope");
    }

    #[test]
    fn describes_folders_with_counts() {
        let t = sample();
        assert_eq!(describe(t.root.find("30").unwrap()), "folder 'Work' (1 item inside)");
        assert_eq!(describe(t.root.find("31").unwrap()), "bookmark 'Jira' <https://jira.example>");
    }

    #[test]
    fn snapshot_is_an_indented_outline_with_state_and_frames() {
        let v = json!({
            "title": "Sign in", "url": "https://x.example/login",
            "scrollY": 800, "viewport": 800, "height": 4000, "truncated": true,
            "elements": [
                {"depth": 0, "kind": "heading", "name": "Welcome"},
                {"depth": 0, "kind": "form", "name": ""},
                {"depth": 1, "ref": "e3", "kind": "email", "name": "Email", "value": "a@b.c", "required": true},
                {"depth": 1, "ref": "e4", "kind": "password", "name": "Password", "value": "••••"},
                {"depth": 1, "ref": "e5", "kind": "select", "name": "Region", "value": "EU", "options": ["US", "EU"]},
                {"depth": 1, "ref": "e6", "kind": "checkbox", "name": "Remember me", "checked": true},
                {"depth": 1, "ref": "e7", "kind": "button", "name": "Sign in", "disabled": true, "covered": true},
                {"depth": 0, "ref": "e8", "kind": "link", "name": "Help", "href": "https://x.example/help", "offscreen": true},
                {"depth": 0, "ref": "e9", "kind": "button", "name": "Menu", "expanded": false}
            ],
            "frames": [{"frameId": 7, "url": "https://pay.example", "elements": [{"depth": 0, "ref": "f7:e1", "kind": "text", "name": "Card"}]}]
        });
        let s = format_snapshot(&v);
        let want = "Sign in\n<https://x.example/login>\n[1.0 pages above, 3.0 pages below; browse_scroll to see more]\n\n\
heading \"Welcome\"\nform\n  [e3] email \"Email\" value=\"a@b.c\" required\n  [e4] password \"Password\" value=\"••••\"\n  \
[e5] select \"Region\" value=\"EU\" options=[US | EU]\n  [e6] checkbox \"Remember me\" checked\n  \
[e7] button \"Sign in\" disabled (covered by something; close it first)\n[e8] link \"Help\" → https://x.example/help (offscreen)\n\
[e9] button \"Menu\" collapsed\n… stopped here; scroll, or use browse_text with find, for the rest\niframe <https://pay.example>\n  [f7:e1] text \"Card\"\n";
        assert_eq!(s, want);
        let whole = format_snapshot(&json!({"title": "t", "url": "u", "scrollY": 0, "viewport": 800, "height": 800, "elements": []}));
        assert!(whole.contains("[whole page visible]"), "{whole}");
    }

    #[test]
    fn find_lines_shows_matches_in_context() {
        let text = "Intro\nOrders\n\nOrder 1 Pending\nOrder 2 Shipped\nFooter\nmore\nlast pending item";
        assert_eq!(
            find_lines(text, "pending"),
            "  Orders\n> Order 1 Pending\n  Order 2 Shipped\n…\n  more\n> last pending item\n"
        );
        assert_eq!(find_lines(text, "zzz"), "No line contains \"zzz\".");
        let many = "hit\n".repeat(60);
        assert!(find_lines(&many, "hit").ends_with("… 10 more matches\n"));
    }

    #[test]
    fn describes_what_an_action_changed() {
        assert_eq!(describe_act(&json!({"result": "clicked", "navigated": false, "opened": []})), "clicked");
        assert_eq!(
            describe_act(&json!({"result": ["e1: typed", "e2: checked"], "navigated": true, "url": "https://n", "title": "Next", "opened": [41]})),
            "e1: typed\ne2: checked\nnow at Next <https://n>\nopened new tab 41 (see browse_tabs)"
        );
        assert_eq!(describe_act(&json!({"result": null})), "done");
    }

    #[test]
    fn chrome_dir_honours_overrides() {
        std::env::set_var("SMOOTH_CHROME_DIR", "/tmp/x-chrome");
        assert_eq!(chrome_dir().unwrap(), PathBuf::from("/tmp/x-chrome"));
        assert_eq!(sock_dir().unwrap(), PathBuf::from("/tmp/x-chrome/socks"));
        std::env::remove_var("SMOOTH_CHROME_DIR");
        assert!(chrome_dir().unwrap().ends_with("chrome"));
    }
}
